## Why

Cron jobs currently spawn a fresh ACP process for each execution, losing all conversation context. For use cases like "summarize what we did today and update the todo list," the cron job needs access to the ongoing conversation history in the thread's session. This is analogous to OpenClaw's heartbeat mechanism, where periodic tasks operate within an existing session rather than starting from scratch.

Additionally, cron job prompts are fixed at creation time. For evolving task lists (e.g. tracking PRs, managing a todo list), the agent needs a way to read and update a persistent task file — similar to OpenClaw's `HEARTBEAT.md`. This allows multiple tasks to be managed in one file, and the agent can add/remove tasks as the situation changes.

## What Changes

- Add `mode` field to `CronJob` struct: `isolated` (default) or `session`
- Add `heartbeat_file` optional field to `CronJob` — path to a markdown file used as the prompt source
- Add `--mode` and `--file` flags to `openab-cron add` CLI
- Modify `CronManager::spawn_job` to check the `SessionPool` when mode is `session`
- When `--file` is set, read the file at execution time as the prompt (agent can update it between runs)
- Add `PoolPromptExecutor` that sends prompts through existing sessions via `SessionPool::with_connection`
- Fallback: if no existing session is found, spawn a new independent ACP process
- Update `skills/openab-cron/SKILL.md` to document `--mode` and `--file` usage

## CLI Design

```bash
# isolated (default) — new session each time, no context
openab-cron add --interval 30m --prompt "報天氣"

# session — reuse existing session, fixed prompt
openab-cron add --interval 30m --mode session --prompt "更新 todo list"

# session + file — reuse existing session, read file as prompt (agent can update)
openab-cron add --interval 30m --mode session --file HEARTBEAT.md
```

| Mode | Session | Prompt source |
|------|---------|---------------|
| `isolated` | New each time | `--prompt` |
| `session` | Reuse existing | `--prompt` or `--file` (pick one) |

## Capabilities

### New Capabilities
- `session-mode`: Cron jobs execute within an existing ACP session, accessing full conversation context
- `heartbeat-file`: Cron jobs can read a markdown file as the prompt source, allowing the agent to update the task list between runs
- `pool-prompt-executor`: New `PoolPromptExecutor` that routes cron prompts through the session pool

### Modified Capabilities
- `CronJob` struct gains `mode` enum and `heartbeat_file: Option<String>` fields
- `openab-cron add` gains `--mode` and `--file` flags
- `CronManager` accepts an optional `SessionPool` reference for session-aware execution
- `SKILL.md` updated with `--mode` and `--file` usage guidance

## Impact

- `src/cron_store.rs` — add `CronMode` enum and `heartbeat_file` to `CronJob`
- `src/bin/openab_cron.rs` — add `--mode` and `--file` flags to `Add` subcommand
- `src/cron_manager.rs` — add `PoolPromptExecutor`, modify `spawn_job` to select executor and read heartbeat file
- `src/main.rs` — pass `SessionPool` clone to `CronManager` on startup
- `skills/openab-cron/SKILL.md` — document `--mode` and `--file` flags
