pub mod proxy;
pub mod runner;

use std::env;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeBoundary {
    pub name: &'static str,
    pub ts_source: &'static str,
    pub goal: &'static str,
}

pub fn critical_boundaries() -> &'static [RuntimeBoundary] {
    &[
        RuntimeBoundary {
            name: "container-runtime",
            ts_source: "src/container-runtime.ts",
            goal: "Preserve host gateway, bind mount, and orphan cleanup behavior.",
        },
        RuntimeBoundary {
            name: "container-runner",
            ts_source: "src/container-runner.ts",
            goal: "Preserve mount isolation, session layout, and container invocation.",
        },
        RuntimeBoundary {
            name: "ipc",
            ts_source: "src/ipc.ts",
            goal: "Preserve group-scoped authorization and filesystem IPC semantics.",
        },
    ]
}

pub const CONTAINER_RUNTIME_BIN: &str = "docker";
pub const CONTAINER_HOST_GATEWAY: &str = "host.docker.internal";

pub fn detect_proxy_bind_host() -> String {
    if cfg!(target_os = "macos") {
        return "127.0.0.1".to_string();
    }

    if Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists() {
        return "127.0.0.1".to_string();
    }

    if let Ok(output) = std::process::Command::new("ip")
        .args(["-4", "addr", "show", "docker0"])
        .output()
    {
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Some(ip) = parse_docker0_ipv4(&stdout) {
                return ip;
            }
        }
    }

    "0.0.0.0".to_string()
}

pub fn proxy_bind_host() -> String {
    env::var("CREDENTIAL_PROXY_HOST").unwrap_or_else(|_| detect_proxy_bind_host())
}

pub fn host_gateway_args() -> Vec<String> {
    if cfg!(target_os = "linux") {
        vec!["--add-host=host.docker.internal:host-gateway".to_string()]
    } else {
        Vec::new()
    }
}

pub fn readonly_mount_args(host_path: &Path, container_path: &str) -> Vec<String> {
    vec![
        "-v".to_string(),
        format!("{}:{container_path}:ro", host_path.display()),
    ]
}

pub fn stop_container_command(name: &str) -> Vec<String> {
    vec![
        CONTAINER_RUNTIME_BIN.to_string(),
        "stop".to_string(),
        name.to_string(),
    ]
}

pub fn is_wsl() -> bool {
    fs::metadata("/proc/sys/fs/binfmt_misc/WSLInterop").is_ok()
}

fn parse_docker0_ipv4(output: &str) -> Option<String> {
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("inet ") {
            let cidr = rest.split_whitespace().next()?;
            let ip = cidr.split('/').next()?;
            return Some(ip.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn readonly_mounts_match_expected_cli_shape() {
        let args = readonly_mount_args(&PathBuf::from("/tmp/source"), "/workspace/group");
        assert_eq!(args, vec!["-v", "/tmp/source:/workspace/group:ro"]);
    }

    #[test]
    fn stop_command_uses_runtime_bin() {
        assert_eq!(
            stop_container_command("nanoclaw-test"),
            vec!["docker", "stop", "nanoclaw-test"]
        );
    }

    #[test]
    fn parses_docker0_output() {
        let sample = "2: docker0: <BROADCAST>\n    inet 172.17.0.1/16 brd 172.17.255.255";
        assert_eq!(parse_docker0_ipv4(sample).as_deref(), Some("172.17.0.1"));
    }
}
