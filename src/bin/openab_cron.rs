use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[path = "../cron_store.rs"]
mod cron_store;
use cron_store::{CronJob, CronStore, DeliveryTarget, Schedule};

const DEFAULT_DATA_FILE: &str = "cron_jobs.json";
const DEFAULT_MAX_JOBS: usize = 5;
const DEFAULT_MIN_INTERVAL: u64 = 60;

#[derive(Parser)]
#[command(name = "openab-cron", about = "Manage OpenAB cron jobs")]
struct Cli {
    #[arg(long, default_value = DEFAULT_DATA_FILE)]
    data_file: PathBuf,
    #[arg(long, default_value_t = DEFAULT_MAX_JOBS)]
    max_jobs: usize,
    #[arg(long, default_value_t = DEFAULT_MIN_INTERVAL)]
    min_interval: u64,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Add a scheduled job
    Add {
        #[arg(long)]
        thread: Option<u64>,
        #[arg(long, help = "Interval like 30s, 5m, 1h, 2d")]
        interval: Option<String>,
        #[arg(long, help = "Cron expression like \"0 9 * * *\"")]
        cron: Option<String>,
        #[arg(long, help = "Absolute time ISO-8601 like \"2026-04-14T14:00:00\"")]
        at: Option<String>,
        #[arg(long, help = "Timezone like Asia/Taipei (for --cron and --at)")]
        tz: Option<String>,
        #[arg(long)]
        prompt: String,
        #[arg(long, help = "Run only once then auto-remove")]
        once: bool,
    },
    /// List cron jobs for a thread
    List {
        #[arg(long)]
        thread: Option<u64>,
        #[arg(long, help = "List jobs across all threads")]
        all: bool,
    },
    /// Remove a cron job by ID
    Remove {
        #[arg(long)]
        id: String,
    },
    /// View or update cron settings
    Config {
        #[arg(long, help = "Set default timezone (e.g. Asia/Taipei)")]
        tz: Option<String>,
    },
}

fn parse_interval(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty interval".into());
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: u64 = num.parse().map_err(|_| format!("invalid number: {num}"))?;
    match unit {
        "s" => Ok(n),
        "m" => Ok(n * 60),
        "h" => Ok(n * 3600),
        "d" => Ok(n * 86400),
        _ => Err(format!("unknown unit '{unit}', use s/m/h/d")),
    }
}

/// Resolve delivery target from explicit --thread or env vars.
/// Priority: --thread > OPENAB_CHANNEL_ID > OPENAB_THREAD_ID (backward compat)
fn resolve_target(explicit_thread: Option<u64>) -> anyhow::Result<DeliveryTarget> {
    let source = std::env::var("OPENAB_SOURCE").unwrap_or_else(|_| "discord".into());

    let channel_id = if let Some(t) = explicit_thread {
        t.to_string()
    } else if let Ok(id) = std::env::var("OPENAB_CHANNEL_ID") {
        id
    } else if let Ok(id) = std::env::var("OPENAB_THREAD_ID") {
        id
    } else {
        anyhow::bail!("--thread not given and OPENAB_CHANNEL_ID/OPENAB_THREAD_ID not set");
    };

    Ok(DeliveryTarget { source, channel_id })
}

fn resolve_schedule(
    interval: Option<String>,
    cron_expr: Option<String>,
    at: Option<String>,
    tz: Option<String>,
    store_default_tz: Option<String>,
    min_interval: u64,
    once: bool,
) -> anyhow::Result<Schedule> {
    let specified = [interval.is_some(), cron_expr.is_some(), at.is_some()];
    let count = specified.iter().filter(|&&x| x).count();
    if count == 0 {
        anyhow::bail!("specify one of --interval, --cron, or --at");
    }
    if count > 1 {
        anyhow::bail!("--interval, --cron, and --at are mutually exclusive");
    }

    // Resolve timezone: explicit --tz > OPENAB_DEFAULT_TZ env > store settings > None (UTC)
    let tz = tz
        .or_else(|| std::env::var("OPENAB_DEFAULT_TZ").ok())
        .or(store_default_tz);

    if let Some(expr) = cron_expr {
        expr.parse::<cron::Schedule>()
            .map_err(|e| anyhow::anyhow!("invalid cron expression: {e}"))?;
        if let Some(ref tz_str) = tz {
            tz_str.parse::<chrono_tz::Tz>()
                .map_err(|_| anyhow::anyhow!("invalid timezone: {tz_str}"))?;
        }
        return Ok(Schedule::Cron { expr, tz });
    }

    if let Some(at_str) = at {
        let dt = if let Some(ref tz_str) = tz {
            let tz: chrono_tz::Tz = tz_str.parse()
                .map_err(|_| anyhow::anyhow!("invalid timezone: {tz_str}"))?;
            chrono::NaiveDateTime::parse_from_str(&at_str, "%Y-%m-%dT%H:%M:%S")
                .or_else(|_| chrono::NaiveDateTime::parse_from_str(&at_str, "%Y-%m-%dT%H:%M"))
                .map_err(|e| anyhow::anyhow!("invalid datetime: {e}"))?
                .and_local_timezone(tz)
                .single()
                .ok_or_else(|| anyhow::anyhow!("ambiguous datetime for timezone {tz_str}"))?
                .with_timezone(&chrono::Utc)
        } else {
            chrono::DateTime::parse_from_rfc3339(&at_str)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .or_else(|_| {
                    chrono::NaiveDateTime::parse_from_str(&at_str, "%Y-%m-%dT%H:%M:%S")
                        .or_else(|_| chrono::NaiveDateTime::parse_from_str(&at_str, "%Y-%m-%dT%H:%M"))
                        .map(|ndt| ndt.and_utc())
                        .map_err(|e| anyhow::anyhow!("invalid datetime: {e}"))
                })?
        };
        if dt <= chrono::Utc::now() {
            anyhow::bail!("--at time must be in the future");
        }
        return Ok(Schedule::At { at: dt });
    }

    if let Some(interval_str) = interval {
        let secs = parse_interval(&interval_str)
            .map_err(|e| anyhow::anyhow!("invalid interval: {e}"))?;
        if !once && secs < min_interval {
            anyhow::bail!("interval too short, minimum is {min_interval}s");
        }
        return Ok(Schedule::Every { interval_secs: secs });
    }

    unreachable!()
}

fn format_schedule(s: &Schedule) -> String {
    match s {
        Schedule::Every { interval_secs } => format!("every {interval_secs}s"),
        Schedule::Cron { expr, tz } => match tz {
            Some(tz) => format!("cron \"{expr}\" ({tz})"),
            None => format!("cron \"{expr}\" (UTC)"),
        },
        Schedule::At { at } => format!("at {at}"),
    }
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let mut store = CronStore::load(&cli.data_file)?;

    match cli.cmd {
        Cmd::Add { thread, interval, cron, at, tz, prompt, once } => {
            let target = resolve_target(thread)?;
            let schedule = resolve_schedule(interval, cron, at, tz, store.settings.default_tz.clone(), cli.min_interval, once)?;

            if once && matches!(schedule, Schedule::Cron { .. }) {
                anyhow::bail!("--once cannot be used with --cron (cron is inherently recurring)");
            }
            let once = once || matches!(schedule, Schedule::At { .. });

            let count = store.jobs.values().filter(|j| j.target.channel_id == target.channel_id).count();
            if count >= cli.max_jobs {
                anyhow::bail!("channel already has {count}/{} jobs", cli.max_jobs);
            }
            let id = uuid::Uuid::new_v4().to_string()[..8].to_string();
            let desc = format_schedule(&schedule);
            let job = CronJob {
                id: id.clone(),
                target: target.clone(),
                schedule,
                prompt,
                created_at: chrono::Utc::now(),
                once,
            };
            store.jobs.insert(id.clone(), job);
            store.save(&cli.data_file)?;
            if once {
                println!("created one-time job {id} ({desc}, {}/{})", target.source, target.channel_id);
            } else {
                println!("created job {id} ({desc}, {}/{})", target.source, target.channel_id);
            }
        }
        Cmd::List { thread, all } => {
            let jobs: Vec<_> = if all {
                store.jobs.values().collect()
            } else {
                let target = resolve_target(thread)?;
                store.jobs.values().filter(|j| j.target.channel_id == target.channel_id).collect()
            };
            if jobs.is_empty() {
                println!("no cron jobs found");
            } else {
                for j in jobs {
                    let kind = if j.once { "once" } else { "recurring" };
                    println!("  {} | {}/{} | {} | {} | {}", j.id, j.target.source, j.target.channel_id, kind, format_schedule(&j.schedule), j.prompt);
                }
            }
        }
        Cmd::Remove { id } => {
            if store.jobs.remove(&id).is_none() {
                anyhow::bail!("job '{id}' not found");
            }
            store.save(&cli.data_file)?;
            println!("removed job {id}");
        }
        Cmd::Config { tz } => {
            if let Some(tz_str) = tz {
                tz_str.parse::<chrono_tz::Tz>()
                    .map_err(|_| anyhow::anyhow!("invalid timezone: {tz_str}"))?;
                store.settings.default_tz = Some(tz_str.clone());
                store.save(&cli.data_file)?;
                println!("default timezone set to {tz_str}");
            } else {
                let tz = store.settings.default_tz.as_deref().unwrap_or("UTC (no default set)");
                println!("default timezone: {tz}");
            }
        }
    }
    Ok(())
}
