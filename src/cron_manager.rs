use crate::cron_store::{CronJob, CronStore, Schedule};
use crate::acp::{classify_notification, AcpEvent, ContentBlock};
use crate::acp::connection::AcpConnection;
use crate::config::AgentConfig;
use serenity::http::Http;
use serenity::model::id::ChannelId;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

// --- Delivery trait ---

#[async_trait::async_trait]
pub trait CronDelivery: Send + Sync {
    async fn send(&self, channel_id: &str, text: &str) -> anyhow::Result<()>;
    fn is_channel_gone(&self, err: &anyhow::Error) -> bool;
}

pub struct DiscordDelivery {
    pub http: Arc<Http>,
}

#[async_trait::async_trait]
impl CronDelivery for DiscordDelivery {
    async fn send(&self, channel_id: &str, text: &str) -> anyhow::Result<()> {
        let id: u64 = channel_id.parse()?;
        ChannelId::new(id).say(&self.http, text).await?;
        Ok(())
    }

    fn is_channel_gone(&self, err: &anyhow::Error) -> bool {
        format!("{err}").contains("Unknown Channel")
    }
}

// --- CronManager ---

pub struct CronManager {
    data_file: PathBuf,
    agent_config: AgentConfig,
    deliveries: HashMap<String, Arc<dyn CronDelivery>>,
    handles: HashMap<String, JoinHandle<()>>,
}

impl CronManager {
    pub fn new(data_file: PathBuf, agent_config: AgentConfig) -> Self {
        Self { data_file, agent_config, deliveries: HashMap::new(), handles: HashMap::new() }
    }

    pub fn register_delivery(&mut self, source: &str, delivery: Arc<dyn CronDelivery>) {
        self.deliveries.insert(source.to_string(), delivery);
    }

    pub async fn start(mut self, reload_secs: u64) {
        info!(data_file = %self.data_file.display(), "cron manager starting");
        let deliveries = Arc::new(self.deliveries.clone());
        loop {
            if let Err(e) = self.sync(&deliveries).await {
                error!(error = %e, "cron sync failed");
            }
            tokio::time::sleep(std::time::Duration::from_secs(reload_secs)).await;
        }
    }

    async fn sync(&mut self, deliveries: &Arc<HashMap<String, Arc<dyn CronDelivery>>>) -> anyhow::Result<()> {
        let store = CronStore::load(&self.data_file)?;
        let file_ids: HashSet<String> = store.jobs.keys().cloned().collect();
        let running_ids: HashSet<String> = self.handles.keys().cloned().collect();

        for id in &running_ids {
            if !file_ids.contains(id) {
                if let Some(h) = self.handles.remove(id) {
                    info!(job_id = %id, "cancelling removed job");
                    h.abort();
                }
            }
        }

        for (id, job) in &store.jobs {
            if !self.handles.contains_key(id) {
                let handle = spawn_job(
                    job.clone(),
                    self.agent_config.clone(),
                    deliveries.clone(),
                    self.data_file.clone(),
                );
                info!(job_id = %id, source = %job.target.source, channel = %job.target.channel_id, "started job");
                self.handles.insert(id.clone(), handle);
            }
        }

        Ok(())
    }
}

fn next_delay(schedule: &Schedule) -> Option<std::time::Duration> {
    match schedule {
        Schedule::Every { interval_secs } => {
            Some(std::time::Duration::from_secs(*interval_secs))
        }
        Schedule::Cron { expr, tz } => {
            let cron_schedule: cron::Schedule = expr.parse().ok()?;
            let now = if let Some(tz_str) = tz {
                let tz: chrono_tz::Tz = tz_str.parse().ok()?;
                let next = cron_schedule.upcoming(tz).next()?;
                next.with_timezone(&chrono::Utc)
            } else {
                cron_schedule.upcoming(chrono::Utc).next()?
            };
            let delta = now - chrono::Utc::now();
            Some(delta.to_std().unwrap_or(std::time::Duration::from_secs(1)))
        }
        Schedule::At { at } => {
            let delta = *at - chrono::Utc::now();
            if delta.num_seconds() <= 0 {
                Some(std::time::Duration::from_secs(0))
            } else {
                Some(delta.to_std().unwrap_or(std::time::Duration::from_secs(1)))
            }
        }
    }
}

fn spawn_job(
    job: CronJob,
    agent_config: AgentConfig,
    deliveries: Arc<HashMap<String, Arc<dyn CronDelivery>>>,
    data_file: PathBuf,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let delivery = match deliveries.get(&job.target.source) {
            Some(d) => d.clone(),
            None => {
                error!(job_id = %job.id, source = %job.target.source, "no delivery registered for source");
                return;
            }
        };

        let delay = match next_delay(&job.schedule) {
            Some(d) => d,
            None => {
                error!(job_id = %job.id, "cannot compute next run time");
                return;
            }
        };
        tokio::time::sleep(delay).await;

        loop {
            info!(job_id = %job.id, once = job.once, "executing cron job");

            match execute_prompt(&agent_config, &format!("[Scheduled task at {}] {}", chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"), job.prompt)).await {
                Ok(text) => {
                    let reply = if text.is_empty() { "(no response)".into() } else { text };
                    if let Err(e) = delivery.send(&job.target.channel_id, &reply).await {
                        if delivery.is_channel_gone(&e) {
                            warn!(job_id = %job.id, "channel gone, removing job");
                            remove_job_from_file(&job.id, &data_file).ok();
                            return;
                        }
                        error!(job_id = %job.id, error = %e, "failed to post cron result");
                    }
                }
                Err(e) => {
                    let msg = format!("⚠️ Cron job `{}` failed: {e}", job.id);
                    let _ = delivery.send(&job.target.channel_id, &msg).await;
                }
            }

            if job.once {
                info!(job_id = %job.id, "one-time job done, removing");
                remove_job_from_file(&job.id, &data_file).ok();
                return;
            }

            let delay = match next_delay(&job.schedule) {
                Some(d) => d,
                None => {
                    error!(job_id = %job.id, "cannot compute next run time, stopping");
                    return;
                }
            };
            tokio::time::sleep(delay).await;
        }
    })
}

async fn execute_prompt(agent_config: &AgentConfig, prompt: &str) -> anyhow::Result<String> {
    let mut conn = AcpConnection::spawn(
        &agent_config.command,
        &agent_config.args,
        &agent_config.working_dir,
        &agent_config.env,
    ).await?;

    conn.initialize().await?;
    conn.session_new(&agent_config.working_dir).await?;

    let blocks = vec![ContentBlock::Text { text: prompt.to_string() }];
    let (mut rx, _) = conn.session_prompt(blocks).await?;

    let mut buf = String::new();
    while let Some(msg) = rx.recv().await {
        if msg.id.is_some() {
            if let Some(err) = &msg.error {
                return Err(anyhow::anyhow!("{err}"));
            }
            break;
        }
        if let Some(AcpEvent::Text(t)) = classify_notification(&msg) {
            buf.push_str(&t);
        }
    }
    conn.prompt_done().await;
    Ok(buf)
}

fn remove_job_from_file(job_id: &str, data_file: &std::path::Path) -> anyhow::Result<()> {
    let mut store = CronStore::load(data_file)?;
    store.jobs.remove(job_id);
    store.save(data_file)?;
    Ok(())
}
