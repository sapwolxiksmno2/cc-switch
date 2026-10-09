use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::Manager;

/// 仅记录主窗口普通状态的物理几何；最大化时仍保留还原所需的位置和尺寸。
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct WindowState {
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    maximized: bool,
}

impl WindowState {
    fn has_valid_size(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.width <= i32::MAX as u32
            && self.height <= i32::MAX as u32
    }
}

#[derive(Deserialize, Serialize)]
struct WindowStates {
    main: Option<WindowState>,
}

#[derive(Default)]
struct CachedState {
    state: Option<WindowState>,
    restoring: bool,
}

struct Snapshot {
    maximized: bool,
    minimized: bool,
    geometry: Option<WindowState>,
}

impl CachedState {
    fn update(&mut self, snapshot: Snapshot) {
        if self.restoring || snapshot.minimized {
            return;
        }

        if let Some(mut geometry) = snapshot.geometry {
            if geometry.has_valid_size() {
                geometry.maximized = snapshot.maximized;
                self.state = Some(geometry);
            }
        } else if let Some(state) = self.state.as_mut() {
            state.maximized = snapshot.maximized;
        }
    }
}

static CACHE: Mutex<CachedState> = Mutex::new(CachedState {
    state: None,
    restoring: false,
});

#[derive(Clone, Copy)]
struct MonitorBounds {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("window-state.json")
}

fn load_from_data_dir(data_dir: &Path) -> Result<Option<WindowState>, String> {
    let path = state_path(data_dir);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("读取窗口状态文件失败（{}）：{error}", path.display())),
    };
    let states: WindowStates = serde_json::from_slice(&bytes).map_err(|error| {
        // 反序列化错误可能包含输入值，只记录位置，不将文件内容写入日志。
        format!(
            "窗口状态 JSON 无效（{}，行 {}，列 {}）",
            path.display(),
            error.line(),
            error.column()
        )
    })?;
    if states.main.as_ref().is_some_and(|state| !state.has_valid_size()) {
        return Err(format!("窗口状态尺寸无效（{}）", path.display()));
    }
    Ok(states.main)
}

fn save_to_data_dir(data_dir: &Path, state: Option<&WindowState>) -> Result<(), String> {
    let Some(state) = state else {
        // 轻量模式或窗口几何读取失败时，不创建空状态、不擦除原来的有效文件。
        return Ok(());
    };
    let states = WindowStates {
        main: Some(state.clone()),
    };
    let bytes = serde_json::to_vec_pretty(&states)
        .map_err(|_| "序列化 Portable 窗口状态失败".to_string())?;
    // 复用原子替换实现；临时文件与最终文件都创建在 Portable 的 data 内。
    crate::config::atomic_write(&state_path(data_dir), &bytes).map_err(|error| error.to_string())
}

fn position_is_visible(state: &WindowState, monitors: &[MonitorBounds]) -> bool {
    if !state.has_valid_size() {
        return false;
    }
    let x = i64::from(state.x);
    let y = i64::from(state.y);
    let right = x + i64::from(state.width);
    let usable_width = i64::from(state.width.min(128));
    let usable_height = i64::from(state.height.min(32));
    monitors.iter().any(|monitor| {
        let left = i64::from(monitor.x);
        let top = i64::from(monitor.y);
        let monitor_right = left + i64::from(monitor.width);
        let monitor_bottom = top + i64::from(monitor.height);
        // 标题栏至少保留可拖动的物理区域，只有几像素相交时安全居中。
        monitor.width > 0
            && monitor.height > 0
            && right.min(monitor_right) - x.max(left) >= usable_width
            && y >= top
            && y + usable_height <= monitor_bottom
    })
}

fn capture(app: &tauri::AppHandle) {
    if CACHE.lock().unwrap_or_else(|error| error.into_inner()).restoring {
        return;
    }
    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    // 不持有缓存锁查询窗口，避免退出时主线程与工作线程互相等待。
    let snapshot = (|| -> tauri::Result<Snapshot> {
        let maximized = window.is_maximized()?;
        let minimized = window.is_minimized()?;
        let geometry = if !maximized && !minimized {
            let size = window.inner_size()?;
            let position = window.outer_position()?;
            Some(WindowState {
                width: size.width,
                height: size.height,
                x: position.x,
                y: position.y,
                maximized,
            })
        } else {
            None
        };
        Ok(Snapshot {
            maximized,
            minimized,
            geometry,
        })
    })();

    match snapshot {
        Ok(snapshot) => CACHE
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .update(snapshot),
        Err(error) => log::debug!("读取 Portable 窗口几何失败，保留已有状态：{error}"),
    }
}

/// 主窗口创建后恢复状态；坏文件不阻断启动，也不访问安装版的配置目录。
pub(crate) fn restore(app: &tauri::AppHandle) {
    let Some(data_dir) = crate::portable::data_dir() else {
        return;
    };
    let Some(window) = app.get_webview_window("main") else {
        return;
    };

    CACHE.lock().unwrap_or_else(|error| error.into_inner()).restoring = true;
    let state = match load_from_data_dir(&data_dir) {
        Ok(state) => state,
        Err(error) => {
            log::warn!("读取 Portable 窗口状态失败，使用默认窗口配置：{error}");
            None
        }
    };
    CACHE.lock().unwrap_or_else(|error| error.into_inner()).state = state.clone();

    if let Some(state) = state {
        let restored = (|| -> tauri::Result<()> {
            window.set_size(tauri::PhysicalSize::new(state.width, state.height))?;
            let position_valid = window
                .available_monitors()
                .map(|monitors| {
                    let bounds = monitors
                        .iter()
                        .map(|monitor| MonitorBounds {
                            x: monitor.position().x,
                            y: monitor.position().y,
                            width: monitor.size().width,
                            height: monitor.size().height,
                        })
                        .collect::<Vec<_>>();
                    position_is_visible(&state, &bounds)
                })
                .unwrap_or(false);
            if position_valid {
                window.set_position(tauri::PhysicalPosition::new(state.x, state.y))?;
            } else {
                // 显示器已移除或无法枚举时，用当前窗口的默认屏幕安全居中。
                window.center()?;
            }
            if state.maximized {
                window.maximize()?;
            }
            Ok(())
        })();
        if let Err(error) = restored {
            log::warn!("恢复 Portable 窗口状态失败，保留当前窗口：{error}");
        }
    }

    CACHE.lock().unwrap_or_else(|error| error.into_inner()).restoring = false;
    capture(app);
}

/// 跟踪主窗口的移动、尺寸变化和关闭，隐藏或销毁窗口前保留普通几何。
pub(crate) fn on_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    if !crate::portable::is_portable() || window.label() != "main" {
        return;
    }
    if matches!(
        event,
        tauri::WindowEvent::Moved(_)
            | tauri::WindowEvent::Resized(_)
            | tauri::WindowEvent::CloseRequested { .. }
    ) {
        capture(window.app_handle());
    }
}

/// 退出或重启时只保存事件缓存，不跨线程查询窗口；普通安装模式不调用此函数。
pub(crate) fn save() -> Result<(), String> {
    let Some(data_dir) = crate::portable::data_dir() else {
        return Ok(());
    };
    let state = CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .state
        .clone();
    save_to_data_dir(&data_dir, state.as_ref())
}

#[cfg(test)]
mod portable_window_state_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn normal_state() -> WindowState {
        WindowState {
            width: 1000,
            height: 650,
            x: 120,
            y: 80,
            maximized: false,
        }
    }

    #[test]
    fn portable_window_state_creates_data_and_round_trips_only_main() {
        let fixture = tempdir().unwrap();
        let data = fixture.path().join("data");
        let state = normal_state();

        save_to_data_dir(&data, Some(&state)).unwrap();

        assert_eq!(state_path(&data), data.join("window-state.json"));
        assert_eq!(load_from_data_dir(&data).unwrap(), Some(state));
        assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&data).unwrap().count(), 1);
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(state_path(&data)).unwrap()).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 1);
        assert!(json.get("main").is_some());
    }

    #[test]
    fn portable_window_state_missing_geometry_preserves_existing_file() {
        let fixture = tempdir().unwrap();
        let data = fixture.path().join("data");
        save_to_data_dir(&data, Some(&normal_state())).unwrap();
        let before = fs::read(state_path(&data)).unwrap();

        save_to_data_dir(&data, None).unwrap();

        assert_eq!(fs::read(state_path(&data)).unwrap(), before);
        let unused_data = fixture.path().join("unused-data");
        save_to_data_dir(&unused_data, None).unwrap();
        assert!(!unused_data.exists());
    }

    #[test]
    fn portable_window_state_maximize_preserves_normal_geometry() {
        let original = normal_state();
        let mut cache = CachedState {
            state: Some(original.clone()),
            restoring: false,
        };

        cache.update(Snapshot {
            maximized: true,
            minimized: false,
            geometry: None,
        });

        assert_eq!(cache.state.as_ref().unwrap().width, original.width);
        assert_eq!(cache.state.as_ref().unwrap().height, original.height);
        assert_eq!(cache.state.as_ref().unwrap().x, original.x);
        assert_eq!(cache.state.as_ref().unwrap().y, original.y);
        assert!(cache.state.as_ref().unwrap().maximized);
    }

    #[test]
    fn portable_window_state_minimize_preserves_maximized_flag() {
        let mut original = normal_state();
        original.maximized = true;
        let mut cache = CachedState {
            state: Some(original.clone()),
            restoring: false,
        };

        cache.update(Snapshot {
            maximized: false,
            minimized: true,
            geometry: None,
        });

        assert_eq!(cache.state, Some(original));
    }

    #[test]
    fn portable_window_state_normal_resize_replaces_cached_geometry() {
        let mut cache = CachedState {
            state: Some(normal_state()),
            restoring: false,
        };
        let mut next = normal_state();
        next.width = 1100;
        next.height = 700;
        next.x = -1000;
        next.y = 140;

        cache.update(Snapshot {
            maximized: false,
            minimized: false,
            geometry: Some(next.clone()),
        });

        assert_eq!(cache.state, Some(next));
    }

    #[test]
    fn portable_window_state_rejects_bad_json_and_zero_size() {
        let fixture = tempdir().unwrap();
        let data = fixture.path().join("data");
        assert_eq!(load_from_data_dir(&data).unwrap(), None);
        fs::create_dir_all(&data).unwrap();
        fs::write(state_path(&data), b"invalid JSON").unwrap();
        assert!(load_from_data_dir(&data).is_err());
        fs::write(
            state_path(&data),
            br#"{"main":{"width":0,"height":650,"x":0,"y":0,"maximized":false}}"#,
        )
        .unwrap();
        assert!(load_from_data_dir(&data).is_err());
    }

    #[test]
    fn portable_window_state_removed_monitor_is_not_restored() {
        let monitor = MonitorBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let mut state = normal_state();
        assert!(position_is_visible(&state, &[monitor]));
        state.x = -2000;
        assert!(!position_is_visible(&state, &[monitor]));
        state.x = 100;
        state.y = -1000;
        assert!(!position_is_visible(&state, &[monitor]));
        assert!(!position_is_visible(&normal_state(), &[]));
    }

    #[test]
    fn portable_window_state_requires_usable_title_bar_area() {
        let monitor = MonitorBounds {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let mut state = normal_state();
        state.x = 1919;
        assert!(!position_is_visible(&state, &[monitor]), "只有一像素水平交叠不可操作");
        state.x = 1 - state.width as i32;
        assert!(!position_is_visible(&state, &[monitor]));
        state.x = 100;
        state.y = 1079;
        assert!(!position_is_visible(&state, &[monitor]), "屏幕底部标题栏不可操作");
        state.y = 1049;
        assert!(!position_is_visible(&state, &[monitor]));
        state.x = 1920 - 128;
        state.y = 1080 - 32;
        assert!(position_is_visible(&state, &[monitor]));
        state.x = 128 - state.width as i32;
        assert!(position_is_visible(&state, &[monitor]));
        state.width = 64;
        state.height = 24;
        state.x = 1920 - 64;
        state.y = 1080 - 24;
        assert!(position_is_visible(&state, &[monitor]), "小窗口按实际尺寸保留可操作区域");
    }

    #[test]
    fn portable_window_state_negative_monitor_coordinates_remain_valid() {
        let monitor = MonitorBounds {
            x: -1920,
            y: -1080,
            width: 1920,
            height: 1080,
        };
        let mut state = normal_state();
        state.x = -1600;
        state.y = -900;
        assert!(position_is_visible(&state, &[monitor]));
        state.x = i32::MAX;
        state.y = i32::MAX;
        assert!(!position_is_visible(&state, &[monitor]));
    }
}
