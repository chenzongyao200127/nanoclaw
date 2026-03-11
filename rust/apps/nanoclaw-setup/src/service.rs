use std::path::Path;

use crate::platform::ServiceManager;

pub fn generate_plist(node_path: &str, project_root: &Path, home_dir: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.nanoclaw</string>
    <key>ProgramArguments</key>
    <array>
        <string>{node_path}</string>
        <string>{}/dist/index.js</string>
    </array>
    <key>WorkingDirectory</key>
    <string>{}</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>/usr/local/bin:/usr/bin:/bin:{}/.local/bin</string>
        <key>HOME</key>
        <string>{}</string>
    </dict>
    <key>StandardOutPath</key>
    <string>{}/logs/nanoclaw.log</string>
    <key>StandardErrorPath</key>
    <string>{}/logs/nanoclaw.error.log</string>
</dict>
</plist>"#,
        project_root.display(),
        project_root.display(),
        home_dir.display(),
        home_dir.display(),
        project_root.display(),
        project_root.display(),
    )
}

pub fn generate_systemd_unit(
    node_path: &str,
    project_root: &Path,
    home_dir: &Path,
    is_system: bool,
) -> String {
    format!(
        r#"[Unit]
Description=NanoClaw Personal Assistant
After=network.target

[Service]
Type=simple
ExecStart={node_path} {}/dist/index.js
WorkingDirectory={}
Restart=always
RestartSec=5
Environment=HOME={}
Environment=PATH=/usr/local/bin:/usr/bin:/bin:{}/.local/bin
StandardOutput=append:{}/logs/nanoclaw.log
StandardError=append:{}/logs/nanoclaw.error.log

[Install]
WantedBy={}"#,
        project_root.display(),
        project_root.display(),
        home_dir.display(),
        home_dir.display(),
        project_root.display(),
        project_root.display(),
        if is_system { "multi-user.target" } else { "default.target" }
    )
}

pub fn generate_nohup_wrapper(node_path: &str, project_root: &Path) -> String {
    let pid_file = project_root.join("nanoclaw.pid");
    format!(
        r#"#!/bin/bash
set -euo pipefail
cd "{}"
nohup "{}" "{}/dist/index.js" >> "{}/logs/nanoclaw.log" 2>> "{}/logs/nanoclaw.error.log" &
echo $! > "{}""#,
        project_root.display(),
        node_path,
        project_root.display(),
        project_root.display(),
        project_root.display(),
        pid_file.display(),
    )
}

pub fn service_manager_name(manager: ServiceManager) -> &'static str {
    match manager {
        ServiceManager::Launchd => "launchd",
        ServiceManager::Systemd => "systemd",
        ServiceManager::None => "none",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn plist_contains_expected_fields() {
        let plist = generate_plist(
            "/usr/local/bin/node",
            &PathBuf::from("/home/user/nanoclaw"),
            &PathBuf::from("/home/user"),
        );
        assert!(plist.contains("com.nanoclaw"));
        assert!(plist.contains("/home/user/nanoclaw/dist/index.js"));
        assert!(plist.contains("nanoclaw.error.log"));
    }

    #[test]
    fn systemd_unit_uses_correct_target() {
        let unit = generate_systemd_unit(
            "/usr/bin/node",
            &PathBuf::from("/srv/nanoclaw"),
            &PathBuf::from("/home/user"),
            true,
        );
        assert!(unit.contains("WantedBy=multi-user.target"));
        assert!(unit.contains("ExecStart=/usr/bin/node /srv/nanoclaw/dist/index.js"));
    }

    #[test]
    fn nohup_wrapper_contains_pid_file() {
        let wrapper = generate_nohup_wrapper(
            "/usr/bin/node",
            &PathBuf::from("/home/user/nanoclaw"),
        );
        assert!(wrapper.contains("#!/bin/bash"));
        assert!(wrapper.contains("nohup"));
        assert!(wrapper.contains("nanoclaw.pid"));
    }
}
