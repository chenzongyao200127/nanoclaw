use std::env;
use std::fs;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Macos,
    Linux,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceManager {
    Launchd,
    Systemd,
    None,
}

pub fn get_platform() -> Platform {
    match env::consts::OS {
        "macos" => Platform::Macos,
        "linux" => Platform::Linux,
        _ => Platform::Unknown,
    }
}

pub fn is_wsl() -> bool {
    if get_platform() != Platform::Linux {
        return false;
    }
    fs::read_to_string("/proc/version")
        .map(|release| {
            let release = release.to_lowercase();
            release.contains("microsoft") || release.contains("wsl")
        })
        .unwrap_or(false)
}

pub fn is_root() -> bool {
    nixless_getuid().is_some_and(|uid| uid == 0)
}

pub fn is_headless() -> bool {
    if get_platform() == Platform::Linux {
        env::var_os("DISPLAY").is_none() && env::var_os("WAYLAND_DISPLAY").is_none()
    } else {
        false
    }
}

pub fn has_systemd() -> bool {
    if get_platform() != Platform::Linux {
        return false;
    }
    fs::read_to_string("/proc/1/comm")
        .map(|init| init.trim() == "systemd")
        .unwrap_or(false)
}

pub fn get_service_manager() -> ServiceManager {
    match get_platform() {
        Platform::Macos => ServiceManager::Launchd,
        Platform::Linux if has_systemd() => ServiceManager::Systemd,
        _ => ServiceManager::None,
    }
}

pub fn command_exists(name: &str) -> bool {
    Command::new("sh")
        .args(["-lc", &format!("command -v {name}")])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

pub fn get_node_version() -> Option<String> {
    let output = Command::new("node").arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8(output.stdout).ok()?;
    Some(version.trim().trim_start_matches('v').to_string())
}

pub fn get_node_major_version() -> Option<u32> {
    let version = get_node_version()?;
    version.split('.').next()?.parse::<u32>().ok()
}

fn nixless_getuid() -> Option<u32> {
    let output = Command::new("id").arg("-u").output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()?.trim().parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_is_valid() {
        assert!(matches!(get_platform(), Platform::Macos | Platform::Linux | Platform::Unknown));
    }

    #[test]
    fn boolean_detectors_do_not_panic() {
        let _ = is_wsl();
        let _ = is_root();
        let _ = is_headless();
        let _ = has_systemd();
    }

    #[test]
    fn service_manager_matches_platform() {
        let platform = get_platform();
        let manager = get_service_manager();
        match platform {
            Platform::Macos => assert_eq!(manager, ServiceManager::Launchd),
            Platform::Linux => assert!(matches!(manager, ServiceManager::Systemd | ServiceManager::None)),
            Platform::Unknown => assert_eq!(manager, ServiceManager::None),
        }
    }

    #[test]
    fn command_exists_returns_bool() {
        assert!(command_exists("sh"));
        assert!(!command_exists("this_command_does_not_exist_xyz_123"));
    }

    #[test]
    fn node_version_if_present_is_parseable() {
        if let Some(version) = get_node_version() {
            assert!(version.chars().next().is_some_and(|ch| ch.is_ascii_digit()));
        }
    }
}
