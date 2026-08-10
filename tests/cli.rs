use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::time::Duration;
use tempfile::TempDir;
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bazaardb-cli"))
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn available_loopback_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn search_uses_the_documented_endpoint_and_offline_cache() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/search_cards"))
        .and(header("X-API-Key", "test-key"))
        .and(query_param("query", "sword"))
        .and(query_param("category", "items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "page": 0,
                "limit": 25,
                "count": 1,
                "total": 1,
                "cards": [{"id": "card-1", "name": "Sword of Swords", "type": "Item"}]
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    command()
        .args([
            "--api-base",
            &server.uri(),
            "--api-key",
            "test-key",
            "--cache-dir",
            cache.path().to_str().unwrap(),
            "search",
            "sword",
            "--category",
            "items",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Sword of Swords"))
        .stdout(predicate::str::contains("\"miss\""));

    command()
        .args([
            "--api-base",
            &server.uri(),
            "--cache-dir",
            cache.path().to_str().unwrap(),
            "--cache-mode",
            "offline",
            "search",
            "sword",
            "--category",
            "items",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Sword of Swords"))
        .stdout(predicate::str::contains("\"offline\""));
}

#[tokio::test]
async fn get_card_preserves_the_complete_card_payload() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/get_card"))
        .and(query_param("name", "Bar of Soap"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "id": "soap-id",
                "name": "Bar of Soap",
                "tiers": {"Bronze": {"active_tooltips": [0]}},
                "unknown_future_field": {"retained": true}
            }
        })))
        .expect(1)
        .mount(&server)
        .await;

    command()
        .args([
            "--api-base",
            &server.uri(),
            "--api-key",
            "test-key",
            "--cache-dir",
            cache.path().to_str().unwrap(),
            "get",
            "Bar of Soap",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("unknown_future_field"))
        .stdout(predicate::str::contains("retained"));
}

#[tokio::test]
async fn search_all_fetches_and_orders_every_page() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    for (page, name) in [(0, "Alpha"), (1, "Bravo"), (2, "Charlie")] {
        Mock::given(method("GET"))
            .and(path("/search_cards"))
            .and(query_param("page", page.to_string()))
            .and(query_param("limit", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "page": page,
                    "limit": 1,
                    "count": 1,
                    "total": 3,
                    "cards": [{"id": format!("card-{page}"), "name": name}]
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
    }

    let output = command()
        .args([
            "--api-base",
            &server.uri(),
            "--api-key",
            "test-key",
            "--cache-dir",
            cache.path().to_str().unwrap(),
            "search",
            "--limit",
            "1",
            "--all",
            "--concurrency",
            "3",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let data = &value["data"];
    assert_eq!(data["pages_fetched"], 3);
    assert_eq!(data["count"], 3);
    assert_eq!(data["cards"][0]["name"], "Alpha");
    assert_eq!(data["cards"][1]["name"], "Bravo");
    assert_eq!(data["cards"][2]["name"], "Charlie");
}

#[tokio::test]
async fn search_all_honors_page_offset_and_page_budget() {
    let server = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    for (page, name) in [(2, "Charlie"), (3, "Delta")] {
        Mock::given(method("GET"))
            .and(path("/search_cards"))
            .and(query_param("page", page.to_string()))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "page": page,
                    "limit": 1,
                    "count": 1,
                    "total": 6,
                    "cards": [{"id": format!("card-{page}"), "name": name}]
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
    }

    command()
        .args([
            "--api-base",
            &server.uri(),
            "--api-key",
            "test-key",
            "--cache-dir",
            cache.path().to_str().unwrap(),
            "search",
            "--page",
            "2",
            "--limit",
            "1",
            "--all",
            "--max-pages",
            "2",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Charlie"))
        .stdout(predicate::str::contains("Delta"))
        .stdout(predicate::str::contains("\"pages_fetched\": 2"));
}

#[tokio::test]
async fn serve_exposes_cua_state_with_etag_on_loopback() {
    let provider = MockServer::start().await;
    let cache = TempDir::new().unwrap();
    Mock::given(method("GET"))
        .and(path("/search_cards"))
        .and(query_param("query", "sword"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {
                "page": 0,
                "limit": 25,
                "count": 1,
                "total": 1,
                "cards": [{"id": "card-1", "name": "Sword of Swords"}]
            }
        })))
        .expect(1)
        .mount(&provider)
        .await;

    let port = available_loopback_port();
    let child = ProcessCommand::new(env!("CARGO_BIN_EXE_bazaardb-cli"))
        .args([
            "--api-base",
            &provider.uri(),
            "--api-key",
            "test-key",
            "--cache-dir",
            cache.path().to_str().unwrap(),
            "serve",
            "sword",
            "--port",
            &port.to_string(),
            "--refresh-seconds",
            "300",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _guard = ChildGuard(child);
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}/v1/state");
    let response = {
        let mut response = None;
        for _ in 0..50 {
            if let Ok(candidate) = client.get(&url).send().await
                && candidate.status().is_success()
            {
                response = Some(candidate);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        response.expect("CUA state server did not start")
    };
    let etag = response.headers()[reqwest::header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    let value: serde_json::Value = response.json().await.unwrap();
    assert_eq!(value["schema_version"], "1.0.0");
    assert_eq!(value["tick"], 1);
    assert_eq!(value["data"]["cards"][0]["name"], "Sword of Swords");

    let cached = client
        .get(url)
        .header(reqwest::header::IF_NONE_MATCH, etag)
        .send()
        .await
        .unwrap();
    assert_eq!(cached.status(), reqwest::StatusCode::NOT_MODIFIED);
}

#[test]
fn endpoints_and_cache_status_do_not_require_credentials() {
    let cache = TempDir::new().unwrap();
    command()
        .arg("endpoints")
        .assert()
        .success()
        .stdout(predicate::str::contains("search_cards"))
        .stdout(predicate::str::contains("get_card"));

    command()
        .args([
            "--cache-dir",
            cache.path().to_str().unwrap(),
            "cache",
            "status",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"entries\": 0"));
}

#[test]
fn missing_api_key_is_a_structured_error() {
    let cache = TempDir::new().unwrap();
    command()
        .args([
            "--api-base",
            "http://127.0.0.1:9/",
            "--cache-dir",
            cache.path().to_str().unwrap(),
            "search",
            "sword",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("BAZAARDB_API_KEY"));
}
