use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, warn};

#[derive(Debug, Serialize)]
struct EmbedRequest<'a> {
    text: &'a str,
}

#[derive(Debug, Deserialize)]
struct EmbedResponse {
    embedding: Vec<f32>,
}

#[derive(Clone)]
pub struct EmbedderClient {
    client: reqwest::Client,
    base_url: Option<String>,
}

impl EmbedderClient {
    pub fn new(base_url: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap_or_default();

        Self { client, base_url }
    }

    /// Checks whether the embedding server is active and reachable
    pub async fn is_available(&self) -> bool {
        if let Some(base) = &self.base_url {
            let health_url = format!("{}/", base.trim_end_matches('/'));
            if let Ok(res) = self.client.get(&health_url).send().await {
                return res.status().is_success();
            }
        }
        false
    }

    /// Generates a vector embedding for the input text using the embedding service
    pub async fn embed(&self, text: &str) -> Option<Vec<f32>> {
        let base = self.base_url.as_ref()?;
        let target_url = format!("{}/embed", base.trim_end_matches('/'));

        let payload = EmbedRequest { text };
        match self.client.post(&target_url).json(&payload).send().await {
            Ok(res) if res.status().is_success() => {
                match res.json::<EmbedResponse>().await {
                    Ok(data) => Some(data.embedding),
                    Err(e) => {
                        warn!("Failed parsing embedding response from {}: {}", target_url, e);
                        None
                    }
                }
            }
            Ok(res) => {
                debug!("Embedding service returned status {}", res.status());
                None
            }
            Err(e) => {
                debug!("Embedding service at {} unreachable: {}", target_url, e);
                None
            }
        }
    }
}
