//! Optional canonical-incident fence at the existing external transaction boundary.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;
use url::Url;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    #[serde(deserialize_with = "status_urls")]
    pub status_urls: Vec<Url>,
    pub fuel_chain_id: String,
    pub writer_identity: String,
    pub fuel_url: Url,
    pub generation: String,
    #[serde(default = "freshness")]
    pub freshness_seconds: u64,
}
fn freshness() -> u64 {
    60
}
fn status_urls<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Vec<Url>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Routes {
        List(Vec<Url>),
        Csv(String),
    }
    match Routes::deserialize(deserializer)? {
        Routes::List(routes) => Ok(routes),
        Routes::Csv(value) => value
            .split(',')
            .map(|url| url.trim().parse().map_err(serde::de::Error::custom))
            .collect(),
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn error(message: &str) -> Error {
    Error::Other(message.into())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct State {
    generation: String,
    fuel_chain_id: String,
    writer_identity: String,
    incident: Option<String>,
    height: Option<u64>,
    hash: Option<String>,
    progressed_at: u64,
    #[serde(default)]
    recoveries: Vec<Value>,
}

/// Durable writer-local state in the committer's existing PostgreSQL database.
#[async_trait::async_trait]
pub trait Journal: std::fmt::Debug + Send + Sync {
    /// Load using the exclusively held writer session; errors close the posting gate.
    async fn load(&self) -> Result<Value>;
    /// Durably replace safety state before allowing another external post.
    async fn save(&self, state: Value) -> Result<()>;
}

#[derive(Debug)]
pub struct Gate {
    config: Config,
    client: reqwest::Client,
    state: Mutex<State>,
    journal: Arc<dyn Journal>,
}

impl Gate {
    pub async fn open(config: Config, journal: Arc<dyn Journal>) -> Result<Self> {
        if config.status_urls.is_empty()
            || config.generation.is_empty()
            || config.freshness_seconds == 0
        {
            return Err(error("Invalid FCR committer configuration"));
        }
        let state: State = serde_json::from_value(journal.load().await?)
            .map_err(|_| error("Missing/corrupt committer safety state; initialize explicitly"))?;
        if state.generation != config.generation
            || state.fuel_chain_id != config.fuel_chain_id
            || state.writer_identity != config.writer_identity
        {
            return Err(error(
                "FCR committer identity changed; explicit recovery required",
            ));
        }
        Ok(Self {
            config,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
                .map_err(|_| error("Cannot create FCR client"))?,
            state: Mutex::new(state),
            journal,
        })
    }

    async fn save(&self, state: &State) -> Result<()> {
        self.journal
            .save(
                serde_json::to_value(state)
                    .map_err(|_| error("Cannot encode committer safety state"))?,
            )
            .await
    }

    async fn query(&self, url: &Url, query: &str) -> Result<Value> {
        let response = self
            .client
            .post(url.clone())
            .json(&json!({"query":query}))
            .send()
            .await
            .map_err(|_| error("FCR source unavailable"))?;
        if !response.status().is_success() {
            return Err(error("FCR source unavailable"));
        }
        let body: Value = response
            .json()
            .await
            .map_err(|_| error("Invalid FCR source"))?;
        if body.get("errors").is_some() {
            return Err(error("FCR GraphQL query failed"));
        }
        Ok(body)
    }

    /// Runs immediately before signing/sending, including blob replacements. Never clears a latch.
    pub async fn check(&self) -> Result<()> {
        let mut state = self.state.lock().await;
        // This also proves that the exclusive PostgreSQL writer session is still alive.
        let stored: State = serde_json::from_value(self.journal.load().await?)
            .map_err(|_| error("Corrupt committer safety state"))?;
        if stored.generation != state.generation
            || stored.fuel_chain_id != state.fuel_chain_id
            || stored.writer_identity != state.writer_identity
        {
            return Err(error(
                "Committer safety identity changed; restart after explicit recovery",
            ));
        }
        if let Some(incident) = stored.incident {
            state.incident = Some(incident);
        }
        if state.incident.is_some() {
            return Err(error(
                "Canonical incident or Fuel recovery holds external posts",
            ));
        }
        let mut fresh = false;
        for url in &self.config.status_urls {
            if let Ok(body) = self.query(url, "{nodeInfo{ethereumFinality}}").await {
                let status = &body["data"]["nodeInfo"]["ethereumFinality"];
                if let Some(incident) = status["incident"].as_str() {
                    state.incident = Some(incident.into());
                    self.save(&state).await?;
                }
                if !status["enabled"].is_boolean()
                    || (status["enabled"] == true
                        && status["generation"].as_str().is_none_or(str::is_empty))
                {
                    continue;
                }
                if status["enabled"] == true
                    && status["generation"].as_str() != Some(self.config.generation.as_str())
                {
                    state.incident = Some("authority-recovery-generation-changed".into());
                    self.save(&state).await?;
                }
                fresh |= status["enabled"] == true
                    && status["checked_at"].as_u64().is_some_and(|at| {
                        at <= now().saturating_add(5)
                            && now().saturating_sub(at) <= self.config.freshness_seconds
                    });
            }
        }
        if state.incident.is_some() {
            return Err(error("Canonical incident or recovery holds external posts"));
        }
        if !fresh {
            return Err(error("No fresh canonical safety source"));
        }
        let body = self
            .query(
                &self.config.fuel_url,
                "{chain{latestBlock{id height} consensusParameters{chainId}}}",
            )
            .await?;
        let chain_id = &body["data"]["chain"]["consensusParameters"]["chainId"];
        if chain_id.as_str() != Some(self.config.fuel_chain_id.as_str()) {
            return Err(error("Fuel chain ID mismatch"));
        }
        let block = &body["data"]["chain"]["latestBlock"];
        let height = block["height"]
            .as_str()
            .and_then(|value| value.parse::<u64>().ok())
            .or_else(|| block["height"].as_u64())
            .ok_or_else(|| error("Missing Fuel height"))?;
        let hash = block["id"]
            .as_str()
            .filter(|hash| hash.len() == 66 && hash.starts_with("0x"))
            .ok_or_else(|| error("Missing Fuel hash"))?;
        if let Some(previous) = state.height {
            if height < previous {
                return Err(error("Fuel source is behind retained high-water mark"));
            }
            let old = self
                .query(
                    &self.config.fuel_url,
                    &format!("{{block(height:\"{previous}\"){{id}}}}"),
                )
                .await?;
            let old_hash = old["data"]["block"]["id"]
                .as_str()
                .ok_or_else(|| error("Fuel high-water identity unavailable"))?;
            if Some(old_hash) != state.hash.as_deref() {
                state.incident = Some("fuel-high-water-identity-changed".into());
                self.save(&state).await?;
                return Err(error("Fuel history changed; explicit recovery required"));
            }
            if height == previous
                && now().saturating_sub(state.progressed_at) > self.config.freshness_seconds
            {
                return Err(error("Fuel tip is not advancing"));
            }
        }
        if state.height.is_none_or(|previous| height > previous) {
            state.height = Some(height);
            state.hash = Some(hash.into());
            state.progressed_at = now();
            self.save(&state).await?;
        }
        Ok(())
    }
}
