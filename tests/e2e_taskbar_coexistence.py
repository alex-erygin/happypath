"""Native Windows regression: HappyPath coexists with the real Hunger Timer.

Both applications use isolated data/WebView directories. Hunger Timer receives a
throwaway APPDATA with autostart explicitly disabled, so the test does not alter
its real statistics or the user's Run registry key.
"""
from __future__ import annotations

import ctypes
from ctypes import wintypes
import os
import shutil
import subprocess
import time
from pathlib import Path

from playwright.sync_api import sync_playwright

import e2e_live as base

ROOT = base.ROOT
HUNGER_EXE = ROOT.parent / "hunger-timer" / "src-tauri" / "target" / "release" / "hunger-timer.exe"
TEST_ROOT = ROOT / "src-tauri" / "target" / "e2e-coexistence"
USER32 = ctypes.windll.user32
USER32.SetProcessDPIAware()


def visible_window(title: str) -> int | None:
    USER32.IsWindowVisible.argtypes = [wintypes.HWND]
    USER32.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
    found: list[int] = []
    callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

    def visit(hwnd, _):
        if USER32.IsWindowVisible(hwnd):
            buffer = ctypes.create_unicode_buffer(512)
            USER32.GetWindowTextW(hwnd, buffer, len(buffer))
            if buffer.value == title:
                found.append(hwnd)
        return True

    USER32.EnumWindows(callback_type(visit), 0)
    return found[0] if found else None


def wait_window(title: str, timeout: float = 20) -> int:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        hwnd = visible_window(title)
        if hwnd:
            return hwnd
        time.sleep(0.2)
    raise AssertionError(f"window {title!r} did not become visible")


def launch_hunger() -> subprocess.Popen:
    appdata = TEST_ROOT / "hunger-appdata"
    hunger_data = appdata / "hunger-timer"
    hunger_data.mkdir(parents=True)
    (hunger_data / "config.json").write_text('{"autostart_off":true}', encoding="utf-8")
    env = os.environ.copy()
    env["APPDATA"] = str(appdata)
    env["WEBVIEW2_USER_DATA_FOLDER"] = str(TEST_ROOT / "hunger-webview")
    env.pop("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", None)
    log = (TEST_ROOT / "hunger.log").open("w", encoding="utf-8")
    process = subprocess.Popen(
        [str(HUNGER_EXE)], cwd=str(HUNGER_EXE.parent), env=env,
        stdout=log, stderr=subprocess.STDOUT,
    )
    process._happypath_log = log
    wait_window("hunger-timer")
    return process


def belongs_to(hwnd: int, root: int) -> bool:
    USER32.GetParent.argtypes = [wintypes.HWND]
    USER32.GetParent.restype = wintypes.HWND
    current = hwnd
    for _ in range(8):
        if current == root:
            return True
        current = USER32.GetParent(current)
        if not current:
            break
    return False


def click_empty_taskbar(left_limit: int) -> None:
    tray = USER32.FindWindowW("Shell_TrayWnd", None)
    tray_rect = base.window_rect(tray)
    # The free strip immediately left of the widget is taskbar background;
    # MSTaskListWClass itself spans the full taskbar and cannot delimit it.
    x = left_limit - 30
    if x <= tray_rect[0] + 100:
        raise AssertionError(f"no safe empty taskbar point before x={left_limit}")
    y = (tray_rect[1] + tray_rect[3]) // 2

    USER32.GetCursorPos.argtypes = [ctypes.POINTER(wintypes.POINT)]
    old = wintypes.POINT()
    USER32.GetCursorPos(ctypes.byref(old))
    USER32.SetCursorPos(x, y)
    USER32.mouse_event(0x0002, 0, 0, 0, 0)  # LEFTDOWN
    time.sleep(0.12)
    USER32.mouse_event(0x0004, 0, 0, 0, 0)  # LEFTUP
    USER32.SetCursorPos(old.x, old.y)
    print(f"CLICK empty taskbar at {(x, y)}", flush=True)


def assert_on_screen(title: str) -> None:
    hwnd = wait_window(title, 3)
    USER32.IsIconic.argtypes = [wintypes.HWND]
    USER32.IsIconic.restype = wintypes.BOOL
    base.check(not USER32.IsIconic(hwnd), f"{title} is not minimized after taskbar click")
    rect = base.window_rect(hwnd)
    center = wintypes.POINT((rect[0] + rect[2]) // 2, (rect[1] + rect[3]) // 2)
    USER32.WindowFromPoint.argtypes = [wintypes.POINT]
    USER32.WindowFromPoint.restype = wintypes.HWND
    hit = USER32.WindowFromPoint(center)
    base.check(belongs_to(hit, hwnd), f"{title} is visible at its center after taskbar click")


def main() -> None:
    base.check(HUNGER_EXE.exists(), f"real Hunger Timer release exists: {HUNGER_EXE}")
    base.check(base.EXE.exists(), f"HappyPath release exists: {base.EXE}")
    shutil.rmtree(TEST_ROOT, ignore_errors=True)
    TEST_ROOT.mkdir(parents=True)
    # base.launch writes into its own isolated target/e2e-data directory.
    shutil.rmtree(base.TEST_ROOT, ignore_errors=True)
    base.DATA_DIR.mkdir(parents=True)

    hunger = None
    happy = None
    with sync_playwright() as playwright:
        try:
            hunger = launch_hunger()
            happy, _browser, _page = base.launch(playwright, "webview-coexistence")
            time.sleep(2.0)
            hunger_rect = base.window_rect(wait_window("hunger-timer"))
            happy_rect = base.window_rect(wait_window("HappyPath"))
            print(f"HUNGER RECT {hunger_rect}; HAPPYPATH RECT {happy_rect}", flush=True)
            click_empty_taskbar(happy_rect[0])
            time.sleep(1.8)
            assert_on_screen("HappyPath")
            assert_on_screen("hunger-timer")
            base.check(
                happy_rect[2] <= hunger_rect[0],
                f"HappyPath does not overlap Hunger Timer: {happy_rect} vs {hunger_rect}",
            )

            # Exercise the visibility guard itself, independently of geometry.
            USER32.ShowWindow.argtypes = [wintypes.HWND, ctypes.c_int]
            happy_hwnd = wait_window("HappyPath")
            USER32.ShowWindow(happy_hwnd, 6)  # SW_MINIMIZE
            time.sleep(1.0)
            assert_on_screen("HappyPath")
            USER32.ShowWindow(happy_hwnd, 0)  # SW_HIDE
            time.sleep(1.0)
            assert_on_screen("HappyPath")
            print("ALL COEXISTENCE CHECKS PASSED", flush=True)
        finally:
            base.stop(happy)
            base.stop(hunger)


if __name__ == "__main__":
    main()
