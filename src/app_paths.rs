use anyhow::{Context, Result};
use std::path::PathBuf;

pub fn data_dir() -> Result<PathBuf> {
    let path = resolve_data_dir(
        portable_data_dir_from_exe(),
        dirs::data_local_dir(),
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        std::env::current_dir().ok(),
        std::env::temp_dir(),
    );
    ensure_dir(&path)
}

pub fn settings_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("settings.json"))
}

pub fn database_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("soziopolis_lingq_tool.db"))
}

pub fn logs_dir() -> Result<PathBuf> {
    ensure_dir(&data_dir()?.join("logs"))
}

pub fn app_log_path() -> Result<PathBuf> {
    Ok(logs_dir()?.join("soziopolis-reader.log"))
}

pub fn support_bundles_dir() -> Result<PathBuf> {
    ensure_dir(&data_dir()?.join("support_bundles"))
}

pub fn browse_cache_dir() -> Result<PathBuf> {
    ensure_dir(&data_dir()?.join("browse_cache"))
}

fn portable_data_dir_from_exe() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    portable_data_dir_from_exe_dir(&exe_dir)
}

fn portable_data_dir_from_exe_dir(exe_dir: &std::path::Path) -> Option<PathBuf> {
    for folder_name in ["data", "portable_data"] {
        let candidate = exe_dir.join(folder_name);
        if candidate.is_dir() {
            return Some(candidate.join("soziopolis_lingq_tool"));
        }
    }
    None
}

fn resolve_data_dir(
    portable_dir: Option<PathBuf>,
    platform_dir: Option<PathBuf>,
    env_dir: Option<PathBuf>,
    cwd: Option<PathBuf>,
    temp_dir: PathBuf,
) -> PathBuf {
    if let Some(path) = portable_dir {
        return path;
    }

    let mut base_dir = resolve_base_data_root(platform_dir, env_dir, cwd, temp_dir);
    base_dir.push("soziopolis_lingq_tool");
    base_dir
}

fn resolve_base_data_root(
    platform_dir: Option<PathBuf>,
    env_dir: Option<PathBuf>,
    cwd: Option<PathBuf>,
    temp_dir: PathBuf,
) -> PathBuf {
    platform_dir.or(env_dir).or(cwd).unwrap_or(temp_dir)
}

fn ensure_dir(path: &PathBuf) -> Result<PathBuf> {
    std::fs::create_dir_all(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    Ok(path.clone())
}

#[cfg(test)]
mod tests {
    use super::{portable_data_dir_from_exe_dir, resolve_base_data_root, resolve_data_dir};
    use std::{
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn unique_temp_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}_{unique}"))
    }

    #[test]
    fn resolve_base_data_root_prefers_platform_dir() {
        let platform = PathBuf::from(r"C:\Users\Alice\AppData\Local");
        let env_dir = PathBuf::from(r"D:\Fallback");
        let cwd = PathBuf::from(r"E:\Workspace");

        let resolved = resolve_base_data_root(
            Some(platform.clone()),
            Some(env_dir),
            Some(cwd),
            PathBuf::from(r"F:\Temp"),
        );

        assert_eq!(resolved, platform);
    }

    #[test]
    fn resolve_base_data_root_falls_back_without_hardcoded_user_path() {
        let env_dir = PathBuf::from(r"D:\LocalAppData");
        let cwd = PathBuf::from(r"E:\Workspace");

        let resolved = resolve_base_data_root(
            None,
            Some(env_dir.clone()),
            Some(cwd),
            PathBuf::from(r"F:\Temp"),
        );

        assert_eq!(resolved, env_dir);
        assert!(!resolved.to_string_lossy().contains(r"\Users\Admin\"));
    }

    #[test]
    fn portable_data_layout_beside_executable_wins_over_local_app_data() {
        let exe_dir = unique_temp_dir("soziopolis_portable_data_layout");
        let local_app_data = exe_dir.join("LocalAppData");
        std::fs::create_dir_all(exe_dir.join("data")).expect("portable data dir");
        std::fs::create_dir_all(&local_app_data).expect("local app data dir");

        let resolved = resolve_data_dir(
            portable_data_dir_from_exe_dir(&exe_dir),
            None,
            Some(local_app_data.clone()),
            None,
            exe_dir.join("Temp"),
        );

        assert_eq!(resolved, exe_dir.join("data").join("soziopolis_lingq_tool"));
        assert!(!resolved.starts_with(local_app_data));

        let _ = std::fs::remove_dir_all(exe_dir);
    }

    #[test]
    fn portable_data_layout_supports_legacy_portable_data_folder() {
        let exe_dir = unique_temp_dir("soziopolis_portable_legacy_layout");
        std::fs::create_dir_all(exe_dir.join("portable_data")).expect("portable_data dir");

        let resolved = resolve_data_dir(
            portable_data_dir_from_exe_dir(&exe_dir),
            None,
            Some(PathBuf::from(r"D:\LocalAppData")),
            None,
            exe_dir.join("Temp"),
        );

        assert_eq!(
            resolved,
            exe_dir.join("portable_data").join("soziopolis_lingq_tool")
        );

        let _ = std::fs::remove_dir_all(exe_dir);
    }

    #[test]
    fn portable_data_layout_prefers_data_when_both_portable_folders_exist() {
        let exe_dir = unique_temp_dir("soziopolis_portable_both_layouts");
        std::fs::create_dir_all(exe_dir.join("data")).expect("data dir");
        std::fs::create_dir_all(exe_dir.join("portable_data")).expect("portable_data dir");

        let resolved = portable_data_dir_from_exe_dir(&exe_dir).expect("portable layout");

        assert_eq!(resolved, exe_dir.join("data").join("soziopolis_lingq_tool"));

        let _ = std::fs::remove_dir_all(exe_dir);
    }
}
