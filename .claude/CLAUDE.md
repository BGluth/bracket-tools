# bracket-tools

Rust mono-repo for esports tournament tooling targeting the start.gg platform, built by Brendan (Alberta Smash TO / player). Public goal: an SDK for querying start.gg with built-in caching and rate limiting. Internal tools: a multi-bracket scheduler for the TO desk, a registration-admin CLI, a set-reporter TUI (early), and a scraper daemon (skeleton).

## Crate Map

| Crate | Directory | Purpose |
|---|---|---|
| bracket-tools-core | crates/bracket-tools-core | Normalized data types and traits |
| bracket-tools-cache | crates/bracket-tools-cache | Async `Storage` trait + Null / Sled / Memory backends |
| bracket-tools-query | crates/bracket-tools-query | Abstract multi-platform query interface (skeleton) |
| bracket-tools-startgg-schema | crates/bracket-tools-startgg-schema | cynic codegen types from the start.gg schema |
| bracket-tools-startgg | crates/bracket-tools-startgg | Main SDK: caching, rate-limited start.gg client |
| bracket-tools-scheduler-core | tools/scheduler/core | Scheduler core: bracket model, scheduling, Elm loop, poll/write loops (also builds for wasm32) |
| bracket-tools-scheduler | tools/scheduler/tui | `scheduler` TUI for the TO desk over the core (runs live at events) |
| bracket-tools-scheduler-web | tools/scheduler/web | Scheduler browser/desktop UI: Dioxus components over the core (`SchedulerTool`, `DemoTool`), mounted by the app |
| bracket-tools-app | tools/app | Dioxus site shell: router, home, settings (start.gg token in localStorage); mounts the tool crates as routes (`dx serve` in that dir) |
| bracket-tools-admin | tools/admin | `gg-admin` CLI: registration admin (roster / add / find / pool) |
| reporter-cli, reporter-state | tools/reporter/* | ratatui set-reporting TUI + its store layer (early) |
| bracket-tools-daemon | tools/daemon | Background scraper daemon (skeleton) |
| edmonton-smash | web/edmonton-smash | Leptos community site (not in the workspace yet) |

Per-crate file maps and design rationale live in the project's auto-memory as cold `architecture_*` files (`architecture_sdk`, `architecture_scheduler`, `architecture_app`, `architecture_admin_tool`, `architecture_repo_layout`). Grep the crate name there before orienting in a crate; there is no in-repo codebase map.

## Where state lives

Progress, plans, and next steps live only in the project's auto-memory (`MEMORY.md` Active Work → `memory/tasks/`, `memory/epics/`). This file carries no progress log and no status percentages; do not add them here.

## Session Workflow

- **Start:** read MEMORY.md Active Work and the linked task / epic files. If the user hasn't named a task, list the in-flight entries plus the planned backlog (`memory/tasks/` with status `planned`) and ask what to work on.
- **Code changes** run in a `worktrees/<slug>` git worktree off `origin/main`, driven from the repo root (`git -C`, `--manifest-path`); the protocol is the `feedback_worktree_protocol` memory. Never edit the repo root directly.
- **Landing:** this project uses no pull requests. Commit (SSH-signed), fetch + rebase on `origin/main`, re-run tests, fast-forward push to `main`, remove the worktree. Never create or offer a PR.
- **Wrap-up:** `/handoff` for the task (and `/epic update` when an epic is involved); archive landed tasks in the same session. After landing, ask whether to pick up another task or wrap the session.

## Key Technical Decisions

- Sled for the disk cache; lazy hydration in the SDK layer, not core; reqwest for HTTP; the multi-platform query abstraction stays; the scheduler is its own crate polling uncached full snapshots on `NullStorage`. Rationale: the `architecture_sdk` / `architecture_scheduler` memory files.
- Nightly Rust is acceptable.

## Conventions

- Rust style: `~/.claude/rules/rust_files.md` (global).
- Sensitive data: live captures and autoplay replays carry real player names and never enter the repo (`*-replay.txt` is gitignored; captures live in `~/work/personal/bracket-tools-captures/`). The repo is public.

## Docs

- `docs/scheduler-ops.md` — the TO-desk runbook for the scheduler (user-facing; keep it current when scheduler behavior changes).
- `tools/admin/README.md` — gg-admin usage and the start.gg public-API limits it works around.
