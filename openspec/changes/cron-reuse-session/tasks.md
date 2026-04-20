## 1. Data Model

- [ ] 1.1 Add `CronMode` enum (`Isolated`, `Session`) to `src/cron_store.rs`, serialized as lowercase strings, default `Isolated`
- [ ] 1.2 Add `mode: CronMode` field to `CronJob` with `#[serde(default)]`
- [ ] 1.3 Add `heartbeat_file: Option<String>` field to `CronJob` with `#[serde(default)]`
- [ ] 1.4 Add test: old JSON without `mode` and `heartbeat_file` deserializes correctly

## 2. CLI

- [ ] 2.1 Add `--mode` option to `Add` subcommand (`isolated`/`session`, default `isolated`)
- [ ] 2.2 Add `--file` option to `Add` subcommand (path to heartbeat markdown file)
- [ ] 2.3 Validate: `--mode session` requires `--prompt` or `--file`; `--file` requires `--mode session`
- [ ] 2.4 Pass `mode` and `heartbeat_file` into `CronJob` on creation
- [ ] 2.5 Show mode in `list` output (e.g. `session`, `session:HEARTBEAT.md`)

## 3. Heartbeat File Support

- [ ] 3.1 Resolve `--file` relative path against data directory (same parent as `cron_jobs.json`)
- [ ] 3.2 In `spawn_job`, if `heartbeat_file` is set, read file content at execution time as the prompt
- [ ] 3.3 If heartbeat file doesn't exist or is empty, skip execution and log warning (don't count as failure)
- [ ] 3.4 Add test: heartbeat file content used as prompt
- [ ] 3.5 Add test: missing/empty heartbeat file skips execution

## 4. Pool-Aware Executor

- [ ] 4.1 Add `PoolPromptExecutor` struct in `src/cron_manager.rs` holding `Arc<SessionPool>` + fallback `AcpExecutor`
- [ ] 4.2 Implement `PromptExecutor` for `PoolPromptExecutor`: try `pool.with_connection` using session key, fallback to `AcpExecutor::execute` on error
- [ ] 4.3 Session key = `format!("{}:{}", target.source, target.channel_id)`

## 5. CronManager Integration

- [ ] 5.1 Add optional `Arc<SessionPool>` field to `CronManager`
- [ ] 5.2 Modify `spawn_job` to select executor based on `job.mode`: `Isolated` → `AcpExecutor`, `Session` → `PoolPromptExecutor`
- [ ] 5.3 Update `CronManager::sync` to pass pool to `spawn_job`

## 6. Main Wiring

- [ ] 6.1 Pass `Arc<SessionPool>` from `main.rs` to `CronManager::new`

## 7. Skill Documentation

- [ ] 7.1 Update `skills/openab-cron/SKILL.md` to document `--mode` and `--file` flags
- [ ] 7.2 Add examples: isolated (default), session with prompt, session with file
- [ ] 7.3 Add note: session mode falls back to isolated if no active session exists

## 8. Verification

- [ ] 8.1 `cargo build` passes
- [ ] 8.2 Existing 95 cron tests still pass
- [ ] 8.3 Test: old JSON backward compatibility
- [ ] 8.4 Test: session mode with mock session pool
- [ ] 8.5 Test: heartbeat file read + empty file skip
