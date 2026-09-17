use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{header, StatusCode};
use codespace_domain::{Profile, WorkspaceId};
use codespace_policy::{Registry, Workspace};
use codespace_server::config::{HttpConfig, INBOX_PATH};
use codespace_server::http::http_router;
use codespace_store::Store;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

async fn spawn_inbox(token: Option<&str>) -> (tempfile::TempDir, SocketAddr, Arc<Store>) {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let mut registry = Registry::new();
    registry.insert(Workspace {
        id: WorkspaceId("demo".into()),
        root: ws,
        profile: Profile::WorkspaceWrite,
    });
    let store = Arc::new(Store::memory().expect("store"));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let app = http_router(
        &HttpConfig {
            host: "127.0.0.1".into(),
            port: addr.port(),
            bearer_token: token.map(str::to_string),
        },
        registry,
        store.clone(),
        CancellationToken::new(),
    );
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (root, addr, store)
}

#[tokio::test]
async fn inbox_bearer_rejects_before_minting_intents() {
    let token = "test-token-do-not-log";
    let (_root, addr, store) = spawn_inbox(Some(token)).await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let response = client
        .post(format!(
            "http://{addr}{INBOX_PATH}/works/work-missing/intents"
        ))
        .header(header::CONTENT_TYPE, "application/json")
        .body(r#"{"body":"should not persist"}"#)
        .send()
        .await
        .expect("unauthorized post");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response.text().await.expect("body");
    assert!(!body.contains(token));
    assert!(body.contains("unauthorized"));
    assert!(store.list_works(None).unwrap().is_empty());
}

#[tokio::test]
async fn inbox_draft_stays_off_the_model_queue_until_queued() {
    let (_root, addr, store) = spawn_inbox(None).await;
    let opened = store
        .open_work(&WorkspaceId("demo".into()), Some("auth".into()))
        .unwrap();
    let client = reqwest::Client::new();
    let created = client
        .post(format!(
            "http://{addr}{INBOX_PATH}/works/{}/intents",
            opened.work_id.0
        ))
        .json(&serde_json::json!({ "body": "later", "delivery": "next_checkpoint" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let draft: serde_json::Value = created.json().await.unwrap();
    assert_eq!(draft["state"], "draft");
    let status = store.steer_status(&opened.work_id).unwrap();
    assert_eq!(status.queued, 0);
    assert!(store.claim_next(&opened.work_id).unwrap().item.is_none());

    let queued = client
        .post(format!(
            "http://{addr}{INBOX_PATH}/intents/{}/queue",
            draft["intent_id"].as_str().unwrap()
        ))
        .json(&serde_json::json!({ "revision": draft["revision"] }))
        .send()
        .await
        .unwrap();
    assert!(queued.status().is_success());
    let status = store.steer_status(&opened.work_id).unwrap();
    assert_eq!(status.queued, 1);
}

#[tokio::test]
async fn inbox_stop_does_not_use_ordinary_feedback_queue_edit() {
    let (_root, addr, store) = spawn_inbox(None).await;
    let opened = store.open_work(&WorkspaceId("demo".into()), None).unwrap();
    let client = reqwest::Client::new();
    let stopped = client
        .post(format!(
            "http://{addr}{INBOX_PATH}/works/{}/stop",
            opened.work_id.0
        ))
        .json(&serde_json::json!({ "reason": "wrong target" }))
        .send()
        .await
        .unwrap();
    assert!(stopped.status().is_success());
    let body: serde_json::Value = stopped.json().await.unwrap();
    assert_eq!(body["intent"]["state"], "claimed");
    assert_eq!(body["intent"]["kind"], "stop_notice");
    let edit = client
        .patch(format!(
            "http://{addr}{INBOX_PATH}/intents/{}",
            body["intent"]["intent_id"].as_str().unwrap()
        ))
        .json(&serde_json::json!({
            "revision": body["intent"]["revision"],
            "body": "rewrite stop"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(edit.status(), StatusCode::CONFLICT);
    let err: serde_json::Value = edit.json().await.unwrap();
    assert_eq!(err["code"], "INTENT_ALREADY_CLAIMED");
}
