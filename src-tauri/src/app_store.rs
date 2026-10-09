use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use tauri_plugin_store::StoreExt;

use crate::error::AppError;

/// Store 中的键名
const STORE_KEY_APP_CONFIG_DIR: &str = "app_config_dir_override";

/// 缓存当前的 app_config_dir 覆盖路径，避免存储 AppHandle
static APP_CONFIG_DIR_OVERRIDE: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

fn override_cache() -> &'static RwLock<Option<PathBuf>> {
    APP_CONFIG_DIR_OVERRIDE.get_or_init(|| RwLock::new(None))
}

fn update_cached_override(value: Option<PathBuf>) {
    if let Ok(mut guard) = override_cache().write() {
        *guard = value;
    }
}

/// 获取 app_config_dir 覆盖路径，Portable 始终显示固定数据目录。
pub fn get_app_config_dir_override() -> Option<PathBuf> {
    if let Some(data_dir) = crate::portable::data_dir() {
        return Some(data_dir);
    }
    override_cache().read().ok()?.clone()
}

fn store_path() -> PathBuf {
    store_path_for_data_dir(crate::portable::data_dir().as_deref())
}

fn store_path_for_data_dir(portable_data_dir: Option<&Path>) -> PathBuf {
    match portable_data_dir {
        Some(data_dir) => data_dir.join("app_paths.json"),
        None => PathBuf::from("app_paths.json"),
    }
}

fn validate_portable_override(data_dir: &Path, path: Option<&str>) -> Result<(), AppError> {
    if let Some(path) = path.map(str::trim).filter(|path| !path.is_empty()) {
        if Path::new(path) != data_dir {
            return Err(AppError::Message(
                "Portable 模式的数据目录固定为程序目录下的 data，不能修改".to_string(),
            ));
        }
    }
    Ok(())
}

fn read_override_from_store(app: &tauri::AppHandle) -> Option<PathBuf> {
    // 不读取旧 Store 或 Portable Store 中的覆盖值，避免外部目录影响数据隔离。
    if let Some(data_dir) = crate::portable::data_dir() {
        return Some(data_dir);
    }

    let store = match app.store_builder(store_path()).build() {
        Ok(store) => store,
        Err(e) => {
            log::warn!("无法创建 Store: {e}");
            return None;
        }
    };

    match store.get(STORE_KEY_APP_CONFIG_DIR) {
        Some(Value::String(path_str)) => {
            let path_str = path_str.trim();
            if path_str.is_empty() {
                return None;
            }

            let path = resolve_path(path_str);

            if !path.exists() {
                log::warn!(
                    "Store 中配置的 app_config_dir 不存在: {path:?}\n\
                     将使用默认路径。"
                );
                return None;
            }

            log::info!("使用 Store 中的 app_config_dir: {path:?}");
            Some(path)
        }
        Some(_) => {
            log::warn!("Store 中的 {STORE_KEY_APP_CONFIG_DIR} 类型不正确，应为字符串");
            None
        }
        None => None,
    }
}

/// 从 Store 刷新 app_config_dir 覆盖值并更新缓存
pub fn refresh_app_config_dir_override(app: &tauri::AppHandle) -> Option<PathBuf> {
    let value = read_override_from_store(app);
    update_cached_override(value.clone());
    value
}

/// 写入 app_config_dir 到 Tauri Store
pub fn set_app_config_dir_to_store(
    app: &tauri::AppHandle,
    path: Option<&str>,
) -> Result<(), AppError> {
    if let Some(data_dir) = crate::portable::data_dir() {
        // 设置页会回传查询到的固定目录；重置与保存当前值均为无写入的幂等操作。
        validate_portable_override(&data_dir, path)?;
        update_cached_override(Some(data_dir));
        return Ok(());
    }

    let store = app
        .store_builder(store_path())
        .build()
        .map_err(|e| AppError::Message(format!("创建 Store 失败: {e}")))?;

    match path {
        Some(p) => {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                store.set(STORE_KEY_APP_CONFIG_DIR, Value::String(trimmed.to_string()));
                log::info!("已将 app_config_dir 写入 Store: {trimmed}");
            } else {
                store.delete(STORE_KEY_APP_CONFIG_DIR);
                log::info!("已从 Store 中删除 app_config_dir 配置");
            }
        }
        None => {
            store.delete(STORE_KEY_APP_CONFIG_DIR);
            log::info!("已从 Store 中删除 app_config_dir 配置");
        }
    }

    store
        .save()
        .map_err(|e| AppError::Message(format!("保存 Store 失败: {e}")))?;

    refresh_app_config_dir_override(app);
    Ok(())
}

/// 解析路径，支持 ~ 开头的相对路径
fn resolve_path(raw: &str) -> PathBuf {
    if raw == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(stripped) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    } else if let Some(stripped) = raw.strip_prefix("~\\") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }

    PathBuf::from(raw)
}

/// 从旧的 settings.json 迁移 app_config_dir 到 Store
pub fn migrate_app_config_dir_from_settings(app: &tauri::AppHandle) -> Result<(), AppError> {
    // app_config_dir 已从 settings.json 移除，此函数保留但不再执行迁移
    // 如果用户在旧版本设置过 app_config_dir，需要在 Store 中手动配置
    log::info!("app_config_dir 迁移功能已移除，请在设置中重新配置");

    let _ = refresh_app_config_dir_override(app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_store_path_is_absolute_and_stays_in_data() {
        let fixture = tempfile::tempdir().expect("创建测试目录");
        let data = fixture.path().join("便携版 with spaces").join("data");

        let path = store_path_for_data_dir(Some(&data));

        assert!(path.is_absolute());
        assert_eq!(path, data.join("app_paths.json"));
    }

    #[test]
    fn non_portable_store_path_uses_existing_tauri_resolver() {
        let path = store_path_for_data_dir(None);

        assert_eq!(path, PathBuf::from("app_paths.json"));
        assert!(path.is_relative());
    }

    #[test]
    fn portable_override_allows_current_directory_and_reset() {
        let fixture = tempfile::tempdir().expect("创建测试目录");
        let data = fixture.path().join("data");
        let current = data.to_str().expect("测试路径为 UTF-8");
        let padded = format!("  {current}  ");

        for path in [None, Some(""), Some("  "), Some(current), Some(padded.as_str())] {
            assert!(validate_portable_override(&data, path).is_ok());
        }
    }

    #[test]
    fn portable_override_rejects_other_directories() {
        let fixture = tempfile::tempdir().expect("创建测试目录");
        let data = fixture.path().join("data");
        let other = fixture.path().join("other");

        assert!(validate_portable_override(&data, other.to_str()).is_err());
        assert!(validate_portable_override(&data, Some("data")).is_err());
        assert!(validate_portable_override(&data, Some("~/.cc-switch")).is_err());
        assert!(!other.exists());
    }
}
