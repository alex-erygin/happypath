"""Isolated live test for opt-in Windows autostart.

Uses a test-only Run value name and an isolated HAPPYPATH_DATA_DIR. The real
HappyPath Run value and real statistics are never read or changed.
"""
from __future__ import annotations

import os
import shutil
import subprocess
import time
import winreg
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "src-tauri" / "target" / "release" / "happypath.exe"
TEST_ROOT = ROOT / "src-tauri" / "target" / "e2e-autostart"
DATA_DIR = TEST_ROOT / "stats"
RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
VALUE_NAME = f"HappyPath-E2E-{os.getpid()}"


def check(condition: bool, name: str) -> None:
    if not condition:
        raise AssertionError(name)
    print(f"PASS {name}", flush=True)


def delete_value() -> None:
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as key:
            winreg.DeleteValue(key, VALUE_NAME)
    except FileNotFoundError:
        pass


def stored_path() -> str | None:
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_READ) as key:
            value, value_type = winreg.QueryValueEx(key, VALUE_NAME)
        check(value_type == winreg.REG_SZ, "autostart uses REG_SZ")
        return value
    except FileNotFoundError:
        return None


def seed_value(path: str) -> None:
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as key:
        winreg.SetValueEx(key, VALUE_NAME, 0, winreg.REG_SZ, path)


def stop(process: subprocess.Popen | None) -> None:
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=8)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def launch(profile: str) -> subprocess.Popen:
    env = os.environ.copy()
    env.pop("HAPPYPATH_DISABLE_AUTOSTART", None)
    env["HAPPYPATH_DATA_DIR"] = str(DATA_DIR)
    env["HAPPYPATH_AUTOSTART_VALUE"] = VALUE_NAME
    env["WEBVIEW2_USER_DATA_FOLDER"] = str(TEST_ROOT / profile)
    env.pop("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", None)
    log = (TEST_ROOT / f"{profile}.log").open("w", encoding="utf-8")
    process = subprocess.Popen(
        [str(EXE)], cwd=str(ROOT), env=env, stdout=log, stderr=subprocess.STDOUT
    )
    process._happypath_log = log
    return process


def assert_running(process: subprocess.Popen) -> None:
    time.sleep(4)
    check(process.poll() is None, "app remains running")


def main() -> None:
    check(EXE.exists(), f"release binary exists: {EXE}")
    shutil.rmtree(TEST_ROOT, ignore_errors=True)
    DATA_DIR.mkdir(parents=True)
    delete_value()
    current = None
    try:
        current = launch("default-off")
        assert_running(current)
        check(stored_path() is None, "ordinary startup leaves autostart disabled")
        check(not DATA_DIR.joinpath("config.json").exists(), "no redundant opt-out config is created")
        stop(current)
        current = None

        # Simulate a previous explicit opt-in. Ordinary startup must not rewrite
        # the stored path anymore; only the tray checkbox may change this value.
        previous_path = r"C:\previous-installation\happypath.exe"
        seed_value(previous_path)
        current = launch("existing-opt-in")
        assert_running(current)
        check(stored_path() == previous_path, "startup never rewrites an existing opt-in value")
    finally:
        stop(current)
        delete_value()
        shutil.rmtree(TEST_ROOT, ignore_errors=True)
    print("ALL AUTOSTART CHECKS PASSED", flush=True)


if __name__ == "__main__":
    main()
