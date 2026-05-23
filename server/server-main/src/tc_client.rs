//! HTTP client to test-control's internal API (loopback).

use mp_contracts::{InternalStartTest, InternalTestStatus};

pub struct TcClient {
    http: reqwest::Client,
    base: String,
}

impl TcClient {
    pub fn new(base: impl Into<String>) -> Self {
        Self { http: reqwest::Client::new(), base: base.into() }
    }

    pub async fn start_test(&self, body: &InternalStartTest) -> Result<(), String> {
        let resp = self
            .http
            .post(format!("{}/internal/tests", self.base))
            .json(body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(format!("start_test {}: {}", resp.status(), resp.text().await.unwrap_or_default()))
        }
    }

    pub async fn test_status(&self, test_id: &str) -> Result<InternalTestStatus, String> {
        let resp = self
            .http
            .get(format!("{}/internal/tests/{test_id}", self.base))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("test_status {}", resp.status()));
        }
        resp.json().await.map_err(|e| e.to_string())
    }
}
