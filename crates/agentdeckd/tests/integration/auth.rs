//! Bearer auth on every endpoint (spec §2): every operation in `openapi.json` answers 401
//! before it looks at the request's path, query or body.

use reqwest::Method;
use serde_json::Value;

use crate::support::TestDaemon;

const OPENAPI: &str = include_str!("../../../../openapi.json");

/// Each `(method, path)` in `openapi.json`, with every `{param}` filled by an id.
fn operations() -> Vec<(Method, String)> {
    let spec: Value = serde_json::from_str(OPENAPI).expect("openapi.json parses");
    let id = uuid::Uuid::now_v7().to_string();
    let mut operations = Vec::new();
    for (path, item) in spec["paths"].as_object().expect("paths") {
        let path = path.replace("{id}", &id);
        for method in item.as_object().expect("a path item").keys() {
            let method = Method::from_bytes(method.to_uppercase().as_bytes()).expect("a method");
            operations.push((method, path.clone()));
        }
    }
    assert!(operations.len() >= 20, "{operations:?}");
    operations
}

async fn status_of_every_operation(
    daemon: &TestDaemon,
    authorization: Option<&str>,
) -> Vec<String> {
    let http = reqwest::Client::new();
    let mut wrong = Vec::new();
    for (method, path) in operations() {
        let mut request = http
            .request(method.clone(), format!("{}{path}", daemon.base_url()))
            // Not valid JSON for any body: auth must be checked first.
            .header("content-type", "application/json")
            .body("not json");
        if let Some(value) = authorization {
            request = request.header("authorization", value);
        }
        let response = request.send().await.expect("a response");
        let status = response.status().as_u16();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status != 401 || body["error_code"] != "unauthorized" {
            wrong.push(format!("{method} {path}: {status} {body}"));
        }
    }
    wrong
}

mod every_operation {
    use super::*;

    #[tokio::test]
    async fn should_reject_a_request_without_a_token_401() {
        let daemon = TestDaemon::start().await;
        let wrong = status_of_every_operation(&daemon, None).await;
        assert!(wrong.is_empty(), "not 401 unauthorized: {wrong:#?}");
    }

    #[tokio::test]
    async fn should_reject_a_wrong_token_401() {
        let daemon = TestDaemon::start().await;
        let wrong = status_of_every_operation(&daemon, Some("Bearer not-the-token")).await;
        assert!(wrong.is_empty(), "not 401 unauthorized: {wrong:#?}");
    }

    #[tokio::test]
    async fn should_reject_the_token_without_the_bearer_scheme_401() {
        let daemon = TestDaemon::start().await;
        let token = daemon.info.token.clone();
        let wrong = status_of_every_operation(&daemon, Some(&token)).await;
        assert!(wrong.is_empty(), "not 401 unauthorized: {wrong:#?}");
    }
}
