//! HTTP client to a data-generator's internal API (loopback).

use mp_contracts::{InternalGeneratorPrepare, InternalGeneratorStatus};

#[derive(Clone)]
pub struct GenClient {
    http: reqwest::Client,
    base: String,
}

impl GenClient {
    pub fn new(base: impl Into<String>) -> Self {
        Self { http: reqwest::Client::new(), base: base.into() }
    }

    pub async fn prepare(&self, body: &InternalGeneratorPrepare) -> Result<String, String> {
        let resp = self
            .http
            .post(format!("{}/internal/generators", self.base))
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("prepare {}: {}", resp.status(), resp.text().await.unwrap_or_default()));
        }
        let v: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
        v["generator_id"].as_str().map(str::to_string).ok_or_else(|| "no generator_id".into())
    }

    pub async fn status(&self, generator_id: &str) -> Result<InternalGeneratorStatus, String> {
        let resp = self
            .http
            .get(format!("{}/internal/generators/{generator_id}", self.base))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("status {}", resp.status()));
        }
        resp.json().await.map_err(|e| e.to_string())
    }

    pub async fn stop(&self, generator_id: &str) -> Result<InternalGeneratorStatus, String> {
        let resp = self
            .http
            .post(format!("{}/internal/generators/{generator_id}/stop", self.base))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("stop {}", resp.status()));
        }
        resp.json().await.map_err(|e| e.to_string())
    }
}
