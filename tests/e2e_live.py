"""Live E2E for the release HappyPath WebView.

Run with a Python environment that already contains Playwright. The app is
launched twice with HAPPYPATH_DATA_DIR pointing inside target/e2e-data; the
user's real %APPDATA%/HappyPath directory is never touched.
"""
from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import time
import urllib.request
import ctypes
from ctypes import wintypes
from pathlib import Path

from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "src-tauri" / "target" / "release" / "happypath.exe"
TEST_ROOT = ROOT / "src-tauri" / "target" / "e2e-data"
DATA_DIR = TEST_ROOT / "stats"


def check(condition: bool, name: str) -> None:
    if not condition:
        raise AssertionError(name)
    print(f"PASS {name}", flush=True)


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def window_rect(hwnd: int) -> tuple[int, int, int, int]:
    user32 = ctypes.windll.user32
    user32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
    rect = wintypes.RECT()
    if not user32.GetWindowRect(hwnd, ctypes.byref(rect)):
        raise RuntimeError(f"GetWindowRect failed for HWND {hwnd}")
    return rect.left, rect.top, rect.right, rect.bottom


def find_top_window(title: str) -> int:
    user32 = ctypes.windll.user32
    user32.IsWindowVisible.argtypes = [wintypes.HWND]
    user32.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
    matches: list[int] = []
    callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

    def visit(hwnd, _):
        if user32.IsWindowVisible(hwnd):
            buffer = ctypes.create_unicode_buffer(512)
            user32.GetWindowTextW(hwnd, buffer, len(buffer))
            if buffer.value == title:
                matches.append(hwnd)
        return True

    user32.EnumWindows(callback_type(visit), 0)
    check(len(matches) == 1, f"exactly one visible {title} window")
    return matches[0]


def check_taskbar_geometry() -> None:
    user32 = ctypes.windll.user32
    user32.SetProcessDPIAware()
    user32.FindWindowW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR]
    user32.FindWindowW.restype = wintypes.HWND
    widget = window_rect(find_top_window("HappyPath"))
    tray_hwnd = user32.FindWindowW("Shell_TrayWnd", None)
    check(bool(tray_hwnd), "Windows taskbar exists")
    tray = window_rect(tray_hwnd)
    check(widget[2] > widget[0] and widget[3] > widget[1], "widget has non-zero physical size")
    check(
        widget[1] >= tray[1] - 2 and widget[3] <= tray[3] + 2,
        f"widget rect {widget} lies inside taskbar band {tray}",
    )


def stop(process: subprocess.Popen | None) -> None:
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=8)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def launch(playwright, profile_name: str):
    port = free_port()
    env = os.environ.copy()
    env["HAPPYPATH_DATA_DIR"] = str(DATA_DIR)
    env["HAPPYPATH_DURATION_SECONDS"] = "3"
    env["WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS"] = f"--remote-debugging-port={port}"
    env["WEBVIEW2_USER_DATA_FOLDER"] = str(TEST_ROOT / profile_name)
    log_path = TEST_ROOT / f"{profile_name}.log"
    log = log_path.open("w", encoding="utf-8")
    process = subprocess.Popen(
        [str(EXE)],
        cwd=str(ROOT),
        env=env,
        stdout=log,
        stderr=subprocess.STDOUT,
    )
    process._happypath_log = log  # closed by stop/exit of the test process

    endpoint = f"http://127.0.0.1:{port}/json"
    deadline = time.monotonic() + 25
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"HappyPath exited early with {process.returncode}; see {log_path}")
        try:
            with urllib.request.urlopen(endpoint, timeout=1) as response:
                pages = json.load(response)
            if pages:
                break
        except (OSError, json.JSONDecodeError):
            time.sleep(0.2)
    else:
        raise RuntimeError(f"CDP did not open on port {port}; see {log_path}")

    browser = playwright.chromium.connect_over_cdp(f"http://127.0.0.1:{port}")
    pages = [page for context in browser.contexts for page in context.pages]
    if not pages:
        raise RuntimeError("CDP connected but no WebView page was found")
    page = pages[0]
    page.wait_for_selector("#widget", timeout=15_000)
    return process, browser, page


def events() -> list[dict]:
    path = DATA_DIR / "events.jsonl"
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line]


def export_csv(page) -> str:
    path = Path(page.evaluate("() => window.__TAURI__.core.invoke('export_stats')"))
    check(path.resolve() == (DATA_DIR / "sessions.csv").resolve(), "CSV exports into isolated data dir")
    return path.read_text(encoding="utf-8-sig")


def main() -> None:
    check(EXE.exists(), f"release binary exists: {EXE}")
    shutil.rmtree(TEST_ROOT, ignore_errors=True)
    DATA_DIR.mkdir(parents=True)
    current = None
    js_errors: list[str] = []

    with sync_playwright() as playwright:
        try:
            current, _browser, page = launch(playwright, "webview-first")
            page.on("pageerror", lambda error: js_errors.append(str(error)))
            page.on("console", lambda message: js_errors.append(message.text) if message.type == "error" else None)
            check_taskbar_geometry()

            check(page.locator("#widget").get_attribute("data-status") == "idle", "initial state is idle")
            check(page.locator("#main-line").inner_text() == "00:03", "test duration comes from isolated config")
            check("0 сессий" in page.locator("#widget").get_attribute("title"), "initial all-time counter is zero")
            check(page.locator("#icon img").count() == 1, "selected raster moon demon is rendered")
            check(page.locator("#icon img").evaluate("img => img.complete && img.naturalWidth === 256"), "moon demon asset loads at full resolution")
            icon_size = page.locator("#icon img").evaluate("img => [img.getBoundingClientRect().width, img.getBoundingClientRect().height]")
            check(icon_size == [36, 36], "portrait receives another roughly 10 percent enlargement")
            check(page.locator("#sub-line").evaluate("el => el.scrollWidth <= el.clientWidth"), "subtitle is not clipped after enlarging portrait")
            check(page.locator("body").evaluate("el => el.scrollWidth <= el.clientWidth"), "enlarged portrait causes no horizontal overflow")
            check(page.locator("#icon svg").count() == 0, "no SVG or text glyph replaces the raster portrait")
            check(page.locator("#sub-line").inner_text() == "Нет времени на раскачку", "requested idle subtitle is shown")
            check(page.locator("#progress").evaluate("el => getComputedStyle(el).backgroundColor") == "rgba(0, 0, 0, 0)", "gray progress track is transparent")

            page.locator("#widget").click()
            page.locator('#widget[data-status="running"]').wait_for(timeout=5_000)
            check(True, "click starts timer")
            time.sleep(1.1)
            page.locator("#widget").click()
            page.locator('#widget[data-status="paused"]').wait_for(timeout=5_000)
            paused_time = page.locator("#main-line").inner_text()
            time.sleep(1.2)
            check(page.locator("#main-line").inner_text() == paused_time, "pause freezes remaining time")

            page.locator("#widget").click()
            page.locator('#widget[data-status="running"]').wait_for(timeout=5_000)
            page.locator('#widget[data-status="completed"]').wait_for(timeout=8_000)
            check(page.locator("#main-line").inner_text() == "Рубеж взят", "timer reaches daily milestone")
            check(page.locator("#icon img").count() == 1, "moon demon remains in completed state")
            check(page.locator(".day.won").count() == 1, "completed day fills one weekday segment")
            check("1 сессия" in page.locator("#widget").get_attribute("title"), "all-time counter increments")

            first_events = events()
            first_kinds = [item["event"] for item in first_events]
            check(first_kinds.count("session_started") == 1, "session_started persisted")
            check(first_kinds.count("session_paused") == 1, "session_paused persisted")
            check(first_kinds.count("session_resumed") == 1, "session_resumed persisted")
            check(first_kinds.count("session_completed") == 1, "session_completed persisted exactly once")
            check(len(export_csv(page).splitlines()) == 2, "CSV contains header and first completed session")

            # Start a second session and kill the app before its deadline. The
            # deadline in state.json must survive and complete after relaunch.
            page.locator("#widget").click()
            page.locator('#widget[data-status="running"]').wait_for(timeout=5_000)
            time.sleep(0.4)
            stop(current)
            current = None
            check(True, "app stopped in the middle of second session")

            current, _browser2, page2 = launch(playwright, "webview-second")
            page2.on("pageerror", lambda error: js_errors.append(str(error)))
            page2.locator('#widget[data-status="completed"]').wait_for(timeout=10_000)
            check("2 сессии" in page2.locator("#widget").get_attribute("title"), "restart restores and accumulates statistics")
            check(page2.locator(".day.won").count() == 1, "two sessions on one day still fill one weekday segment")

            completed = [item for item in events() if item["event"] == "session_completed"]
            check(len(completed) == 2, "restart writes second completion without duplicating first")
            check(len({item["session_id"] for item in completed}) == 2, "completed session IDs are unique")
            check(all(item["v"] == 1 for item in events()), "every JSONL event carries schema version")
            check(len(export_csv(page2).splitlines()) == 3, "CSV regenerates from full accumulated history")

            page2.evaluate("() => window.__TAURI__.core.invoke('reset_timer')")
            page2.locator('#widget[data-status="idle"]').wait_for(timeout=5_000)
            check("2 сессии" in page2.locator("#widget").get_attribute("title"), "reset clears current timer but preserves history")
            check(not js_errors, f"no JavaScript errors: {js_errors}")
            print(f"E2E DATA {DATA_DIR}", flush=True)
            print("ALL E2E CHECKS PASSED", flush=True)
        finally:
            stop(current)


if __name__ == "__main__":
    main()
