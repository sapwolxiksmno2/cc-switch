use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static PORTABLE_ROOT: OnceLock<Result<Option<PathBuf>, String>> = OnceLock::new();
static INITIALIZATION: OnceLock<Result<(), String>> = OnceLock::new();

/// 缓存 Portable 检测结果，错误不得转换成非 Portable 模式。
fn detected_root() -> Result<Option<&'static PathBuf>, String> {
    PORTABLE_ROOT
        .get_or_init(detect_root)
        .as_ref()
        .map(|root| root.as_ref())
        .map_err(|error| error.clone())
}

#[cfg(target_os = "windows")]
fn detect_root() -> Result<Option<PathBuf>, String> {
    detect_root_for_executable(std::env::current_exe())
}

#[cfg(not(target_os = "windows"))]
fn detect_root() -> Result<Option<PathBuf>, String> {
    Ok(None)
}

#[cfg(any(test, target_os = "windows"))]
fn detect_root_for_executable(executable: std::io::Result<PathBuf>) -> Result<Option<PathBuf>, String> {
    let executable = executable.map_err(|error| format!("获取当前可执行文件路径失败：{error}"))?;
    portable_root_for_exe(&executable)
}

#[cfg(any(test, target_os = "windows"))]
fn portable_root_for_exe(executable: &Path) -> Result<Option<PathBuf>, String> {
    let parent = executable
        .parent()
        .ok_or_else(|| format!("当前可执行文件没有父目录（{}）", executable.display()))?;
    root_from_marker_metadata(parent, std::fs::metadata(parent.join("portable.ini")))
}

#[cfg(any(test, target_os = "windows"))]
fn root_from_marker_metadata(
    parent: &Path,
    metadata: std::io::Result<std::fs::Metadata>,
) -> Result<Option<PathBuf>, String> {
    let marker = parent.join("portable.ini");
    match metadata {
        Ok(metadata) if metadata.is_file() => Ok(Some(parent.to_path_buf())),
        Ok(_) => Err(format!("Portable 标记不是普通文件（{}）", marker.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "读取 Portable 标记元数据失败（{}）：{error}",
            marker.display()
        )),
    }
}

fn data_dir_for_root(root: &Path) -> PathBuf {
    root.join("data")
}

fn temp_dir_for_root(root: &Path) -> PathBuf {
    data_dir_for_root(root).join("tmp")
}

/// 判断当前进程是否为 Portable 模式。
pub fn is_portable() -> bool {
    root_dir().is_some()
}

/// 获取 Portable 模式下可执行文件所在目录。
pub fn root_dir() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(root) = test_support::root_override() {
        return root;
    }
    // 启动入口负责返回初始化错误；绕过入口的调用也不能回退到用户目录。
    detected_root()
        .unwrap_or_else(|error| panic!("Portable 路径检测失败：{error}"))
        .cloned()
}

/// 获取 Portable 数据目录。
pub fn data_dir() -> Option<PathBuf> {
    root_dir().map(|root| data_dir_for_root(&root))
}

/// 获取 Portable 临时目录。
pub fn temp_dir() -> Option<PathBuf> {
    root_dir().map(|root| temp_dir_for_root(&root))
}

/// 创建临时文件；非 Portable 保留 tempfile 的默认目录选择。
pub fn create_temp_file() -> std::io::Result<tempfile::NamedTempFile> {
    match temp_dir() {
        Some(dir) => {
            std::fs::create_dir_all(&dir)?;
            tempfile::NamedTempFile::new_in(dir)
        }
        None => tempfile::NamedTempFile::new(),
    }
}

/// 创建独立临时目录；调用方继续持有 TempDir，保证失败时清理。
pub fn create_temp_dir(prefix: Option<&str>) -> std::io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    if let Some(prefix) = prefix {
        builder.prefix(prefix);
    }
    match temp_dir() {
        Some(dir) => {
            std::fs::create_dir_all(&dir)?;
            builder.tempdir_in(dir)
        }
        None => match prefix {
            Some(_) => builder.tempdir(),
            None => tempfile::tempdir(),
        },
    }
}

/// 获取可用临时根目录，用于原本明确指定 tempdir_in 的生产入口。
pub fn writable_temp_dir() -> std::io::Result<PathBuf> {
    match temp_dir() {
        Some(dir) => {
            std::fs::create_dir_all(&dir)?;
            Ok(dir)
        }
        None => Ok(std::env::temp_dir()),
    }
}

/// 终端在函数返回后才读文件，Portable 随机文件名避免并发覆盖和路径注入。
pub fn terminal_temp_path(default_name: &str, suffix: &str) -> std::io::Result<PathBuf> {
    let Some(dir) = temp_dir() else {
        return Ok(std::env::temp_dir().join(default_name));
    };
    std::fs::create_dir_all(&dir)?;
    let file = tempfile::Builder::new()
        .prefix("cc_switch_terminal_")
        .suffix(suffix)
        .tempfile_in(dir)?;
    file.into_temp_path().keep().map_err(|error| error.error)
}

/// 校验 SQL 导出的实际落点，拒绝相对路径、父目录跳转和链接越界。
pub fn checked_export_path(path: &Path) -> Result<PathBuf, String> {
    let Some(data) = data_dir() else {
        return Ok(path.to_path_buf());
    };
    let refused = || "Portable 模式只能将 SQL 备份导出到 data 目录内".to_string();
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        || !path.starts_with(&data)
        || path == data
    {
        return Err(refused());
    }
    let canonical_data = std::fs::canonicalize(&data).map_err(|_| refused())?;
    // 从最近的已有祖先解析链接；尚未创建的子目录也必须保持在 data 内。
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(existing.file_name().ok_or_else(refused)?.to_os_string());
                existing = existing.parent().ok_or_else(refused)?;
            }
            Err(_) => return Err(refused()),
        }
    }
    let mut resolved = std::fs::canonicalize(existing).map_err(|_| refused())?;
    if !resolved.starts_with(&canonical_data) {
        return Err(refused());
    }
    for component in missing.into_iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

/// Portable 不允许修改系统或用户全局安装状态。
pub fn reject_system_write(operation: &str) -> Result<(), String> {
    if is_portable() {
        return Err(format!("Portable 模式不支持{operation}"));
    }
    Ok(())
}

/// 在 Tauri 初始化路径前，仅创建一次 Portable 数据和临时目录。
pub fn initialize() -> Result<(), String> {
    INITIALIZATION
        .get_or_init(|| {
            let root = detected_root()?.map(|root| root.as_path());
            #[cfg(target_os = "windows")]
            {
                initialize_with_temp_environment(root, |name, path| {
                    std::env::set_var(name, path);
                })
            }
            #[cfg(not(target_os = "windows"))]
            {
                initialize_for_root(root)
            }
        })
        .clone()
}

#[cfg(any(test, target_os = "windows"))]
fn initialize_with_temp_environment(
    root: Option<&Path>,
    mut set_environment: impl FnMut(&str, &Path),
) -> Result<(), String> {
    initialize_for_root(root)?;
    if let Some(root) = root {
        // 注入写入方便于测试；生产只设置临时目录，不改变客户端配置根环境。
        let temp = temp_dir_for_root(root);
        set_environment("TEMP", &temp);
        set_environment("TMP", &temp);
    }
    Ok(())
}

fn initialize_for_root(root: Option<&Path>) -> Result<(), String> {
    let Some(root) = root else {
        return Ok(());
    };
    let data_dir = data_dir_for_root(root);
    let temp_dir = temp_dir_for_root(root);

    // 在任何创建动作前拒绝已有链接，防止沿 junction 写入 Portable 根之外。
    validate_directory_metadata(&data_dir)?;
    validate_directory_metadata(&temp_dir)?;
    validate_directory_metadata(&data_dir.join("webview2"))?;
    std::fs::create_dir_all(&data_dir).map_err(|error| {
        format!(
            "创建 Portable 数据目录失败（{}）：{error}",
            data_dir.display()
        )
    })?;
    validate_directory_metadata(&data_dir)?;
    validate_canonical_directory(root, &data_dir, Path::new("data"))?;
    std::fs::create_dir_all(&temp_dir).map_err(|error| {
        format!(
            "创建 Portable 临时目录失败（{}）：{error}",
            temp_dir.display()
        )
    })?;
    validate_directory_metadata(&temp_dir)?;
    validate_canonical_directory(
        root,
        &temp_dir,
        Path::new("data").join("tmp").as_path(),
    )?;
    Ok(())
}

fn validate_directory_metadata(path: &Path) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "检查 Portable 目录失败（{}）：{error}",
                path.display()
            ));
        }
    };
    #[cfg(target_os = "windows")]
    let is_link = {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    };
    #[cfg(not(target_os = "windows"))]
    let is_link = metadata.file_type().is_symlink();
    if is_link || !metadata.is_dir() {
        return Err(format!(
            "Portable 目录必须是普通目录，不能是链接或文件（{}）",
            path.display()
        ));
    }
    Ok(())
}

fn validate_canonical_directory(root: &Path, path: &Path, relative: &Path) -> Result<(), String> {
    let resolve = |path: &Path| {
        std::fs::canonicalize(path)
            .map_err(|error| format!("解析 Portable 目录失败（{}）：{error}", path.display()))
    };
    let expected = resolve(root)?.join(relative);
    if resolve(path)? != expected {
        return Err(format!(
            "Portable 目录超出可执行文件所在目录（{}）",
            path.display()
        ));
    }
    Ok(())
}

/// 测试通过线程局部值切换 Portable，避免修改进程环境或读取真实用户配置。
#[cfg(test)]
pub(crate) mod test_support {
    use std::cell::RefCell;
    use std::path::PathBuf;

    thread_local! {
        static ROOT_OVERRIDE: RefCell<Option<Option<PathBuf>>> = const { RefCell::new(None) };
    }

    pub(super) fn root_override() -> Option<Option<PathBuf>> {
        ROOT_OVERRIDE.with(|root| root.borrow().clone())
    }

    pub struct PortableTestRoot {
        previous: Option<Option<PathBuf>>,
        _fixture: tempfile::TempDir,
        root: PathBuf,
        _same_thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }

    impl PortableTestRoot {
        pub fn new() -> Self {
            let fixture = tempfile::tempdir().expect("创建 Portable 测试目录");
            let root = fixture.path().join("便携版 with spaces");
            // 只建立测试目录，不执行生产初始化中的进程环境变更。
            std::fs::create_dir_all(root.join("data/tmp")).expect("初始化 Portable 测试目录");
            let previous = ROOT_OVERRIDE.with(|value| value.replace(Some(Some(root.clone()))));
            Self {
                previous,
                _fixture: fixture,
                root,
                _same_thread: std::marker::PhantomData,
            }
        }

        pub fn data_dir(&self) -> PathBuf {
            self.root.join("data")
        }

        pub fn temp_dir(&self) -> PathBuf {
            self.data_dir().join("tmp")
        }
    }

    impl Drop for PortableTestRoot {
        fn drop(&mut self) {
            ROOT_OVERRIDE.with(|value| value.replace(self.previous.take()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::{self, ErrorKind};
    use tempfile::tempdir;

    #[test]
    fn portable_initialization_sets_only_temp_variables_after_directory_creation() {
        let fixture = tempdir().unwrap();
        let root = fixture.path().join("portable");
        let expected = root.join("data/tmp");
        let mut writes = Vec::new();
        initialize_with_temp_environment(Some(&root), |name, path| {
            assert!(path.is_dir());
            writes.push((name.to_string(), path.to_path_buf()));
        }).unwrap();
        assert_eq!(writes, vec![
            ("TEMP".to_string(), expected.clone()),
            ("TMP".to_string(), expected),
        ]);
        writes.clear();
        initialize_with_temp_environment(None, |name, path| {
            writes.push((name.to_string(), path.to_path_buf()));
        }).unwrap();
        assert!(writes.is_empty());

        let blocked = fixture.path().join("blocked");
        fs::create_dir(&blocked).unwrap();
        fs::write(blocked.join("data"), b"placeholder blocker").unwrap();
        assert!(initialize_with_temp_environment(Some(&blocked), |name, path| {
            writes.push((name.to_string(), path.to_path_buf()));
        }).is_err());
        assert!(writes.is_empty(), "初始化失败不能设置临时目录环境");
    }

    #[test]
    fn portable_initialization_accepts_normal_unicode_and_space_paths() {
        let fixture = tempdir().unwrap();
        let root = fixture.path().join("便携版本 with spaces");
        fs::create_dir(&root).unwrap();
        initialize_for_root(Some(&root)).unwrap();
        assert_eq!(fs::canonicalize(root.join("data")).unwrap(), fs::canonicalize(&root).unwrap().join("data"));
        assert_eq!(fs::canonicalize(root.join("data/tmp")).unwrap(), fs::canonicalize(&root).unwrap().join("data/tmp"));
    }

    #[cfg(any(unix, target_os = "windows"))]
    #[test]
    fn portable_initialization_rejects_linked_data_tmp_and_webview_before_external_writes() {
        let fixture = tempdir().unwrap();
        let external = tempdir().unwrap();
        for relative in ["data", "data/tmp", "data/webview2"] {
            let root = fixture.path().join(relative.replace('/', "-"));
            fs::create_dir_all(&root).unwrap();
            let link = relative.split('/').fold(root.clone(), |path, part| path.join(part));
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            #[cfg(unix)]
            std::os::unix::fs::symlink(external.path(), &link).unwrap();
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;

                // 按路径段构建 Windows 路径并显式引用，防止 mklink 将 /tmp 当作开关。
                let command = format!(
                    "mklink /J \"{}\" \"{}\"",
                    link.display(),
                    external.path().display()
                );
                let output = std::process::Command::new("cmd")
                    .args(["/D", "/C"])
                    .raw_arg(command)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "创建隔离的 junction 测试目录失败：{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let result = initialize_for_root(Some(&root));
            #[cfg(unix)]
            fs::remove_file(&link).unwrap();
            #[cfg(target_os = "windows")]
            fs::remove_dir(&link).unwrap();
            assert!(result.is_err(), "Portable 根链接必须拒绝");
            assert_eq!(fs::read_dir(external.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn portable_temporary_files_and_terminal_scripts_stay_in_data_tmp() {
        let portable = test_support::PortableTestRoot::new();
        let file = create_temp_file().unwrap();
        let dir = create_temp_dir(None).unwrap();
        let lifecycle = create_temp_dir(Some("cc_switch_lifecycle_")).unwrap();
        assert_eq!(file.path().parent().unwrap(), portable.temp_dir());
        assert_eq!(dir.path().parent().unwrap(), portable.temp_dir());
        assert_eq!(lifecycle.path().parent().unwrap(), portable.temp_dir());
        assert_eq!(writable_temp_dir().unwrap(), portable.temp_dir());
        let config = terminal_temp_path("placeholder.json", ".json").unwrap();
        let provider_bat = terminal_temp_path("placeholder.bat", ".bat").unwrap();
        let running_bat = terminal_temp_path("placeholder.bat", ".bat").unwrap();
        for path in [&config, &provider_bat, &running_bat] {
            assert_eq!(path.parent().unwrap(), portable.temp_dir());
            assert!(path.is_file());
        }
        assert_ne!(provider_bat, running_bat);
    }

    #[test]
    fn portable_export_paths_reject_external_and_parent_directory_targets() {
        let portable = test_support::PortableTestRoot::new();
        let target = portable.data_dir().join("exports/备份 with spaces.sql");
        let checked = checked_export_path(&target).unwrap();
        assert!(checked.starts_with(std::fs::canonicalize(portable.data_dir()).unwrap()));
        for path in [
            PathBuf::from("placeholder.sql"),
            portable.data_dir().join("../outside.sql"),
            portable.data_dir().with_file_name("data-other").join("outside.sql"),
            portable.data_dir(),
        ] {
            let error = checked_export_path(&path).unwrap_err();
            assert_eq!(error, "Portable 模式只能将 SQL 备份导出到 data 目录内");
        }
    }

    #[cfg(unix)]
    #[test]
    fn portable_export_rejects_existing_and_ancestor_symlinks_outside_data() {
        let portable = test_support::PortableTestRoot::new();
        let external = tempdir().unwrap();
        std::os::unix::fs::symlink(external.path(), portable.data_dir().join("linked")).unwrap();
        fs::write(external.path().join("existing.sql"), b"placeholder").unwrap();
        assert!(checked_export_path(&portable.data_dir().join("linked/new.sql")).is_err());
        assert!(checked_export_path(&portable.data_dir().join("linked/existing.sql")).is_err());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn portable_export_rejects_windows_junction_outside_data() {
        let portable = test_support::PortableTestRoot::new();
        let external = tempdir().unwrap();
        let link = portable.data_dir().join("linked");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(external.path())
            .output()
            .unwrap()
            .status;
        assert!(status.success(), "创建隔离的 junction 测试目录");
        let rejected = checked_export_path(&link.join("placeholder.sql"));
        fs::remove_dir(&link).unwrap();
        assert!(rejected.is_err());
        assert!(!external.path().join("placeholder.sql").exists());
    }

    #[test]
    fn marker_present_uses_executable_parent_as_portable_data_root() {
        let fixture = tempdir().expect("创建测试目录");
        let root = fixture.path().join("portable build");
        let executable = root.join("cc-switch.exe");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("portable.ini"), b"").unwrap();

        let detected = portable_root_for_exe(&executable).unwrap().unwrap();
        assert_eq!(detected, root);
        assert_eq!(data_dir_for_root(&detected), root.join("data"));
    }

    #[test]
    fn missing_marker_is_not_portable_and_creates_no_data() {
        let fixture = tempdir().expect("创建测试目录");
        let executable = fixture.path().join("cc-switch.exe");

        let root = portable_root_for_exe(&executable).unwrap();
        assert_eq!(root, None);
        initialize_for_root(root.as_deref()).unwrap();
        assert!(!fixture.path().join("data").exists());
    }

    #[test]
    fn marker_directory_is_an_error() {
        let fixture = tempdir().expect("创建测试目录");
        let marker = fixture.path().join("portable.ini");
        fs::create_dir(&marker).unwrap();

        let error = portable_root_for_exe(&fixture.path().join("cc-switch.exe")).unwrap_err();
        assert!(error.contains(&marker.display().to_string()));
        assert!(!fixture.path().join("data").exists());
    }

    #[test]
    fn metadata_errors_other_than_not_found_are_propagated() {
        let fixture = tempdir().expect("创建测试目录");
        let marker = fixture.path().join("portable.ini");

        for kind in [ErrorKind::PermissionDenied, ErrorKind::Other] {
            let error = root_from_marker_metadata(
                fixture.path(),
                Err(io::Error::new(kind, "测试元数据错误")),
            )
            .unwrap_err();
            assert!(error.contains(&marker.display().to_string()));
        }
    }

    #[test]
    fn current_exe_failure_is_an_error() {
        let error = detect_root_for_executable(Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "测试可执行文件路径错误",
        )))
        .unwrap_err();

        assert!(error.contains("测试可执行文件路径错误"));
    }

    #[test]
    fn spaces_and_unicode_paths_remain_absolute() {
        let fixture = tempdir().expect("创建测试目录");
        let root = fixture.path().join("发行版 with spaces");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("portable.ini"), b"").unwrap();

        let detected = portable_root_for_exe(&root.join("cc-switch.exe"))
            .unwrap()
            .unwrap();
        let data = data_dir_for_root(&detected);
        let temp = temp_dir_for_root(&detected);
        assert_eq!(detected, root);
        assert!(data.is_absolute());
        assert!(temp.is_absolute());
        assert_eq!(data, root.join("data"));
        assert_eq!(temp, root.join("data").join("tmp"));
    }

    #[test]
    fn initialization_creates_only_portable_data_and_temp() {
        let fixture = tempdir().expect("创建测试目录");
        fs::write(fixture.path().join("portable.ini"), b"").unwrap();
        let root = portable_root_for_exe(&fixture.path().join("cc-switch.exe"))
            .unwrap()
            .unwrap();

        initialize_for_root(Some(&root)).unwrap();
        initialize_for_root(Some(&root)).unwrap();
        assert!(fixture.path().join("data").is_dir());
        assert!(fixture.path().join("data").join("tmp").is_dir());
        assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 2);
        assert_eq!(fs::read_dir(fixture.path().join("data")).unwrap().count(), 1);
    }

    #[test]
    fn initialization_failure_does_not_use_or_migrate_old_home() {
        let fixture = tempdir().expect("创建测试目录");
        let root = fixture.path().join("portable");
        let old_dir = fixture.path().join("fake home").join(".cc-switch");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&old_dir).unwrap();
        let old_db = old_dir.join("cc-switch.db");
        fs::write(&old_db, b"test sentinel").unwrap();
        fs::write(root.join("portable.ini"), b"").unwrap();
        fs::write(root.join("data"), b"test blocker").unwrap();
        let detected = portable_root_for_exe(&root.join("cc-switch.exe"))
            .unwrap()
            .unwrap();

        assert_eq!(data_dir_for_root(&detected), root.join("data"));
        let error = initialize_for_root(Some(&detected)).unwrap_err();
        assert!(error.contains(&root.join("data").display().to_string()));
        assert_eq!(fs::read(&old_db).unwrap(), b"test sentinel");
        assert_eq!(fs::read_dir(&old_dir).unwrap().count(), 1);
        assert!(!root.join("cc-switch.db").exists());
    }

    #[test]
    fn temporary_directory_failure_is_propagated() {
        let fixture = tempdir().expect("创建测试目录");
        let temp = fixture.path().join("data").join("tmp");
        fs::create_dir_all(temp.parent().unwrap()).unwrap();
        fs::write(&temp, b"test blocker").unwrap();

        let error = initialize_for_root(Some(fixture.path())).unwrap_err();
        assert!(error.contains(&temp.display().to_string()));
        assert!(temp.is_file());
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn non_windows_build_is_non_portable() {
        assert!(!is_portable());
        assert_eq!(root_dir(), None);
        assert_eq!(data_dir(), None);
        assert_eq!(temp_dir(), None);
        assert_eq!(initialize(), Ok(()));
    }
}
