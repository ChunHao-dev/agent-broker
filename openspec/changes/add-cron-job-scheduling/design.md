## Context

OpenAB is a Discord bot that bridges AI coding agents (Kiro, Claude, Codex, Gemini, Copilot) over ACP protocol. Each thread gets its own agent process via the session pool. Environment variables (`OPENAB_CHANNEL_ID`, `OPENAB_SOURCE`) are injected at agent spawn time.

## Goals / Non-Goals

**Goals:**
- Users manage cron jobs via natural language — agent calls `openab-cron` CLI
- Jobs persist across restarts via JSON file
- Jobs execute prompts through independent ACP agent sessions and post results to the originating channel
- Thread/channel ID and source passed via env var — AI doesn't need to know or pass them
- Tool awareness via SKILL.md — zero per-message token cost
- Support all 5 agent variants with native skill paths
- Timezone support: per-job `--tz`, global default via config
- Multi-source delivery via `CronDelivery` trait (Discord now, extensible)

**Non-Goals:**
- MCP server integration
- Discord slash commands for cron
- Prompt injection for tool awareness

## Decisions

1. **Env-based channel ID and source over prompt injection**
   - openAB injects `OPENAB_CHANNEL_ID` and `OPENAB_SOURCE` at agent spawn time
   - `openab-cron` CLI reads env — AI never needs to pass these values
   - Eliminates per-message token cost

2. **SKILL.md for tool awareness, deployed per-agent**
   - Kiro: `$HOME/.kiro/skills/openab-cron/SKILL.md`
   - Claude: `$HOME/.claude/skills/openab-cron/SKILL.md`
   - Codex/Gemini: `$HOME/.agents/skills/openab-cron/SKILL.md`
   - Copilot: `$HOME/.copilot/skills/openab-cron/SKILL.md`
   - Same SKILL.md content, different paths per agent convention
   - Staged to `/opt/openab-skills/` at build, copied to `$HOME` at startup (PVC-safe)

3. **Shared JSON file between CLI and main process**
   - `openab-cron` writes to `cron_jobs.json`, CronManager reads it every 30s
   - Atomic writes (temp file + rename) prevent corruption

4. **CronDelivery trait for multi-source delivery**
   - `trait CronDelivery { send(), is_channel_gone() }`
   - `DiscordDelivery` is the current implementation
   - Job's `target.source` field selects which delivery to use
   - Adding Slack/Telegram only requires a new impl + `register_delivery()`

5. **Independent ACP session per job execution**
   - CronManager spawns temporary AcpConnection, not from the user session pool
   - No interference with user conversations

6. **Timezone priority chain**
   - `--tz` flag > `OPENAB_DEFAULT_TZ` env > `cron_jobs.json` store settings > UTC

## Risks / Trade-offs

- [Risk] Agent doesn't pick up SKILL.md → Mitigation: verified paths per agent, tested on deploy
- [Risk] Agent misformats CLI command → Mitigation: clear SKILL.md with examples; CLI returns helpful errors
- [Risk] JSON file corruption on crash → Mitigation: atomic write (temp + rename)
- [Risk] Thread deleted but job runs → Mitigation: auto-remove job on unknown channel error via `is_channel_gone()`
