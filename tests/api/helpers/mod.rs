pub mod user;

use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use hyper::{Body, Request};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use tower::ServiceExt;

use crate::helper::TestApp;

/// HTTP response for test
#[derive(Debug)]
pub struct TestResponse {
    pub status_code: StatusCode,
    pub headers: HashMap<String, String>,
    pub body: Value,
}

impl TestResponse {
    /// Create a new `TestResponse`
    pub async fn new(app: &TestApp, url: &str, method: &str, body: Option<String>, token: Option<&str>) -> Self {
        let mut request = Request::builder()
            .uri(url)
            .method(method)
            .header("Content-Type", "application/json");
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }

        let request = request.body(match body {
            None => Body::empty(),
            Some(body) => body.into(),
        });

        let response = app.router.clone().oneshot(request.unwrap()).await.unwrap();

        let status_code = response.status();
        let body = hyper::body::to_bytes(response.into_body())
            .await
            .expect("failed to convert body into bytes");
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

        TestResponse {
            status_code,
            body,
            headers: HashMap::new(),
        }
    }
}

#[derive(Deserialize, Debug)]
pub struct TestUser {
    pub id: String,
    pub lastname: String,
    pub firstname: String,
    pub username: String,
    pub roles: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TestUser {
    pub fn from_body(body: &str) -> Self {
        serde_json::from_str(body).expect("error when deserializing body")
    }
}

#[derive(Deserialize, Debug)]
pub struct TestPasswordReset {
    pub token: String,
    pub expired_at: DateTime<Utc>,
}

impl TestPasswordReset {
    pub fn from_body(body: &str) -> Self {
        serde_json::from_str(body).expect("error when deserializing body")
    }
}
