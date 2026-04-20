use crate::cron_store::{CronJob, CronStore, Schedule};
use crate::acp::{classify_notification, AcpEvent, ContentBlock};
use crate::acp::connection::AcpConnection;
use crate::config::AgentConfig;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serenity::http::Http;
use serenity::model::id::ChannelId;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

// --- Error classification (5.4) ---

#[derive(Debug, Clone, Copy)]
pub enum CronErrorKind {
    Spawn,
    Agent,
    Timeout,
    Delivery,
}

impl std::fmt::Display for CronErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn => write!(f, "spawn failure"),
            Self::Agent => write!(f, "agent error"),
            Self::Timeout => write!(f, "execution timeout"),
            Self::Delivery => write!(f, "delivery failure"),
        }
    }
}

pub struct CronError {
    pub kind: CronErrorKind,
    pub inner: anyhow::Error,
}

impl std::fmt::Display for CronError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.kind, self.inner)
    }
}

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

// --- Delivery with retry (5.3) ---

const DELIVERY_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

async fn send_with_retry(
    delivery: &dyn CronDelivery,
    channel_id: &str,
    text: &str,
) -> Result<(), CronError> {
    match delivery.send(channel_id, text).await {
        Ok(()) => Ok(()),
        Err(first_err) => {
            warn!(error = %first_err, "delivery failed, retrying in 5s");
            tokio::time::sleep(DELIVERY_RETRY_DELAY).await;
            delivery.send(channel_id, text).await.map_err(|e| CronError {
                kind: CronErrorKind::Delivery,
                inner: e,
            })
        }
    }
}

// --- PromptExecutor trait ---

#[async_trait::async_trait]
pub trait PromptExecutor: Send + Sync {
    async fn execute(&self, prompt: &str) -> anyhow::Result<String>;
}

pub struct AcpExecutor {
    pub agent_config: AgentConfig,
}

#[async_trait::async_trait]
impl PromptExecutor for AcpExecutor {
    async fn execute(&self, prompt: &str) -> anyhow::Result<String> {
        execute_prompt(&self.agent_config, prompt).await
    }
}

// --- CronManager ---

const EXEC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
const BASE_SYNC_INTERVAL: u64 = 30;
const MAX_SYNC_INTERVAL: u64 = 120;

pub struct CronManager {
    data_file: PathBuf,
    executor: Arc<dyn PromptExecutor>,
    deliveries: HashMap<String, Arc<dyn CronDelivery>>,
    handles: HashMap<String, JoinHandle<()>>,
}

impl CronManager {
    pub fn new(data_file: PathBuf, executor: Arc<dyn PromptExecutor>) -> Self {
        Self { data_file, executor, deliveries: HashMap::new(), handles: HashMap::new() }
    }

    pub fn register_delivery(&mut self, source: &str, delivery: Arc<dyn CronDelivery>) {
        self.deliveries.insert(source.to_string(), delivery);
    }

    // 5.6: exponential backoff for sync failures + file watch for immediate sync
    pub async fn start(mut self, reload_secs: u64) {
        info!(data_file = %self.data_file.display(), "cron manager starting");
        let deliveries = Arc::new(self.deliveries.clone());
        let base = if reload_secs > 0 { reload_secs } else { BASE_SYNC_INTERVAL };
        let mut consecutive_sync_failures: u32 = 0;

        // Set up file watcher
        let (watch_tx, mut watch_rx) = tokio::sync::mpsc::channel::<()>(1);
        let watch_path = self.data_file.clone();
        let _watcher = setup_file_watcher(&watch_path, watch_tx);

        loop {
            match self.sync(&deliveries).await {
                Ok(()) => consecutive_sync_failures = 0,
                Err(e) => {
                    consecutive_sync_failures += 1;
                    error!(error = %e, failures = consecutive_sync_failures, "cron sync failed");
                }
            }
            let backoff = sync_backoff(base, consecutive_sync_failures);

            // Wait for file change OR fallback timeout
            tokio::select! {
                _ = watch_rx.recv() => {
                    // File changed, sync immediately
                    // Small debounce to coalesce rapid writes
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(backoff)) => {
                    // Fallback poll
                }
            }
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
            if job.paused || job.failed {
                continue;
            }
            if !self.handles.contains_key(id) {
                let handle = spawn_job(
                    job.clone(),
                    self.executor.clone(),
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

fn sync_backoff(base: u64, consecutive_failures: u32) -> u64 {
    if consecutive_failures > 0 {
        (base * 2u64.saturating_pow(consecutive_failures - 1)).min(MAX_SYNC_INTERVAL)
    } else {
        base
    }
}

fn setup_file_watcher(path: &PathBuf, tx: tokio::sync::mpsc::Sender<()>) -> Option<RecommendedWatcher> {
    let watch_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let file_name = path.file_name().map(|f| f.to_os_string());

    let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
        if let Ok(event) = res {
            let dominated = matches!(
                event.kind,
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            );
            if !dominated {
                return;
            }
            // Only trigger for our specific file
            let matches = file_name.as_ref().map_or(true, |name| {
                event.paths.iter().any(|p| p.file_name().map(|f| f == name.as_os_str()).unwrap_or(false))
            });
            if matches {
                let _ = tx.try_send(());
            }
        }
    }).ok()?;

    watcher.watch(&watch_dir, RecursiveMode::NonRecursive).ok()?;
    info!(path = %watch_dir.display(), "file watcher active");
    Some(watcher)
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
    executor: Arc<dyn PromptExecutor>,
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

            // 5.1: timeout wraps execute; AcpConnection::drop kills process group
            let result = tokio::time::timeout(
                EXEC_TIMEOUT,
                executor.execute(&format!(
                    "[Scheduled task at {}] {}",
                    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
                    job.prompt
                )),
            ).await;

            // 5.4: classify the error
            let exec_result = match result {
                Ok(Ok(text)) => Ok(text),
                Ok(Err(e)) => {
                    let kind = classify_exec_error(&e);
                    Err(CronError { kind, inner: e })
                }
                Err(_) => Err(CronError {
                    kind: CronErrorKind::Timeout,
                    inner: anyhow::anyhow!("execution timed out after {}s", EXEC_TIMEOUT.as_secs()),
                }),
            };

            match exec_result {
                Ok(text) => {
                    // 5.2: reset failure count on success
                    update_job_health(&job.id, &data_file, true).ok();
                    let reply = if text.is_empty() { "(no response)".into() } else { text };
                    // 5.3: retry delivery once
                    if let Err(e) = send_with_retry(delivery.as_ref(), &job.target.channel_id, &reply).await {
                        if delivery.is_channel_gone(&e.inner) {
                            warn!(job_id = %job.id, "channel gone, removing job");
                            remove_job_from_file(&job.id, &data_file).ok();
                            return;
                        }
                        error!(job_id = %job.id, error = %e, "delivery failed after retry");
                    }
                    if job.once {
                        info!(job_id = %job.id, "one-time job done, removing");
                        remove_job_from_file(&job.id, &data_file).ok();
                        return;
                    }
                }
                Err(e) => {
                    // 5.2: track consecutive failures
                    let failures = update_job_health(&job.id, &data_file, false)
                        .unwrap_or(1);
                    warn!(job_id = %job.id, kind = %e.kind, failures, error = %e.inner, "cron job failed");

                    if job.once {
                        let msg = format!("⚠️ Cron job `{}` failed ({}): {}", job.id, e.kind, e.inner);
                        let _ = delivery.send(&job.target.channel_id, &msg).await;
                        mark_job_failed(&job.id, &data_file).ok();
                        return;
                    }

                    // recurring: log error and continue to next schedule
                    let msg = format!(
                        "⚠️ Cron job `{}` failed ({}×) — {}: {}",
                        job.id, failures, e.kind, e.inner
                    );
                    let _ = delivery.send(&job.target.channel_id, &msg).await;
                }
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

// 5.4: classify spawn vs agent errors
fn classify_exec_error(err: &anyhow::Error) -> CronErrorKind {
    let msg = format!("{err}");
    if msg.contains("failed to spawn") {
        CronErrorKind::Spawn
    } else {
        CronErrorKind::Agent
    }
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

// 5.2: update health, returns current consecutive failure count
fn update_job_health(job_id: &str, data_file: &std::path::Path, success: bool) -> anyhow::Result<u32> {
    let mut failures = 0;
    CronStore::mutate_locked(data_file, |store| {
        if let Some(job) = store.jobs.get_mut(job_id) {
            if success {
                job.health.consecutive_failures = 0;
                job.health.last_status = Some("ok".into());
            } else {
                job.health.consecutive_failures += 1;
                job.health.last_status = Some("failed".into());
            }
            job.health.last_run = Some(chrono::Utc::now());
            failures = job.health.consecutive_failures;
        }
    })?;
    Ok(failures)
}

fn remove_job_from_file(job_id: &str, data_file: &std::path::Path) -> anyhow::Result<()> {
    CronStore::mutate_locked(data_file, |store| {
        store.jobs.remove(job_id);
    })?;
    Ok(())
}

fn mark_job_failed(job_id: &str, data_file: &std::path::Path) -> anyhow::Result<()> {
    CronStore::mutate_locked(data_file, |store| {
        if let Some(job) = store.jobs.get_mut(job_id) {
            job.failed = true;
            job.health.last_status = Some("failed".into());
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cron_store::{CronJob, CronMode, CronStore, DeliveryTarget, JobHealth, Schedule};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_path() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("cron_mgr_test_{}.json", uuid::Uuid::new_v4()));
        p
    }

    fn make_job(id: &str) -> CronJob {
        CronJob {
            id: id.to_string(),
            target: DeliveryTarget { source: "discord".into(), channel_id: "123".into() },
            schedule: Schedule::Every { interval_secs: 60 },
            prompt: "test".into(),
            created_at: chrono::Utc::now(),
            once: false,
            failed: false,
            paused: false,
            health: JobHealth::default(),
            mode: CronMode::default(),
            heartbeat_file: None,
        }
    }

    fn seed(path: &PathBuf, id: &str) {
        CronStore::mutate_locked(path, |store| {
            store.jobs.insert(id.into(), make_job(id));
        }).unwrap();
    }

    // --- MockDelivery ---

    struct MockDelivery {
        fail_count: AtomicU32,
        channel_gone: bool,
    }

    impl MockDelivery {
        fn ok() -> Self {
            Self { fail_count: AtomicU32::new(0), channel_gone: false }
        }
        fn fail_n_times(n: u32) -> Self {
            Self { fail_count: AtomicU32::new(n), channel_gone: false }
        }
        fn gone() -> Self {
            Self { fail_count: AtomicU32::new(2), channel_gone: true }
        }
    }

    #[async_trait::async_trait]
    impl CronDelivery for MockDelivery {
        async fn send(&self, _channel_id: &str, _text: &str) -> anyhow::Result<()> {
            let remaining = self.fail_count.load(Ordering::SeqCst);
            if remaining > 0 {
                self.fail_count.fetch_sub(1, Ordering::SeqCst);
                if self.channel_gone {
                    anyhow::bail!("Unknown Channel");
                }
                anyhow::bail!("mock delivery error");
            }
            Ok(())
        }
        fn is_channel_gone(&self, err: &anyhow::Error) -> bool {
            format!("{err}").contains("Unknown Channel")
        }
    }

    // --- update_job_health ---

    #[test]
    fn test_health_increments_on_failure() {
        let path = temp_path();
        seed(&path, "j1");

        let f1 = update_job_health("j1", &path, false).unwrap();
        assert_eq!(f1, 1);
        let f2 = update_job_health("j1", &path, false).unwrap();
        assert_eq!(f2, 2);
        let f3 = update_job_health("j1", &path, false).unwrap();
        assert_eq!(f3, 3);

        let store = CronStore::load(&path).unwrap();
        assert_eq!(store.jobs["j1"].health.consecutive_failures, 3);
        assert_eq!(store.jobs["j1"].health.last_status.as_deref(), Some("failed"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn test_health_resets_on_success() {
        let path = temp_path();
        seed(&path, "j1");

        update_job_health("j1", &path, false).unwrap();
        update_job_health("j1", &path, false).unwrap();
        let f = update_job_health("j1", &path, true).unwrap();
        assert_eq!(f, 0);

        let store = CronStore::load(&path).unwrap();
        assert_eq!(store.jobs["j1"].health.consecutive_failures, 0);
        assert_eq!(store.jobs["j1"].health.last_status.as_deref(), Some("ok"));
        assert!(store.jobs["j1"].health.last_run.is_some());
        std::fs::remove_file(&path).unwrap();
    }

    // --- remove_job_from_file ---

    #[test]
    fn test_remove_job_from_file() {
        let path = temp_path();
        seed(&path, "j1");

        remove_job_from_file("j1", &path).unwrap();

        let store = CronStore::load(&path).unwrap();
        assert!(!store.jobs.contains_key("j1"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn test_remove_nonexistent_job_is_ok() {
        let path = temp_path();
        seed(&path, "j1");

        // Removing a job that doesn't exist should not error
        remove_job_from_file("nope", &path).unwrap();

        let store = CronStore::load(&path).unwrap();
        assert_eq!(store.jobs.len(), 1);
        std::fs::remove_file(&path).unwrap();
    }

    // --- mark_job_failed ---

    #[test]
    fn test_mark_job_failed() {
        let path = temp_path();
        seed(&path, "j1");

        mark_job_failed("j1", &path).unwrap();

        let store = CronStore::load(&path).unwrap();
        assert!(store.jobs["j1"].failed);
        assert_eq!(store.jobs["j1"].health.last_status.as_deref(), Some("failed"));
        std::fs::remove_file(&path).unwrap();
    }

    // --- send_with_retry ---

    #[tokio::test]
    async fn test_send_with_retry_success() {
        let delivery = MockDelivery::ok();
        let result = send_with_retry(&delivery, "123", "hello").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_send_with_retry_fail_then_success() {
        let delivery = MockDelivery::fail_n_times(1);
        let result = send_with_retry(&delivery, "123", "hello").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_send_with_retry_both_fail() {
        let delivery = MockDelivery::fail_n_times(2);
        let result = send_with_retry(&delivery, "123", "hello").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err().kind, CronErrorKind::Delivery));
    }

    #[tokio::test]
    async fn test_send_with_retry_channel_gone() {
        let delivery = MockDelivery::gone();
        let result = send_with_retry(&delivery, "123", "hello").await;
        // Retry will also fail with Unknown Channel
        assert!(result.is_err());
        assert!(delivery.is_channel_gone(&result.unwrap_err().inner));
    }

    // --- classify_exec_error ---

    #[test]
    fn test_classify_spawn_error() {
        let err = anyhow::anyhow!("failed to spawn kiro-cli: No such file");
        assert!(matches!(classify_exec_error(&err), CronErrorKind::Spawn));
    }

    #[test]
    fn test_classify_agent_error() {
        let err = anyhow::anyhow!("API key expired");
        assert!(matches!(classify_exec_error(&err), CronErrorKind::Agent));
    }

    // --- MockExecutor ---

    struct OkExecutor(String);
    #[async_trait::async_trait]
    impl PromptExecutor for OkExecutor {
        async fn execute(&self, _prompt: &str) -> anyhow::Result<String> {
            Ok(self.0.clone())
        }
    }

    struct FailExecutor;
    #[async_trait::async_trait]
    impl PromptExecutor for FailExecutor {
        async fn execute(&self, _prompt: &str) -> anyhow::Result<String> {
            anyhow::bail!("agent crashed");
        }
    }

    struct HangExecutor;
    #[async_trait::async_trait]
    impl PromptExecutor for HangExecutor {
        async fn execute(&self, _prompt: &str) -> anyhow::Result<String> {
            tokio::time::sleep(std::time::Duration::from_secs(9999)).await;
            Ok(String::new())
        }
    }

    struct FailThenOkExecutor {
        calls: AtomicU32,
        fail_first_n: u32,
    }
    impl FailThenOkExecutor {
        fn new(fail_first_n: u32) -> Self {
            Self { calls: AtomicU32::new(0), fail_first_n }
        }
    }
    #[async_trait::async_trait]
    impl PromptExecutor for FailThenOkExecutor {
        async fn execute(&self, _prompt: &str) -> anyhow::Result<String> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_first_n {
                anyhow::bail!("agent crashed");
            }
            Ok("success".into())
        }
    }

    // --- timeout ---

    #[tokio::test]
    async fn test_timeout_triggers() {
        let executor = HangExecutor;
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            executor.execute("test"),
        ).await;
        assert!(result.is_err()); // Elapsed = timeout
    }

    #[tokio::test]
    async fn test_no_timeout_on_fast_response() {
        let executor = OkExecutor("done".into());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            executor.execute("test"),
        ).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().unwrap(), "done");
    }

    // --- spawn_job integration ---

    fn make_once_job(id: &str) -> CronJob {
        let mut job = make_job(id);
        job.once = true;
        job.schedule = Schedule::Every { interval_secs: 0 };
        job
    }

    fn setup_spawn(job: CronJob, executor: Arc<dyn PromptExecutor>, delivery: Arc<dyn CronDelivery>, path: &PathBuf) -> JoinHandle<()> {
        let mut deliveries = HashMap::new();
        deliveries.insert("discord".to_string(), delivery);
        spawn_job(job, executor, Arc::new(deliveries), path.clone())
    }

    #[tokio::test]
    async fn test_spawn_job_success_removes_once_job() {
        let path = temp_path();
        let job = make_once_job("j1");
        seed(&path, "j1");

        let handle = setup_spawn(
            job,
            Arc::new(OkExecutor("result".into())),
            Arc::new(MockDelivery::ok()),
            &path,
        );
        handle.await.unwrap();

        let store = CronStore::load(&path).unwrap();
        assert!(!store.jobs.contains_key("j1")); // removed after success
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn test_spawn_job_empty_response() {
        let path = temp_path();
        let job = make_once_job("j1");
        seed(&path, "j1");

        let delivery = Arc::new(MockDelivery::ok());
        let handle = setup_spawn(
            job,
            Arc::new(OkExecutor(String::new())), // empty response
            delivery,
            &path,
        );
        handle.await.unwrap();

        // Job should still complete and be removed
        let store = CronStore::load(&path).unwrap();
        assert!(!store.jobs.contains_key("j1"));
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn test_spawn_job_failure_marks_once_job_failed() {
        let path = temp_path();
        let job = make_once_job("j1");
        seed(&path, "j1");

        let handle = setup_spawn(
            job,
            Arc::new(FailExecutor),
            Arc::new(MockDelivery::ok()),
            &path,
        );
        handle.await.unwrap();

        let store = CronStore::load(&path).unwrap();
        assert!(store.jobs["j1"].failed);
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn test_spawn_job_channel_gone_removes_job() {
        let path = temp_path();
        let job = make_once_job("j1");
        seed(&path, "j1");

        let handle = setup_spawn(
            job,
            Arc::new(OkExecutor("result".into())),
            Arc::new(MockDelivery::gone()), // channel deleted
            &path,
        );
        handle.await.unwrap();

        let store = CronStore::load(&path).unwrap();
        assert!(!store.jobs.contains_key("j1")); // removed
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn test_recurring_continues_after_failure() {
        let path = temp_path();
        let mut job = make_job("j1");
        job.schedule = Schedule::Every { interval_secs: 1 };
        seed(&path, "j1");

        let executor = Arc::new(FailThenOkExecutor::new(2)); // fail 2 times, then succeed
        let handle = setup_spawn(
            job,
            executor.clone(),
            Arc::new(MockDelivery::ok()),
            &path,
        );

        // Each iteration: sleep(1s) + execute + sleep(1s) + execute + ...
        // Advance and yield repeatedly to let the spawned task progress
        for _ in 0..10 {
            tokio::time::advance(std::time::Duration::from_secs(1)).await;
            tokio::task::yield_now().await;
        }

        let store = CronStore::load(&path).unwrap();
        assert!(store.jobs.contains_key("j1"));
        assert!(!store.jobs["j1"].paused);
        assert!(!store.jobs["j1"].failed);
        assert_eq!(store.jobs["j1"].health.last_status.as_deref(), Some("ok"));
        assert_eq!(store.jobs["j1"].health.consecutive_failures, 0);

        handle.abort();
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn test_spawn_job_no_delivery_registered() {
        let path = temp_path();
        let mut job = make_once_job("j1");
        job.target.source = "slack".into(); // no slack delivery registered
        seed(&path, "j1");

        let mut deliveries = HashMap::new();
        deliveries.insert("discord".to_string(), Arc::new(MockDelivery::ok()) as Arc<dyn CronDelivery>);

        let handle = spawn_job(
            job,
            Arc::new(OkExecutor("result".into())),
            Arc::new(deliveries),
            path.clone(),
        );
        handle.await.unwrap();

        // Job should still exist (wasn't processed, just skipped)
        let store = CronStore::load(&path).unwrap();
        assert!(store.jobs.contains_key("j1"));
        std::fs::remove_file(&path).unwrap();
    }

    // --- sync_backoff ---

    #[test]
    fn test_sync_backoff_no_failures() {
        assert_eq!(sync_backoff(30, 0), 30);
    }

    #[test]
    fn test_sync_backoff_escalates() {
        assert_eq!(sync_backoff(30, 1), 30);   // 30 * 2^0
        assert_eq!(sync_backoff(30, 2), 60);   // 30 * 2^1
        assert_eq!(sync_backoff(30, 3), 120);  // 30 * 2^2
    }

    #[test]
    fn test_sync_backoff_caps_at_max() {
        assert_eq!(sync_backoff(30, 4), 120);  // 30 * 2^3 = 240, capped to 120
        assert_eq!(sync_backoff(30, 10), 120); // still capped
    }

    #[test]
    fn test_sync_backoff_resets_on_success() {
        // After failures, success resets to 0 → backoff returns base
        assert_eq!(sync_backoff(30, 3), 120);
        assert_eq!(sync_backoff(30, 0), 30); // reset
    }

    // --- setup_file_watcher ---

    #[tokio::test]
    async fn test_file_watcher_triggers_on_target_file() {
        let dir = std::env::temp_dir().join(format!("fw_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("cron_jobs.json");
        std::fs::write(&target, "{}").unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
        let _watcher = setup_file_watcher(&target, tx);
        assert!(_watcher.is_some(), "watcher should be created");

        // Small delay for watcher to register
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Write to target file
        std::fs::write(&target, r#"{"settings":{},"jobs":{}}"#).unwrap();

        // Should receive notification within 2 seconds
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            rx.recv(),
        ).await;
        assert!(result.is_ok(), "should receive file change notification");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn test_file_watcher_ignores_other_files() {
        let dir = std::env::temp_dir().join(format!("fw_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("cron_jobs.json");
        std::fs::write(&target, "{}").unwrap();

        let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
        let _watcher = setup_file_watcher(&target, tx);

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Write to a different file in the same directory
        std::fs::write(dir.join("other.txt"), "hello").unwrap();

        // Should NOT receive notification
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            rx.recv(),
        ).await;
        assert!(result.is_err(), "should not trigger for other files");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
