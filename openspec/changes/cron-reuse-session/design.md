## Context

OpenAB's cron system currently spawns an independent ACP process per job execution via `AcpExecutor`. The session pool (`SessionPool`) manages per-thread ACP connections for interactive conversations, keyed by platform-prefixed channel ID (e.g. `discord:123456789`). These two systems are completely separate — cron jobs have no access to conversation context.

Additionally, cron job prompts are fixed at creation time. OpenClaw solves this with `HEARTBEAT.md` — a file the agent reads each heartbeat and can update between runs.

The `PromptExecutor` trait already abstracts prompt execution. The session pool already supports `with_connection` for sending prompts through existing sessions.

## Goals / Non-Goals

**Goals:**
- Two execution modes: `isolated` (default) and `session`
- `session` mode executes within the existing ACP session for the target channel
- `--file` option reads a markdown file as the prompt at execution time (agent can update it)
- Graceful fallback to spawning a new process if no session exists
- Backward compatible — existing jobs default to `isolated` mode

**Non-Goals:**
- Creating sessions on demand for session-mode jobs (we only reuse existing ones)
- Modifying the session pool's eviction or lifecycle logic
- Parsing `tasks:` blocks with intervals inside heartbeat files (OpenClaw feature, not needed for v1)

## Decisions

1. **`CronMode` enum: two modes**
   - `CronMode::Isolated` — current behavior, new ACP process each time
   - `CronMode::Session` — reuse existing session from pool
   - Serialized as lowercase string in JSON: `"isolated"`, `"session"`
   - Defaults to `"isolated"` via `#[serde(default)]`

2. **`--file` is a prompt source option, not a mode**
   - `heartbeat_file: Option<String>` on `CronJob`
   - Works with any mode, but primarily useful with `session` mode
   - At execution time, read file content as the prompt instead of using fixed `prompt` field
   - Agent can update the file between runs

3. **New `PoolPromptExecutor` over modifying `AcpExecutor`**
   - `AcpExecutor` stays unchanged for isolated execution
   - `PoolPromptExecutor` wraps `SessionPool` + fallback `AcpExecutor`
   - `spawn_job` selects which executor based on `job.mode`

4. **Session key derived from `DeliveryTarget`**
   - Session pool key format is `<source>:<channel_id>` (e.g. `discord:123456789`)
   - `CronJob.target` already has `source` and `channel_id`
   - Session key = `format!("{}:{}", target.source, target.channel_id)`

5. **Fallback to `AcpExecutor` when no session exists**
   - `PoolPromptExecutor::execute` first tries `pool.with_connection(key, ...)`
   - If no connection found, falls back to `AcpExecutor::execute`
   - Log a warning when falling back

6. **Pass `SessionPool` as `Arc<SessionPool>` to `CronManager`**
   - `CronManager::new` takes an optional `Arc<SessionPool>`
   - `spawn_job` receives the pool reference and creates `PoolPromptExecutor` when needed

7. **Heartbeat file storage and behavior**
   - Stored in PVC data directory under `heartbeat/` alongside `cron_jobs.json`
   - CLI accepts relative path, CronManager resolves against data directory as base path
   - If file doesn't exist or is empty, skip execution and log warning (don't count as failure)
   - The fixed `prompt` field is still required as fallback description / label for `list` output
   - Agent creates the file during conversation, then sets up the cron job
   - Agent can update the file via normal file tools during session execution

   ```
   PVC (/data)
   ├── cron_jobs.json
   └── heartbeat/
       └── HEARTBEAT.md
   ```

8. **CLI validation**
   - `--mode session` accepts `--prompt` or `--file` (one required)
   - `--mode isolated` (default) requires `--prompt`
   - `--file` without `--mode session` is an error

## Risks / Trade-offs

- [Risk] Session evicted between cron check and prompt send → Mitigation: fallback to fresh spawn
- [Risk] Cron prompt interferes with active user conversation → Mitigation: `with_connection` acquires write lock; prompt prefixed with `[Scheduled task at ...]`
- [Risk] Long-running cron prompt blocks user messages → Mitigation: existing 5 min timeout applies
- [Risk] Heartbeat file deleted or corrupted → Mitigation: skip execution, log warning, don't count as failure
- [Trade-off] Session-mode jobs depend on user activity to have a session → Acceptable: fallback ensures jobs always execute
- [Trade-off] Heartbeat file is outside JSON store → Acceptable: agent needs write access
