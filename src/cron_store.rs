use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CronJob {
    pub id: String,
    pub target: DeliveryTarget,
    pub schedule: Schedule,
    pub prompt: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub once: bool,
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

impl CronStore {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&data)?)
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}
