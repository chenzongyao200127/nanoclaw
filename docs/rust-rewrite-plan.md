# Rust Rewrite Plan

## Goal

Rewrite NanoClaw in Rust as the primary implementation. The TypeScript code remains useful only as a source reference while the Rust codebase grows.

## Workspace Layout

- `rust/crates/nanoclaw-core`: shared types, config loading, path logic, timezone helpers, mount validation, and formatting utilities.
- `rust/crates/nanoclaw-db`: SQLite schema, queries, and persistence for chats, messages, sessions, groups, and tasks.
- `rust/crates/nanoclaw-runtime`: container runtime behavior, bind-mount assembly, credential proxy support, and IPC boundaries.
- `rust/apps/nanoclaw-host`: the main orchestrator that replaces `src/index.ts`.
- `rust/apps/nanoclaw-setup`: bootstrap and service-management commands that replace `setup/`.
- `rust/apps/nanoclaw-runner`: the container-side agent process that replaces `container/agent-runner/`.

## Rewrite Order

1. Finish core types and config.
2. Finish database and schema logic.
3. Build the host runtime: polling, routing, queueing, scheduler, and IPC.
4. Port setup commands.
5. Replace the container-side runner.

## Non-Negotiable Behavior

- Preserve the security model documented in `docs/SECURITY.md`.
- Preserve the SQLite table layout and stored semantics unless a deliberate Rust-native redesign is made.
- Preserve per-group isolation, readonly project mounts, and credential proxy behavior.

## Immediate Next Work

- Port `src/router.ts` and related formatting helpers into `nanoclaw-core`.
- Add Rust-side container invocation code to `nanoclaw-runtime`.
- Move the polling loop and scheduler from `src/index.ts` and `src/task-scheduler.ts` into `nanoclaw-host`.
