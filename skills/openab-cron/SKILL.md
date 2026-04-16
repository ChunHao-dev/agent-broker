---
name: openab-cron
description: Manage scheduled tasks. Use when user wants to set up recurring or one-time jobs, reminders, or periodic checks.
---

# openab-cron

Schedule recurring or one-time tasks in the current thread. The thread ID is automatically detected — you do not need to specify it.

## Commands

```bash
# Set default timezone (persisted, used when --tz is not specified)
openab-cron config --tz "Asia/Taipei"

# View current default timezone
openab-cron config

# Recurring job — fixed interval
openab-cron add --interval <e.g. 30s, 5m, 1h, 2d> --prompt "<task>"

# Recurring job — cron expression (precise scheduling)
openab-cron add --cron "<expression>" --prompt "<task>"
openab-cron add --cron "<expression>" --tz "<timezone>" --prompt "<task>"

# One-time job — delay then execute once
openab-cron add --once --interval <delay> --prompt "<task>"

# One-time job — at specific time
openab-cron add --at "<ISO-8601 datetime>" --prompt "<task>"
openab-cron add --at "<datetime>" --tz "<timezone>" --prompt "<task>"

# List jobs in this thread
openab-cron list

# List jobs across all threads
openab-cron list --all

# Remove a job by ID
openab-cron remove --id <job-id>
```

## Examples

```bash
# Every hour
openab-cron add --interval 1h --prompt "check the weather"

# Every weekday at 9am Taipei time
openab-cron add --cron "0 0 9 * * Mon-Fri *" --tz "Asia/Taipei" --prompt "morning briefing"

# Every day at 6pm UTC
openab-cron add --cron "0 0 18 * * * *" --prompt "daily summary"

# Remind in 10 minutes (one-time)
openab-cron add --once --interval 10m --prompt "remind user to check email"

# At a specific time (one-time)
openab-cron add --at "2026-04-14T14:00" --tz "Asia/Taipei" --prompt "meeting reminder"
```

## Cron Expression Format

```
sec  min  hour  day  month  weekday  year
 *    *    *     *    *      *        *

0 0 9 * * Mon-Fri *    → weekdays at 9:00
0 */30 * * * * *        → every 30 minutes
0 0 8,12,18 * * * *     → at 8:00, 12:00, 18:00
```

## Notes

- `--interval`, `--cron`, and `--at` are mutually exclusive
- Minimum interval for recurring jobs is 60 seconds
- Maximum 5 jobs per thread
- One-time jobs are automatically removed after execution
- `--at` jobs are always one-time
- `--tz` accepts IANA timezone names (e.g. Asia/Taipei, America/New_York, Europe/London)
- If no `--tz` is given, uses the default timezone set via `openab-cron config --tz`. If no default is set, uses UTC.
- When a user first sets up cron jobs, ask their timezone and set it with `openab-cron config --tz`.
