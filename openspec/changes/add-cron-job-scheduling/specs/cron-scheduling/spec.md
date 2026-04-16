## ADDED Requirements

### Requirement: Create cron job via CLI
The system SHALL provide an `openab-cron add` command that creates a recurring or one-time job with three schedule types: `--interval`, `--cron`, and `--at`. The `--thread` parameter SHALL be optional; when omitted, the CLI reads `OPENAB_CHANNEL_ID` from the environment.

#### Scenario: Add job without --thread (env-based)
- **WHEN** `openab-cron add --interval 30m --prompt "check weather"` is executed inside an agent process with `OPENAB_CHANNEL_ID=123`
- **THEN** a new job is created for channel 123 with source from `OPENAB_SOURCE`

#### Scenario: Add job with cron expression and timezone
- **WHEN** `openab-cron add --cron "0 0 9 * * Mon-Fri *" --tz "Asia/Taipei" --prompt "morning briefing"` is executed
- **THEN** a new recurring job is created that runs weekdays at 9am Taipei time

#### Scenario: Add one-time job at specific time
- **WHEN** `openab-cron add --at "2026-04-14T14:00" --tz "Asia/Taipei" --prompt "meeting reminder"` is executed
- **THEN** a one-time job is created that executes at the specified time and auto-removes after

#### Scenario: Add one-time job with delay
- **WHEN** `openab-cron add --once --interval 10m --prompt "remind me"` is executed
- **THEN** a one-time job is created that auto-removes after execution

#### Scenario: No channel ID available
- **WHEN** `openab-cron add` is called without `--thread` and `OPENAB_CHANNEL_ID` is not set
- **THEN** the CLI exits with an error explaining how to provide a channel ID

#### Scenario: Mutually exclusive schedule types
- **WHEN** `openab-cron add --interval 30m --cron "0 * * * * * *" --prompt "test"` is executed
- **THEN** the CLI exits with an error stating the options are mutually exclusive

### Requirement: List cron jobs via CLI
The system SHALL provide an `openab-cron list` command with optional `--all` flag.

#### Scenario: List jobs for current channel
- **WHEN** `openab-cron list` is executed with `OPENAB_CHANNEL_ID=123`
- **THEN** the CLI prints jobs for channel 123

#### Scenario: List all jobs across channels
- **WHEN** `openab-cron list --all` is executed
- **THEN** the CLI prints all jobs across all channels

### Requirement: Remove cron job via CLI
The system SHALL provide an `openab-cron remove` command that deletes a job by ID.

#### Scenario: Remove existing job
- **WHEN** `openab-cron remove --id abc123` is executed
- **THEN** the job is removed and the CLI confirms

### Requirement: Configure default timezone
The system SHALL provide an `openab-cron config` command to view and set the default timezone.

#### Scenario: Set default timezone
- **WHEN** `openab-cron config --tz "Asia/Taipei"` is executed
- **THEN** the default timezone is persisted and used when `--tz` is not specified

#### Scenario: View default timezone
- **WHEN** `openab-cron config` is executed without arguments
- **THEN** the current default timezone is displayed

### Requirement: Timezone resolution priority
The system SHALL resolve timezone in this order: explicit `--tz` flag > `OPENAB_DEFAULT_TZ` env var > store settings > UTC.

### Requirement: Inject environment variables on agent spawn
The system SHALL inject `OPENAB_CHANNEL_ID`, `OPENAB_SOURCE`, and `OPENAB_DEFAULT_TZ` as environment variables when spawning an agent process.

#### Scenario: Agent process receives context
- **WHEN** openAB spawns an agent process for channel 123
- **THEN** the process environment contains `OPENAB_CHANNEL_ID=123`, `OPENAB_SOURCE=discord`

### Requirement: Expose cron tool via SKILL.md
The system SHALL provide a SKILL.md file deployed to each agent's native skill path so the agent discovers the tool without per-message prompt injection.

#### Scenario: Agent discovers cron tool
- **WHEN** a user says "每30分鐘報天氣" in a thread
- **THEN** the agent recognizes the intent from SKILL.md and calls `openab-cron add`

### Requirement: Multi-source delivery
The system SHALL use a `CronDelivery` trait to abstract message delivery, allowing multiple backends.

#### Scenario: Discord delivery
- **WHEN** a cron job with `source: "discord"` executes
- **THEN** the result is posted via Discord API to the specified channel

#### Scenario: Unknown source
- **WHEN** a cron job has a source with no registered delivery
- **THEN** the system logs an error and skips execution

### Requirement: Persist and execute cron jobs
The system SHALL persist jobs to JSON and execute them on schedule via independent ACP sessions.

#### Scenario: Job executes on schedule
- **WHEN** a job's schedule triggers
- **THEN** the system sends the prompt to a temporary ACP agent and posts the response to the channel

#### Scenario: One-time job auto-removes
- **WHEN** a one-time job finishes execution
- **THEN** the job is removed from the JSON file

#### Scenario: Channel deleted
- **WHEN** delivery fails with "channel not found"
- **THEN** the job is automatically removed
