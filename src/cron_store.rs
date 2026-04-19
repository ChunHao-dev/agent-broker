use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Schedule {
    #[serde(rename = "every")]
    Every { interval_secs: u64 },
    #[serde(rename = "cron")]
    Cron { expr: String, #[serde(default)] tz: Option<String> },
    #[serde(rename = "at")]
    At { at: DateTime<Utc> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryTarget {
    pub source: String,
    pub channel_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobHealth {
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub last_run: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub target: DeliveryTarget,
    pub schedule: Schedule,
    pub prompt: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub once: bool,
    #[serde(default)]
    pub failed: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub health: JobHealth,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CronSettings {
    #[serde(default)]
    pub default_tz: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CronStore {
    #[serde(default)]
    pub settings: CronSettings,
    pub jobs: HashMap<String, CronJob>,
}

#[allow(dead_code)]
impl CronStore {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }

    /// Atomic read-modify-write with file lock.
    pub fn mutate_locked(path: &Path, f: impl FnOnce(&mut Self)) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.lock_exclusive()?;
        let mut data = String::new();
        (&file).read_to_string(&mut data)?;
        let mut store = if data.is_empty() {
            Self::default()
        } else {
            serde_json::from_str(&data)?
        };
        f(&mut store);
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&store)?)?;
        std::fs::rename(&tmp, path)?;
        file.unlock()?;
        Ok(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_path() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("cron_test_{}.json", uuid::Uuid::new_v4()));
        p
    }

    fn make_job(id: &str, once: bool, failed: bool) -> CronJob {
        CronJob {
            id: id.to_string(),
            target: DeliveryTarget {
                source: "discord".to_string(),
                channel_id: "123".to_string(),
            },
            schedule: Schedule::Every { interval_secs: 60 },
            prompt: "test".to_string(),
            created_at: Utc::now(),
            once,
            failed,
            paused: false,
            health: JobHealth::default(),
        }
    }

    #[test]
    fn test_load_empty_file() {
        let path = temp_path();
        let store = CronStore::load(&path).unwrap();
        assert!(store.jobs.is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_mutate_and_load() {
        let path = temp_path();
        CronStore::mutate_locked(&path, |store| {
            store.jobs.insert("j1".into(), make_job("j1", false, false));
        }).unwrap();

        let loaded = CronStore::load(&path).unwrap();
        assert_eq!(loaded.jobs.len(), 1);
        assert_eq!(loaded.jobs["j1"].prompt, "test");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn test_remove_job() {
        let path = temp_path();
        CronStore::mutate_locked(&path, |store| {
            store.jobs.insert("j1".into(), make_job("j1", false, false));
            store.jobs.insert("j2".into(), make_job("j2", false, false));
        }).unwrap();

        CronStore::mutate_locked(&path, |store| {
            store.jobs.remove("j1");
        }).unwrap();

        let loaded = CronStore::load(&path).unwrap();
        assert_eq!(loaded.jobs.len(), 1);
        assert!(!loaded.jobs.contains_key("j1"));
        assert!(loaded.jobs.contains_key("j2"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn test_mark_job_failed() {
        let path = temp_path();
        CronStore::mutate_locked(&path, |store| {
            store.jobs.insert("j1".into(), make_job("j1", true, false));
        }).unwrap();

        CronStore::mutate_locked(&path, |store| {
            store.jobs.get_mut("j1").unwrap().failed = true;
        }).unwrap();

        let loaded = CronStore::load(&path).unwrap();
        assert!(loaded.jobs["j1"].failed);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn test_failed_job_not_picked_up() {
        let store = CronStore {
            settings: CronSettings::default(),
            jobs: [
                ("j1".into(), make_job("j1", true, true)),
                ("j2".into(), make_job("j2", false, false)),
            ].into_iter().collect(),
        };

        let active: Vec<_> = store.jobs.values().filter(|j| !j.failed).collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "j2");
    }

    #[test]
    fn test_failed_field_defaults_false() {
        let json = r#"{
            "settings": {},
            "jobs": {
                "j1": {
                    "id": "j1",
                    "target": {"source": "discord", "channel_id": "123"},
                    "schedule": {"kind": "every", "interval_secs": 60},
                    "prompt": "test",
                    "created_at": "2026-01-01T00:00:00Z",
                    "once": false
                }
            }
        }"#;
        let store: CronStore = serde_json::from_str(json).unwrap();
        assert!(!store.jobs["j1"].failed);
    }

    #[test]
    fn test_default_tz_setting() {
        let path = temp_path();
        CronStore::mutate_locked(&path, |store| {
            store.settings.default_tz = Some("Asia/Taipei".into());
        }).unwrap();

        let loaded = CronStore::load(&path).unwrap();
        assert_eq!(loaded.settings.default_tz.as_deref(), Some("Asia/Taipei"));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn test_pause_and_resume() {
        let path = temp_path();
        CronStore::mutate_locked(&path, |store| {
            store.jobs.insert("j1".into(), make_job("j1", false, false));
        }).unwrap();

        CronStore::mutate_locked(&path, |store| {
            store.jobs.get_mut("j1").unwrap().paused = true;
        }).unwrap();

        let loaded = CronStore::load(&path).unwrap();
        assert!(loaded.jobs["j1"].paused);

        CronStore::mutate_locked(&path, |store| {
            let job = store.jobs.get_mut("j1").unwrap();
            job.paused = false;
            job.health.consecutive_failures = 0;
        }).unwrap();

        let loaded = CronStore::load(&path).unwrap();
        assert!(!loaded.jobs["j1"].paused);
        assert_eq!(loaded.jobs["j1"].health.consecutive_failures, 0);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn test_paused_job_not_picked_up() {
        let mut paused_job = make_job("j1", false, false);
        paused_job.paused = true;

        let store = CronStore {
            settings: CronSettings::default(),
            jobs: [
                ("j1".into(), paused_job),
                ("j2".into(), make_job("j2", false, false)),
            ].into_iter().collect(),
        };

        let active: Vec<_> = store.jobs.values().filter(|j| !j.paused && !j.failed).collect();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "j2");
    }

    #[test]
    fn test_job_health_persistence() {
        let path = temp_path();
        CronStore::mutate_locked(&path, |store| {
            let mut job = make_job("j1", false, false);
            job.health.consecutive_failures = 2;
            job.health.last_status = Some("failed".into());
            job.health.last_run = Some(Utc::now());
            store.jobs.insert("j1".into(), job);
        }).unwrap();

        let loaded = CronStore::load(&path).unwrap();
        assert_eq!(loaded.jobs["j1"].health.consecutive_failures, 2);
        assert_eq!(loaded.jobs["j1"].health.last_status.as_deref(), Some("failed"));
        assert!(loaded.jobs["j1"].health.last_run.is_some());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn test_old_json_without_paused_and_health() {
        let json = r#"{
            "settings": {},
            "jobs": {
                "j1": {
                    "id": "j1",
                    "target": {"source": "discord", "channel_id": "123"},
                    "schedule": {"kind": "every", "interval_secs": 60},
                    "prompt": "test",
                    "created_at": "2026-01-01T00:00:00Z",
                    "once": false,
                    "failed": false
                }
            }
        }"#;
        let store: CronStore = serde_json::from_str(json).unwrap();
        let job = &store.jobs["j1"];
        assert!(!job.paused);
        assert_eq!(job.health.consecutive_failures, 0);
        assert!(job.health.last_status.is_none());
        assert!(job.health.last_run.is_none());
    }

    #[test]
    fn test_health_resets_on_success() {
        let path = temp_path();
        CronStore::mutate_locked(&path, |store| {
            let mut job = make_job("j1", false, false);
            job.health.consecutive_failures = 2;
            job.health.last_status = Some("failed".into());
            store.jobs.insert("j1".into(), job);
        }).unwrap();

        CronStore::mutate_locked(&path, |store| {
            let job = store.jobs.get_mut("j1").unwrap();
            job.health.consecutive_failures = 0;
            job.health.last_status = Some("ok".into());
            job.health.last_run = Some(Utc::now());
        }).unwrap();

        let loaded = CronStore::load(&path).unwrap();
        assert_eq!(loaded.jobs["j1"].health.consecutive_failures, 0);
        assert_eq!(loaded.jobs["j1"].health.last_status.as_deref(), Some("ok"));
        std::fs::remove_file(&path).unwrap();
    }
}
