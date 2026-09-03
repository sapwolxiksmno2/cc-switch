use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const LEGACY_PORTABLE_MARKER: &str = "portable.ini";
const PORTABLE_DATA_DIR: &str = "data";
const PORTABLE_MARKER: &str = "portable.ini";

static RUNTIME_PATHS: OnceLock<Option<PortablePaths>> = OnceLock::new();
static NORMAL_WINDOW_STATE: OnceLock<Mutex<Option<PortableWindowState>>> = OnceLock::new();

/// Every path owned by CC Switch in a Windows portable build.
///
/// External tool configuration paths deliberately do not use this type. They
/// continue to resolve through their existing Claude/Codex/Gemini/etc. path
/// helpers, so only CC Switch's own files are redirected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortablePaths {
    pub data_dir: PathBuf,
}

impl PortablePaths {
    pub fn for_executable(executable: &Path) -> Option<Self> {
        let root_dir = executable.parent()?.to_path_buf();
        let legacy_marker = root_dir.join(LEGACY_PORTABLE_MARKER);
        let data_marker = root_dir.join(PORTABLE_DATA_DIR).join(PORTABLE_MARKER);
        if !legacy_marker.is_file() && !data_marker.is_file() {
            return None;
        }

        Some(Self {
            data_dir: root_dir.join(PORTABLE_DATA_DIR),
        })
    }

    pub fn temp_dir(&self) -> PathBuf {
        self.data_dir.join("temp")
    }

    pub fn webview_dir(&self) -> PathBuf {
        self.data_dir.join("webview")
    }

    pub fn app_store_path(&self) -> PathBuf {
        self.data_dir.join("app_paths.json")
    }

    pub fn window_state_path(&self) -> PathBuf {
        self.data_dir.join("window-state.ini")
    }

    fn create_directories(&self) -> io::Result<()> {
        fs::create_dir_all(&self.data_dir)?;
        fs::create_dir_all(self.temp_dir())?;
        fs::create_dir_all(self.webview_dir())?;
        Ok(())
    }

    #[cfg(target_os = "windows")]
    fn apply_process_environment(&self) {
        let temp_dir = self.temp_dir();
        let webview_dir = self.webview_dir();

        // GetTempPathW honors TEMP/TMP. tempfile, command staging files, sync
        // archives and child processes therefore stay below data/temp.
        std::env::set_var("TEMP", &temp_dir);
        std::env::set_var("TMP", &temp_dir);

        // WebView2 owns localStorage, IndexedDB, browser cache, GPU cache and
        // Crashpad files. This override must be set before Tauri creates a
        // WebView2 environment.
        std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", &webview_dir);
    }

    #[cfg(not(target_os = "windows"))]
    fn apply_process_environment(&self) {}
}

/// Resolve portable paths for the running executable without mutating state.
pub fn paths() -> Option<PortablePaths> {
    if let Some(cached) = RUNTIME_PATHS.get() {
        return cached.clone();
    }

    std::env::current_exe()
        .ok()
        .and_then(|executable| PortablePaths::for_executable(&executable))
}

/// Prepare process-wide paths before Tauri, WebView2, logging or the panic hook
/// can create files. Directory creation failures are reported to stderr while
/// retaining the portable path choice; falling back to a profile directory
/// would violate portable isolation.
pub fn prepare_runtime() -> Option<PortablePaths> {
    let detected = std::env::current_exe()
        .ok()
        .and_then(|executable| PortablePaths::for_executable(&executable));
    let paths = RUNTIME_PATHS.get_or_init(|| detected).clone()?;
    paths.apply_process_environment();
    if let Err(error) = paths.create_directories() {
        eprintln!("Failed to prepare CC Switch portable data directory: {error}");
    }
    Some(paths)
}

pub fn is_portable_mode() -> bool {
    paths().is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortableWindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

impl PortableWindowState {
    pub fn encode(self) -> String {
        format!(
            "x={}\ny={}\nwidth={}\nheight={}\nmaximized={}\n",
            self.x, self.y, self.width, self.height, self.maximized
        )
    }

    pub fn decode(input: &str) -> Option<Self> {
        let mut x = None;
        let mut y = None;
        let mut width = None;
        let mut height = None;
        let mut maximized = None;

        for line in input.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "x" => x = value.trim().parse().ok(),
                "y" => y = value.trim().parse().ok(),
                "width" => width = value.trim().parse().ok(),
                "height" => height = value.trim().parse().ok(),
                "maximized" => maximized = value.trim().parse().ok(),
                _ => {}
            }
        }

        let state = Self {
            x: x?,
            y: y?,
            width: width?,
            height: height?,
            maximized: maximized?,
        };
        if state.width < 320
            || state.height < 240
            || state.width > 32_768
            || state.height > 32_768
        {
            return None;
        }
        Some(state)
    }
}

pub fn remember_normal_window_state(mut state: PortableWindowState) {
    state.maximized = false;
    let slot = NORMAL_WINDOW_STATE.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = slot.lock() {
        *guard = Some(state);
    }
}

pub fn normal_window_state() -> Option<PortableWindowState> {
    NORMAL_WINDOW_STATE
        .get()
        .and_then(|slot| slot.lock().ok().and_then(|guard| *guard))
}

fn select_window_state_for_save(
    current: PortableWindowState,
    normal: Option<PortableWindowState>,
) -> PortableWindowState {
    if current.maximized {
        return normal
            .map(|normal| PortableWindowState {
                maximized: true,
                ..normal
            })
            .unwrap_or(current);
    }
    current
}

pub fn window_state_for_save(current: PortableWindowState) -> PortableWindowState {
    select_window_state_for_save(current, normal_window_state())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_root(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::current_dir().unwrap().join(format!(
            ".portable-path-test-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn no_marker_means_installed_mode() {
        let root = test_root("installed");
        fs::create_dir(&root).unwrap();
        let executable = root.join("cc-switch.exe");

        assert_eq!(PortablePaths::for_executable(&executable), None);

        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn data_marker_maps_every_owned_path_below_data() {
        let root = test_root("data-marker");
        let data = root.join(PORTABLE_DATA_DIR);
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join(PORTABLE_MARKER), "portable=true\n").unwrap();

        let paths = PortablePaths::for_executable(&root.join("cc-switch.exe")).unwrap();

        assert_eq!(paths.data_dir, data);
        assert_eq!(paths.temp_dir(), data.join("temp"));
        assert_eq!(paths.webview_dir(), data.join("webview"));
        assert_eq!(paths.app_store_path(), data.join("app_paths.json"));
        assert_eq!(paths.window_state_path(), data.join("window-state.ini"));

        fs::remove_file(data.join(PORTABLE_MARKER)).unwrap();
        fs::remove_dir(data).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn legacy_root_marker_remains_compatible() {
        let root = test_root("legacy-marker");
        fs::create_dir(&root).unwrap();
        fs::write(root.join(LEGACY_PORTABLE_MARKER), "portable=true\n").unwrap();

        let paths = PortablePaths::for_executable(&root.join("cc-switch.exe")).unwrap();
        assert_eq!(paths.data_dir, root.join(PORTABLE_DATA_DIR));

        fs::remove_file(root.join(LEGACY_PORTABLE_MARKER)).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn directory_preparation_stays_below_data() {
        let root = test_root("prepare");
        fs::create_dir(&root).unwrap();
        fs::write(root.join(LEGACY_PORTABLE_MARKER), "portable=true\n").unwrap();
        let paths = PortablePaths::for_executable(&root.join("cc-switch.exe")).unwrap();

        paths.create_directories().unwrap();

        assert!(paths.data_dir.is_dir());
        assert!(paths.temp_dir().is_dir());
        assert!(paths.webview_dir().is_dir());

        fs::remove_dir(paths.temp_dir()).unwrap();
        fs::remove_dir(paths.webview_dir()).unwrap();
        fs::remove_dir(paths.data_dir).unwrap();
        fs::remove_file(root.join(LEGACY_PORTABLE_MARKER)).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn portable_window_state_round_trips() {
        let state = PortableWindowState {
            x: -120,
            y: 48,
            width: 1280,
            height: 720,
            maximized: true,
        };

        assert_eq!(PortableWindowState::decode(&state.encode()), Some(state));
    }

    #[test]
    fn portable_window_state_rejects_invalid_dimensions() {
        assert_eq!(
            PortableWindowState::decode("x=0\ny=0\nwidth=0\nheight=650\nmaximized=false\n"),
            None
        );
    }

    #[test]
    fn maximized_save_uses_last_normal_bounds() {
        let current = PortableWindowState {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            maximized: true,
        };
        let normal = PortableWindowState {
            x: 120,
            y: 80,
            width: 1100,
            height: 700,
            maximized: false,
        };

        assert_eq!(
            select_window_state_for_save(current, Some(normal)),
            PortableWindowState {
                maximized: true,
                ..normal
            }
        );
    }
}
