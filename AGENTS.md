# Repository Guidelines

## Project Structure & Module Organization

`src/` contains the main NanoClaw runtime: routing, channels, container execution, scheduling, and SQLite-backed state. `setup/` holds the bootstrap and service-registration flow, with tests beside the implementation. `container/agent-runner/` is the container-side TypeScript package used by the runtime. Supporting material lives in `docs/`, `assets/`, `config-examples/`, and `repo-tokens/`. Group-specific memory and state are organized under `groups/*/CLAUDE.md`.

## Build, Test, and Development Commands

- `npm install` installs root dependencies.
- `npm run dev` starts the app from source with `tsx`.
- `npm run build` compiles the root package to `dist/`.
- `npm run start` runs the compiled build.
- `npm run setup` executes the local setup flow.
- `npm run typecheck` runs strict TypeScript checks without emitting files.
- `npm test` runs Vitest once for `src/**/*.test.ts` and `setup/**/*.test.ts`.
- `npm run test:watch` runs Vitest in watch mode.
- `npm run format:check` verifies formatting; `npm run format` rewrites files.
- `npm --prefix container/agent-runner run build` builds the container runner package.

## Coding Style & Naming Conventions

This repo uses TypeScript ESM with strict compiler settings. Follow the existing style: 2-space indentation, single quotes, and `.js` import suffixes in TS source. Keep filenames kebab-case such as `container-runner.ts`; use `camelCase` for functions and variables, and `PascalCase` for types or classes. Prefer small modules with tests near the code they validate.

## Testing Guidelines

Vitest is the test framework. Name tests `*.test.ts` and keep them in the same top-level area as the code under test, for example `src/router.test.ts` for `src/router.ts`. Add tests for bug fixes and behavior changes, including failure paths around routing, persistence, scheduling, and container boundaries. Before opening a PR, run `npm test` and `npm run typecheck`.

## Commit & Pull Request Guidelines

Recent history follows Conventional Commits (`fix:`, `feat:`, `docs:`, `ci:`, `chore:`). Use short, imperative subjects and add a scope when it clarifies the change. Per `CONTRIBUTING.md`, core PRs should stay limited to bug fixes, security fixes, and simplifications; new capabilities should usually ship as Claude Code skills instead of core features. PRs should summarize user impact, note validation commands, and link related issues when applicable.

## Security & Contribution Model

NanoClaw is intentionally minimal and container-isolated. Preserve mount and runtime safety assumptions, avoid committing secrets, and use `config-examples/` for templates. If you are adding a new integration or workflow, prefer a skill-based contribution over expanding the base runtime.
