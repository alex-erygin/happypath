#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod stats;

use chrono::Utc;
use stats::{Snapshot, StatsStore};
use std::path::Path;
use std::sync::Mutex;
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager,
};

#[tauri::command]
fn get_snapshot(store: tauri::State<'_, Mutex<StatsStore>>) -> Result<Snapshot, String> {
    store
        .lock()
        .map_err(|_| "stats store lock poisoned".to_string())?
        .snapshot_at(Utc::now())
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn toggle_timer(store: tauri::State<'_, Mutex<StatsStore>>) -> Result<Snapshot, String> {
    store
        .lock()
        .map_err(|_| "stats store lock poisoned".to_string())?
        .toggle_at(Utc::now())
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn reset_timer(store: tauri::State<'_, Mutex<StatsStore>>) -> Result<Snapshot, String> {
    store
        .lock()
        .map_err(|_| "stats store lock poisoned".to_string())?
        .reset_at(Utc::now())
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn export_stats(store: tauri::State<'_, Mutex<StatsStore>>) -> Result<String, String> {
    store
        .lock()
        .map_err(|_| "stats store lock poisoned".to_string())?
        .export_csv()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| error.to_string())
}

#[cfg(windows)]
fn open_folder(path: &Path) {
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

#[cfg(not(windows))]
fn open_folder(_path: &Path) {}

/// Tool window не сворачивается по Win+D / «Показать рабочий стол».
#[cfg(windows)]
fn make_tool_window(win: &tauri::WebviewWindow) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::*;

    if let Ok(raw) = win.hwnd() {
        let hwnd = HWND(raw.0 as _);
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let new_ex = (ex | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize);
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new_ex);
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }
}

#[cfg(windows)]
fn widget_position_near_tray(win_w: i32, win_h: i32) -> Option<(i32, i32)> {
    use windows::core::w;
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, FindWindowW, GetWindowRect};

    unsafe {
        let tray = FindWindowW(w!("Shell_TrayWnd"), None).ok()?;
        let notify =
            FindWindowExW(Some(tray), Some(HWND::default()), w!("TrayNotifyWnd"), None).ok()?;
        let mut trc: RECT = std::mem::zeroed();
        GetWindowRect(tray, &mut trc).ok()?;
        let mut nrc: RECT = std::mem::zeroed();
        GetWindowRect(notify, &mut nrc).ok()?;
        // Физические пиксели. Зазор сохраняет кликабельной стрелку скрытых значков.
        let x = nrc.left - win_w - 108;
        let y = trc.top + (trc.bottom - trc.top - win_h) / 2;
        Some((x, y))
    }
}

/// Возвращает виджет поверх таскбара только при реальном перекрытии: слепой
/// TOPMOST-реордер на каждом тике рвёт mouse down в WebView2.
#[cfg(windows)]
fn spawn_zorder_keeper(win: &tauri::WebviewWindow) {
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::UI::WindowsAndMessaging::*;

    if let Ok(raw) = win.hwnd() {
        let hwnd_raw = raw.0 as isize;
        std::thread::spawn(move || {
            let mut hidden = false;
            let mut tick: u32 = 0;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(300));
                tick = tick.wrapping_add(1);
                let hwnd = HWND(hwnd_raw as _);
                unsafe {
                    if tick.is_multiple_of(4) {
                        let mut rc: RECT = std::mem::zeroed();
                        if GetWindowRect(hwnd, &mut rc).is_ok() {
                            let w = rc.right - rc.left;
                            let h = rc.bottom - rc.top;
                            if let Some((nx, ny)) = widget_position_near_tray(w, h) {
                                if nx != rc.left || ny != rc.top {
                                    let _ = SetWindowPos(
                                        hwnd,
                                        Some(HWND_TOPMOST),
                                        nx,
                                        ny,
                                        0,
                                        0,
                                        SWP_NOSIZE | SWP_NOACTIVATE,
                                    );
                                }
                            }
                        }
                    }
                    let fullscreen = fullscreen_app_running();
                    if fullscreen && !hidden {
                        let _ = ShowWindow(hwnd, SW_HIDE);
                        hidden = true;
                    } else if !fullscreen && hidden {
                        let _ = ShowWindow(hwnd, SW_SHOWNA);
                        hidden = false;
                    }
                    if !hidden {
                        let mut rc: RECT = std::mem::zeroed();
                        if GetWindowRect(hwnd, &mut rc).is_ok() {
                            let pt = POINT {
                                x: (rc.left + rc.right) / 2,
                                y: (rc.top + rc.bottom) / 2,
                            };
                            let hit = WindowFromPoint(pt);
                            let mut ours = hit == hwnd;
                            let mut parent = hit;
                            for _ in 0..8 {
                                if ours {
                                    break;
                                }
                                match GetParent(parent) {
                                    Ok(value) if value != HWND::default() => {
                                        ours = value == hwnd;
                                        parent = value;
                                    }
                                    _ => break,
                                }
                            }
                            if !ours {
                                let _ = SetWindowPos(
                                    hwnd,
                                    Some(HWND_TOPMOST),
                                    0,
                                    0,
                                    0,
                                    0,
                                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                                );
                            }
                        }
                    }
                }
            }
        });
    }
}

#[cfg(windows)]
fn fullscreen_app_running() -> bool {
    use windows::Win32::UI::Shell::{
        SHQueryUserNotificationState, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
    };
    unsafe {
        SHQueryUserNotificationState()
            .map(|state| state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE)
            .unwrap_or(false)
    }
}

fn main() {
    let store = StatsStore::load().unwrap_or_else(|error| {
        panic!(
            "HappyPath: не удалось открыть папку статистики {}: {error}",
            stats::data_dir().display()
        )
    });

    tauri::Builder::default()
        .manage(Mutex::new(store))
        .invoke_handler(tauri::generate_handler![
            get_snapshot,
            toggle_timer,
            reset_timer,
            export_stats
        ])
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.unminimize();
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
        .setup(|app| {
            if let Some(win) = app.get_webview_window("main") {
                #[cfg(windows)]
                let positioned = if let Ok(size) = win.outer_size() {
                    widget_position_near_tray(size.width as i32, size.height as i32)
                        .map(|(x, y)| {
                            let _ = win.set_position(tauri::Position::Physical(
                                tauri::PhysicalPosition { x, y },
                            ));
                        })
                        .is_some()
                } else {
                    false
                };
                #[cfg(not(windows))]
                let positioned = false;
                if !positioned {
                    if let Ok(Some(monitor)) = win.primary_monitor() {
                        let mon = monitor.size();
                        if let Ok(size) = win.outer_size() {
                            let _ = win.set_position(tauri::Position::Physical(
                                tauri::PhysicalPosition {
                                    x: 12,
                                    y: mon.height as i32 - size.height as i32,
                                },
                            ));
                        }
                    }
                }
                #[cfg(windows)]
                {
                    make_tool_window(&win);
                    spawn_zorder_keeper(&win);
                }
            }

            let reset =
                MenuItem::with_id(app, "reset", "Сбросить текущий таймер", true, None::<&str>)?;
            let export = MenuItem::with_id(
                app,
                "export",
                "Обновить CSV и открыть папку",
                true,
                None::<&str>,
            )?;
            let open =
                MenuItem::with_id(app, "open", "Открыть папку статистики", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&reset, &export, &open, &quit])?;
            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .tooltip("HappyPath — Лунный рубеж")
                .menu(&menu)
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "reset" => {
                        let store = app.state::<Mutex<StatsStore>>();
                        if let Ok(mut store) = store.lock() {
                            let _ = store.reset_at(Utc::now());
                        };
                    }
                    "export" => {
                        let store = app.state::<Mutex<StatsStore>>();
                        if let Ok(store) = store.lock() {
                            if let Ok(path) = store.export_csv() {
                                if let Some(parent) = path.parent() {
                                    open_folder(parent);
                                }
                            }
                        };
                    }
                    "open" => {
                        let store = app.state::<Mutex<StatsStore>>();
                        if let Ok(store) = store.lock() {
                            open_folder(store.data_dir());
                        };
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running HappyPath");
}
