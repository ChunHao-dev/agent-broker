## 1. Config & Data Model

- [x] 1.1 Add `[cron]` section to `Config` in `src/config.rs` (enabled, data_file, default_tz)
- [x] 1.2 Create `src/cron_store.rs` with `CronJob`, `CronStore`, `Schedule` (Every/Cron/At), timezone support

## 2. CLI Binary

- [x] 2.1 Add chrono, cron, chrono-tz, async-trait dependencies and `[[bin]]` for `openab-cron`
- [x] 2.2 Create `src/bin/openab_cron.rs` with `add`, `list`, `remove`, `config` subcommands
- [x] 2.3 Make `--thread` optional, fallback to `OPENAB_CHANNEL_ID` env var
- [x] 2.4 Add timezone support: `--tz` flag, `openab-cron config --tz`, priority chain (flag > env > store > UTC)
- [x] 2.5 Support three schedule types: `--interval`, `--cron`, `--at`

## 3. CronManager

- [x] 3.1 Create `src/cron_manager.rs` with `CronDelivery` trait for multi-source support
- [x] 3.2 Implement `DiscordDelivery` as first delivery backend
- [x] 3.3 Independent ACP session execution per job (not from pool)
- [x] 3.4 Support one-time jobs (auto-remove after execution)
- [x] 3.5 Auto-remove jobs when channel is gone

## 4. Integration

- [x] 4.1 Inject platform-prefixed `OPENAB_CHANNEL_ID` (e.g. `discord:123`) in `src/acp/pool.rs` on spawn, parsed from ChatAdapter thread_key
- [x] 4.2 Inject `OPENAB_DEFAULT_TZ` from config into agent env in `src/main.rs`
- [x] 4.3 Create SKILL.md at `skills/openab-cron/SKILL.md`
- [x] 4.4 Update all Dockerfiles to copy SKILL.md to agent-specific paths:
  - Kiro: `$HOME/.kiro/skills/`
  - Claude: `$HOME/.claude/skills/`
  - Codex/Gemini: `$HOME/.agents/skills/`
  - Copilot: `$HOME/.copilot/skills/`
- [x] 4.5 Fix CMD in all Dockerfiles for clap subcommand (`run`)
- [x] 4.6 Remove hardcoded weather job (`src/cron.rs`)
- [x] 4.7 Remove redundant AGENTS.md (replaced by SKILL.md)
- [x] 4.8 Build, deploy, verify

## 5. Error Handling

- [ ] 5.1 Add execution timeout (5 min) to `execute_prompt()` — kill agent process on timeout
- [ ] 5.2 Track consecutive failure count per job, auto-pause after 3 failures, notify user
- [ ] 5.3 Add retry with backoff for delivery send failures (1 retry, 5s delay)
- [ ] 5.4 Classify error messages: spawn failure vs agent error vs delivery failure
- [ ] 5.5 Show job health status in `openab-cron list` (last run status, failure count)
- [ ] 5.6 Add exponential backoff for CronManager sync failures (30s → 60s → 120s)
