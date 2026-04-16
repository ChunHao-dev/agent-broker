# RFC: Cron Job Scheduling for openAB

## Summary

Add the ability for Discord users to create, manage, and execute scheduled tasks through natural language. The AI agent recognizes scheduling intent and manages jobs via a tool, with openAB handling the actual scheduling and execution.

## Motivation

Currently openAB is purely reactive — it only responds when a user sends a message. There's no way to set up recurring tasks like "check the weather every hour" or one-time reminders like "remind me in 30 minutes". Adding cron job support would make openAB significantly more useful as a persistent assistant.

---

## How OpenClaw Does It

OpenClaw has a mature cron job system. Here's the full flow:

### Architecture

```
User: "Check weather every day at 9am"
    │
    ▼
┌─────────────────────────────────────────────────────────────────┐
│  AI Agent (main conversation session)                           │
│                                                                 │
│  1. Recognize scheduling intent                                 │
│  2. Call built-in cron tool (tool calling, JSON params)          │
│     {                                                           │
│       action: "add",                                            │
│       job: {                                                    │
│         schedule: { kind: "cron", expr: "0 9 * * *",            │
│                     tz: "Asia/Taipei" },                        │
│         payload:  { kind: "agentTurn",                          │
│                     message: "check weather" },                 │
│         delivery: { mode: "announce" }                          │
│       }                                                         │
│     }                                                           │
└──────────────────────┬──────────────────────────────────────────┘
                       │ tool call → runtime intercepts
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│  cron-tool.ts (in-process)                                      │
│                                                                 │
│  3. Normalize layer repairs AI input                            │
│     (field relocation, case fix, type coercion)                 │
│  4. Inject agentId, sessionKey                                  │
│  5. Call local Gateway via WebSocket JSON-RPC                   │
└──────────────────────┬──────────────────────────────────────────┘
                       │ ws://127.0.0.1:18789
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│  Gateway (WebSocket server, separate process)                   │
│                                                                 │
│  6. Auto-resolve delivery target from session context           │
│     (user on Telegram → result goes back to Telegram)           │
│  7. Persist to JSON file                                        │
└──────────────────────┬──────────────────────────────────────────┘
                       │
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│  Scheduling Engine (setTimeout loop)                            │
│                                                                 │
│  8. Each tick finds due jobs                                    │
│  9. Execute based on payload.kind:                              │
│     ├─ systemEvent → inject into main session (has context)     │
│     ├─ agentTurn  → new isolated session (no context,           │
│     │               configurable model/timeout)                 │
│     └─ script     → run shell command directly                  │
│  10. After execution:                                           │
│      ├─ Update state (nextRunAtMs, lastRunStatus)               │
│      ├─ Write run log (status, duration, token usage)           │
│      ├─ On failure → exponential backoff retry                  │
│      └─ N consecutive failures → send failureAlert              │
└──────────────────────┬──────────────────────────────────────────┘
                       │
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│  Delivery Layer (decoupled from execution)                      │
│                                                                 │
│  11. Deliver based on delivery.mode:                            │
│      ├─ announce → send to chat (Telegram/WhatsApp/Slack/...)   │
│      ├─ webhook  → HTTP POST to specified URL                   │
│      └─ none     → log only, don't send                         │
└─────────────────────────────────────────────────────────────────┘
```

### Key Design

- **Tool exposure**: Built-in tool (`cron`), not MCP. Part of OpenClaw's own tool system (alongside `exec`, `message`). AI calls it via tool calling, runtime intercepts, normalizes input, then forwards to Gateway via WebSocket JSON-RPC
- **Delivery target**: Auto-resolved from session context (session store records lastChannel, lastTo, lastThreadId). AI doesn't need to pass any ID
- **Schedule types**: `at` (one-shot absolute time), `every` (fixed interval), `cron` (cron expression + timezone)
- **Execution modes**: Three coexisting — main session injection (`systemEvent`), isolated session (`agentTurn`), direct script
- **Reliability**: Exponential backoff, timeout protection, failure alerts, throttled restart catch-up, concurrency control

---

## Proposed Approach for openAB

### Architecture

```
User: "Check weather every hour"
    │
    ▼ @mention or thread message
┌─────────────────────────────────────────────────────────────────┐
│  openAB (Rust main process)                                     │
│                                                                 │
│  1. Receive Discord message                                     │
│  2. Find or create agent session (per thread)                   │
│  3. Inject env on spawn: OPENAB_THREAD_ID=<thread_id>           │
│  4. Send prompt to agent                                        │
└──────────────────────┬──────────────────────────────────────────┘
                       │ ACP JSON-RPC (stdio)
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│  AI Agent (kiro-cli / claude / codex / gemini)                  │
│                                                                 │
│  5. Discover openab-cron tool from SKILL.md / AGENTS.md         │
│  6. Recognize scheduling intent                                 │
│  7. Call shell tool to execute CLI:                              │
│     openab-cron add --interval 1h --prompt "check weather"      │
│     (no --thread needed, CLI reads env OPENAB_THREAD_ID)        │
└──────────────────────┬──────────────────────────────────────────┘
                       │ shell exec (inherits env)
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│  openab-cron CLI                                                │
│                                                                 │
│  8. Read OPENAB_THREAD_ID env var                               │
│  9. Write to cron_jobs.json (atomic write: temp + rename)       │
└──────────────────────┬──────────────────────────────────────────┘
                       │ JSON file
                       ▼
┌─────────────────────────────────────────────────────────────────┐
│  CronManager (inside openAB main process)                       │
│                                                                 │
│  10. Reload JSON every 30s, sync in-memory tasks                │
│  11. On schedule → spawn temporary ACP connection               │
│      (outside session pool)                                     │
│  12. Send prompt → collect response → post to Discord thread    │
│  13. One-time jobs → auto-remove after execution                │
└─────────────────────────────────────────────────────────────────┘
```

### Key Design

- **Tool exposure**: SKILL.md (Kiro) + AGENTS.md (Claude/Codex/Gemini), loaded once at agent startup, 0 extra tokens per message
- **Delivery target**: Env var `OPENAB_THREAD_ID`, each thread has its own agent process, injected at spawn, CLI reads automatically. AI doesn't need to know
- **Schedule types**: `--interval` (fixed interval), `--once` (one-time delay)
- **Execution mode**: Isolated session (temporary ACP connection per execution, outside pool, no interference with user conversations)
- **Persistence**: JSON file, atomic write (temp + rename)

---

## Comparison

### Tool Exposure

| | OpenClaw | openAB |
|---|---------|--------|
| Method | Built-in tool calling → runtime intercept → Gateway WS | CLI + SKILL.md / AGENTS.md |
| AI input format | JSON object | Shell command + flags |
| Input repair | ✅ Normalize layer (field relocation, type coercion) | ❌ Strict CLI parsing (but flags are self-documenting, less error-prone) |
| Token cost | Tool schema loaded once per session | SKILL/AGENTS loaded once at agent startup, 0 per-message |
| Cross-agent support | Single agent architecture | ✅ Any agent with shell tool support |

### Delivery Target Resolution

| | OpenClaw | openAB |
|---|---------|--------|
| Method | Session-based auto-resolution (session store) | Env var `OPENAB_THREAD_ID` |
| AI needs to pass ID | ❌ Not at all | ❌ Not at all |
| Complexity | High (session store + fallback chain) | Low (env var, one line of code) |
| Cross-channel delivery | ✅ Supported | ❌ Fixed to original thread |

### Scheduling

| | OpenClaw | openAB |
|---|---------|--------|
| Fixed interval | ✅ `everyMs` | ✅ `--interval` |
| One-time delay | ✅ `at` (relative time) | ✅ `--once --interval` |
| Cron expression | ✅ `expr` + `tz` | 🔜 Planned (`--cron "0 9 * * *"`) |
| Absolute time | ✅ `at` (ISO-8601) | 🔜 Planned (`--at "2026-04-14T14:00"`) |
| Timezone | ✅ Per-job `tz` field | 🔜 Planned (`--tz "Asia/Taipei"`) |

### Execution Mode

| | OpenClaw | openAB |
|---|---------|--------|
| AI isolated session | ✅ `agentTurn` | ✅ Temporary ACP connection |
| AI main session injection | ✅ `systemEvent` | ❓ To discuss (see caveats) |
| Direct script | ✅ `script` | ❓ To discuss |
| Outside session pool | ✅ | ✅ |

**Caveats for main session injection**: openAB sessions are in-memory agent processes, unlike OpenClaw's persistent session store. Main session injection faces additional challenges in openAB:
- Session TTL expired → agent process already cleaned up, cannot inject
- Session pool full → cannot rebuild session
- Agent processing user message → cron injection conflicts with user conversation

### Reliability

| | OpenClaw | openAB |
|---|---------|--------|
| Exponential backoff retry | ✅ | ❓ To discuss |
| Timeout protection | ✅ Per-job | ❓ To discuss |
| Failure alerts | ✅ `failureAlert` | ❓ To discuss |
| Restart catch-up | ✅ Throttled replay | ❓ To discuss |
| Concurrency limit | ✅ `maxConcurrentRuns` | ❓ To discuss |
| Run log | ✅ `<jobId>.jsonl` | ❓ To discuss |

### Security

| | OpenClaw | openAB |
|---|---------|--------|
| Restrict who can create | ✅ `ownerOnly` | ❓ To discuss (reuse `allowed_users`?) |
| Per-job tool restrictions | ✅ `toolsAllow` | ❌ Inherits agent global config |

---

## POC Implementation (Completed)

The following features have working prototype code (not yet merged to main):

### Core Infrastructure
- **`openab-cron` CLI binary** — `add`, `list`, `remove` subcommands, parsed with clap
- **`CronManager`** — Scheduling manager inside openAB main process, reloads JSON every 30s, syncs in-memory tasks
- **JSON persistence** — `cron_jobs.json`, atomic write (temp + rename) to prevent corruption
- **`[cron]` config section** — `enabled`, `data_file` settings, controllable via Helm values

### Schedule Types
- **Fixed interval** — `openab-cron add --interval 1h --prompt "..."`
- **One-time delay** — `openab-cron add --once --interval 10m --prompt "..."`, auto-removes after execution

### Execution
- **Isolated session execution** — CronManager spawns temporary AcpConnection per execution, outside session pool, not limited by `max_sessions`
- **Thread deletion detection** — Auto-removes job when Discord thread is deleted

### Tool Exposure
- **SKILL.md** — For Kiro CLI, placed at `~/.kiro/skills/openab-cron/SKILL.md`
- **AGENTS.md** — For Claude Code / Codex / Gemini, placed in working directory
- **Dockerfile** — Copies from `/opt/openab-skills/` to PVC-mounted home directory at startup

### Thread ID Passing
- **Env var injection** — `pool.rs` injects `OPENAB_THREAD_ID` when spawning agent
- **CLI auto-reads** — `--thread` is optional, falls back to env var when not specified
- **AI doesn't need to know** — No prompt injection needed, 0 extra tokens per message

---

## Alternatives Considered

### 1. Prompt injection vs SKILL.md

| | Prompt injection | SKILL.md |
|---|-----------------|----------|
| Token cost | ~150 tokens/msg | 0 (loaded at agent startup) |
| Thread ID | Hardcoded in prompt | Via env var, AI doesn't need to know |
| Maintainability | Change Rust code | Change markdown file |

Chose SKILL.md: saves tokens, decoupled, works across agents.

### 2. MCP server vs CLI

| | MCP server | CLI |
|---|-----------|-----|
| Token cost | ~300-500 tokens/session | 0 (SKILL description) |
| Input format | Structured JSON | Shell flags |
| Cross-agent | Requires MCP support per agent | ✅ Any agent with shell tool |
| Dev cost | High (write MCP server) | Low (clap CLI) |

Chose CLI: simpler, more universal, lower token cost.

### 3. Env var vs Session-based resolution

| | Env var | Session-based (OpenClaw) |
|---|---------|--------------------------|
| Complexity | Low (inject at spawn) | High (session store + fallback chain) |
| Single channel | ✅ `OPENAB_THREAD_ID` | ✅ |
| Multi-channel (same pod) | ✅ Add `OPENAB_SOURCE` | ✅ |
| Cross-channel session (user switches channel) | ❌ | ✅ |
| Fits when | One process per thread/chat (openAB) | One session may span channels (OpenClaw) |

Chose env var: openAB uses per-thread processes, source is fixed and won't cross channels. Even when adding Telegram etc., just inject an additional `OPENAB_SOURCE` env var — no session store needed.

### 4. Gateway API vs JSON file

| | Gateway API | JSON file |
|---|-----------|-----------|
| State consistency | ✅ Central control | Requires atomic write |
| Multi-pod sharing | ✅ | ❌ |
| Complexity | High (extra process) | Low |
| Fits when | Multi-channel, multi-pod | Single pod |

Chose JSON file: openAB is currently single-pod, doesn't need Gateway complexity.

---

## Future Possibilities

### Scheduling Enhancements
- **Cron expressions** — `--cron "0 9 * * *"` for precise time scheduling
- **Absolute time** — `--at "2026-04-14T14:00"` for exact execution time
- **Timezone** — `--tz "Asia/Taipei"` per-job timezone

### Execution Modes
- **Main session injection** — Inject into user's existing session (needs to solve TTL, pool limit, conversation conflict issues)
- **Direct script** — Run shell commands without AI (fast, cheap, deterministic)

### Reliability
- **Exponential backoff retry** — Delay retry on failure, reset on success
- **Timeout protection** — Per-job `timeoutSeconds`
- **Failure alerts** — Notify user after N consecutive failures
- **Restart catch-up** — Throttled replay of missed jobs after pod restart
- **Concurrency control** — `maxConcurrentRuns` to limit simultaneous executions

### Observability
- **Run log** — Per-execution records (status, duration, token usage)
- **Token usage tracking** — Track AI cost of cron jobs

### Multi-source
- **Telegram / Slack etc.** — Reserve `source` field in job data model
- **Cross-channel delivery** — Deliver results to different channel/thread

### Security
- **User restrictions** — Limit who can create cron jobs
- **Per-job tool restrictions** — Allow-list of tools per job

---

## Discussion Questions

1. **Cron expressions** — Should we support `0 9 * * *` style scheduling? Enables "every weekday at 9am" which fixed intervals can't do. Requires adding a `cron` crate dependency.

2. **Main session mode** — Should cron jobs be able to inject into the user's existing session (with conversation context)? Useful for reminders, but openAB's in-memory sessions have TTL expiry, pool limits, and conversation conflict issues.

3. **Failure handling** — How sophisticated should retry logic be? Ranges from "just report the error" to full exponential backoff with failure alerts.

4. **Restart catch-up** — When the pod restarts, should missed jobs be replayed? Or simply skipped?

5. **Run log** — Should we log every execution (status, duration, token usage)?

6. **Security** — Should cron job creation be restricted to certain Discord users?

7. **Cross-channel delivery** — Should job results be deliverable to a different channel/thread than where the job was created?

8. **Concurrency limit** — Should there be a cap on how many cron jobs can execute simultaneously?

9. **Multi-source support** — Currently the source is Discord. Telegram and others may be added in the future. Within a single pod, CLI + JSON file can be shared, but the job data model may need a `source` field (discord/telegram) and corresponding channel ID so CronManager knows where to deliver.

Feedback and suggestions welcome!
