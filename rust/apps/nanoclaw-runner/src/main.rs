use std::io;
use std::path::Path;

use nanoclaw_runner::{ContainerOutput, RunnerStatus, drain_ipc_input, read_container_input, write_output};

fn main() {
    let stdin = io::stdin();
    let mut handle = stdin.lock();

    match read_container_input(&mut handle) {
        Ok(input) => {
            let pending = drain_ipc_input(Path::new("/workspace/ipc/input"));
            let result = ContainerOutput {
                status: RunnerStatus::Error,
                result: None,
                new_session_id: input.session_id,
                error: Some(format!(
                    "Rust runner protocol initialized for group {} ({} pending IPC messages). Claude SDK integration is not implemented yet.",
                    input.group_folder,
                    pending.len()
                )),
            };
            let _ = write_output(&mut io::stdout(), &result);
        }
        Err(error) => {
            let result = ContainerOutput {
                status: RunnerStatus::Error,
                result: None,
                new_session_id: None,
                error: Some(format!("Failed to parse input: {error}")),
            };
            let _ = write_output(&mut io::stdout(), &result);
            std::process::exit(1);
        }
    }
}
