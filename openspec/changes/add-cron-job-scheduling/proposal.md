## Why

Users currently have no way to schedule recurring tasks from Discord. Users should be able to create, list, and remove cron jobs via natural language in a Discord thread. The AI agent handles intent recognition and calls a CLI tool (`openab-cron`) to manage jobs.

## What Changes

- Add `openab-cron` CLI binary for cron job CRUD (add/list/remove/config), sharing a JSON file with the main process
- Add a CronManager in the main openab process that watches the JSON file and schedules job execution
- CronManager uses `CronDelivery` trait for multi-source delivery (Discord now, extensible to Slack etc.)
- Expose cron tool to all agents via SKILL.md — each agent gets it in its native skill path
- Inject `OPENAB_CHANNEL_ID` env var (platform-prefixed, e.g. `discord:123`) when spawning agent process
- Add `[cron]` config section with timezone support
- Support three schedule types: fixed interval, cron expression (with timezone), one-time at specific time

## Capabilities

### New Capabilities
- `cron-scheduling`: CLI-based cron job management (`openab-cron add/list/remove/config`) with natural language interaction via AI agent
- `multi-agent-skill`: SKILL.md deployed to all 5 agent variants (Kiro, Claude, Codex, Gemini, Copilot)
- `timezone-support`: Per-job and global default timezone via `--tz` flag and `openab-cron config --tz`
- `multi-source-delivery`: CronDelivery trait allows plugging in new delivery backends beyond Discord
- `error-resilience`: Execution timeout, auto-pause on consecutive failures, delivery retry with backoff

### Modified Capabilities
- All Dockerfiles updated with SKILL.md copy, openab-cron binary, and CMD fix for clap subcommand

## Impact

- New binary: `src/bin/openab_cron.rs`
- New modules: `src/cron_manager.rs`, `src/cron_store.rs`
- Removed: `src/cron.rs` (hardcoded weather job)
- Removed: `skills/openab-cron/AGENTS.md` (replaced by SKILL.md)
- `src/acp/pool.rs` — inject `OPENAB_CHANNEL_ID` and `OPENAB_SOURCE` env on spawn
- `src/config.rs` — add `[cron]` config section with `default_tz`
- `src/main.rs` — wire up CronManager lifecycle, inject `OPENAB_DEFAULT_TZ`
- `Cargo.toml` — add chrono, cron, chrono-tz, async-trait, `[[bin]]` openab-cron
- All 5 Dockerfiles — SKILL.md paths, openab-cron binary, CMD fix
- `skills/openab-cron/SKILL.md` — agent skill definition
- `config.toml.example` — cron config section
- `charts/` — Helm values for cron
