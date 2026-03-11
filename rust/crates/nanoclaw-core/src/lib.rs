pub mod config;
pub mod env;
pub mod group_folder;
pub mod mount_security;
pub mod router;
pub mod scheduler;
pub mod sender_allowlist;
pub mod timezone;
pub mod types;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationPhase {
    Scaffolded,
    CoreParity,
    HostParity,
    SetupParity,
    RunnerParity,
    DirectRewrite,
}

impl MigrationPhase {
    pub fn description(self) -> &'static str {
        match self {
            Self::Scaffolded => "Rust workspace exists and compiles.",
            Self::CoreParity => "Shared types, config, formatting, and validation are ported.",
            Self::HostParity => "Message loop, scheduler, IPC, and credential proxy are ported.",
            Self::SetupParity => "Bootstrap and service-management commands are ported.",
            Self::RunnerParity => "Container-side runner behavior is ported into Rust.",
            Self::DirectRewrite => "Rust is the primary implementation for the repository.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortingTarget {
    pub ts_area: &'static str,
    pub rust_area: &'static str,
    pub summary: &'static str,
}

pub fn default_porting_targets() -> &'static [PortingTarget] {
    &[
        PortingTarget {
            ts_area: "src/config.ts, src/types.ts, src/timezone.ts",
            rust_area: "nanoclaw-core",
            summary: "Shared configuration, domain types, and time helpers.",
        },
        PortingTarget {
            ts_area: "src/db.ts",
            rust_area: "nanoclaw-db",
            summary: "SQLite schema, migrations, and persistence accessors.",
        },
        PortingTarget {
            ts_area: "src/container-runtime.ts, src/container-runner.ts, src/ipc.ts",
            rust_area: "nanoclaw-runtime",
            summary: "Container lifecycle, IPC contract, and host boundaries.",
        },
        PortingTarget {
            ts_area: "src/index.ts, src/router.ts, src/group-queue.ts, src/task-scheduler.ts",
            rust_area: "apps/nanoclaw-host",
            summary: "Host orchestrator, routing, concurrency, and scheduling.",
        },
        PortingTarget {
            ts_area: "setup/*.ts",
            rust_area: "apps/nanoclaw-setup",
            summary: "Bootstrap flow, environment validation, and service management.",
        },
        PortingTarget {
            ts_area: "container/agent-runner/src/*.ts",
            rust_area: "apps/nanoclaw-runner",
            summary: "Container-side runner rewritten directly in Rust.",
        },
    ]
}
