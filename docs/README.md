# jcode Docs

Reference documentation for the jcode codebase.

> **This is the Android/Termux fork** ([SehonYT/jcode-termux](https://github.com/SehonYT/jcode-termux),
> maintained by [@SehonYT](https://github.com/SehonYT)). Android-specific documentation lives in
> [`docs/android/`](android/README.md) — start there for anything platform-related.

## Layout

- `docs/*.md` — architecture, feature, and behavior docs (current state of the system).
- `docs/plans/` — forward-looking plans, roadmaps, and TODO trackers. May be partially implemented or stale.
- `docs/audits/` — point-in-time audits and reviews. Historical snapshots, not kept up to date.
- `docs/proposals/` — design proposals not yet committed to.
- `docs/dev/` — developer-facing process and testing notes.

## Key entry points

- Architecture: `SERVER_ARCHITECTURE.md`, `MODULAR_ARCHITECTURE_RFC.md`, `CRATE_OWNERSHIP_BOUNDARIES.md`
- Swarm: `SWARM_ARCHITECTURE.md`
- Memory: `MEMORY_ARCHITECTURE.md`, `MEMORY_BUDGET.md`, `MEMORY_INCIDENT_RUNBOOK.md`
- Refactoring and quality: `REFACTORING.md`, `plans/CODE_QUALITY_10_10_PLAN.md`
- Desktop app: `DESKTOP_APP_ARCHITECTURE.md`, `DESKTOP_CODEBASE_ARCHITECTURE.md`
- Providers: `PROVIDER_DOCTOR.md`, `AWS_BEDROCK_PROVIDER.md`
- MCP servers: `MCP_REMOTE.md`
- Platform: `WINDOWS.md`, `TERMINAL_CAPABILITIES.md`
- Android/Termux: [`android/`](android/README.md) — install, build matrix, hardening, tests

## Conventions

- Docs describing current behavior live at the top level; anything speculative goes in `plans/` or `proposals/`.
- Prefer updating an existing doc over adding a near-duplicate.
- Root of the repo should only hold README, CONTRIBUTING, RELEASING, AGENTS, LICENSE, and similar meta files. Put everything else here.
