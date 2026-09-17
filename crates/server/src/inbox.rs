//! User Inbox HTTP API. Not MCP. Same optional Bearer as `/mcp`.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use codespace_domain::{
    DeliveryPolicy, ErrorBody, ErrorCode, IntentId, IntentKind, UserIntent, WorkId,
};
use codespace_store::CreateIntent;
use serde::{Deserialize, Serialize};

use codespace_runner::Runner;

use crate::mcp::CodeSpace;

pub fn router(handler: CodeSpace) -> Router {
    Router::new()
        .route("/inbox/works", get(list_works))
        .route("/inbox/works/{id}", get(get_work))
        .route(
            "/inbox/works/{id}/intents",
            get(list_intents).post(create_draft),
        )
        .route("/inbox/works/{id}/reorder", post(reorder))
        .route("/inbox/works/{id}/stop", post(stop_work))
        .route("/inbox/intents/{id}", patch(edit_intent))
        .route("/inbox/intents/{id}/queue", post(queue_intent))
        .route("/inbox/intents/{id}/cancel", post(cancel_intent))
        .with_state(handler)
}

#[derive(Debug, Deserialize)]
struct ListWorksQuery {
    workspace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CreateDraftBody {
    body: String,
    #[serde(default)]
    kind: IntentKind,
    #[serde(default)]
    delivery: DeliveryPolicy,
}

#[derive(Debug, Deserialize)]
struct EditBody {
    revision: u64,
    body: String,
    #[serde(default)]
    delivery: Option<DeliveryPolicy>,
}

#[derive(Debug, Deserialize)]
struct RevisionBody {
    revision: u64,
}

#[derive(Debug, Deserialize)]
struct ReorderBody {
    intent_ids: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct StopBody {
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct StopResult {
    ok: bool,
    killed: u32,
    intent: UserIntent,
}

async fn list_works(
    State(handler): State<CodeSpace>,
    Query(query): Query<ListWorksQuery>,
) -> Result<Json<serde_json::Value>, InboxError> {
    let works = handler
        .store
        .list_works(query.workspace_id.as_deref())
        .map_err(InboxError)?;
    Ok(Json(
        serde_json::to_value(works).unwrap_or(serde_json::json!([])),
    ))
}

async fn get_work(
    State(handler): State<CodeSpace>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, InboxError> {
    let work = handler.store.get_work(&WorkId(id)).map_err(InboxError)?;
    Ok(Json(
        serde_json::to_value(work).unwrap_or(serde_json::json!({})),
    ))
}

async fn list_intents(
    State(handler): State<CodeSpace>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, InboxError> {
    let intents = handler
        .store
        .list_intents(&WorkId(id), true)
        .map_err(InboxError)?;
    Ok(Json(
        serde_json::to_value(intents).unwrap_or(serde_json::json!([])),
    ))
}

async fn create_draft(
    State(handler): State<CodeSpace>,
    Path(id): Path<String>,
    Json(body): Json<CreateDraftBody>,
) -> Result<(StatusCode, Json<UserIntent>), InboxError> {
    let intent = handler
        .store
        .create_draft(
            &WorkId(id),
            CreateIntent {
                body: body.body,
                kind: body.kind,
                delivery: body.delivery,
            },
        )
        .map_err(InboxError)?;
    Ok((StatusCode::CREATED, Json(intent)))
}

async fn edit_intent(
    State(handler): State<CodeSpace>,
    Path(id): Path<String>,
    Json(body): Json<EditBody>,
) -> Result<Json<UserIntent>, InboxError> {
    handler
        .store
        .edit_intent(&IntentId(id), body.revision, body.body, body.delivery)
        .map(Json)
        .map_err(InboxError)
}

async fn queue_intent(
    State(handler): State<CodeSpace>,
    Path(id): Path<String>,
    Json(body): Json<RevisionBody>,
) -> Result<Json<UserIntent>, InboxError> {
    handler
        .store
        .queue_intent(&IntentId(id), body.revision)
        .map(Json)
        .map_err(InboxError)
}

async fn cancel_intent(
    State(handler): State<CodeSpace>,
    Path(id): Path<String>,
) -> Result<Json<UserIntent>, InboxError> {
    handler
        .store
        .cancel_intent(&IntentId(id))
        .map(Json)
        .map_err(InboxError)
}

async fn reorder(
    State(handler): State<CodeSpace>,
    Path(id): Path<String>,
    Json(body): Json<ReorderBody>,
) -> Result<Json<serde_json::Value>, InboxError> {
    let ids: Vec<IntentId> = body.intent_ids.into_iter().map(IntentId).collect();
    let intents = handler
        .store
        .reorder_queued(&WorkId(id), &ids)
        .map_err(InboxError)?;
    Ok(Json(
        serde_json::to_value(intents).unwrap_or(serde_json::json!([])),
    ))
}

async fn stop_work(
    State(handler): State<CodeSpace>,
    Path(id): Path<String>,
    body: Option<Json<StopBody>>,
) -> Result<Json<StopResult>, InboxError> {
    let work_id = WorkId(id);
    let work = handler.store.get_work(&work_id).map_err(InboxError)?;
    let killed = handler
        .runner
        .terminate_workspace(&work.workspace_id.0)
        .map_err(InboxError)?;
    let reason = body.and_then(|Json(b)| b.reason).unwrap_or_default();
    let intent = handler
        .store
        .insert_stop_notice(&work_id, &reason)
        .map_err(InboxError)?;
    Ok(Json(StopResult {
        ok: true,
        killed,
        intent,
    }))
}

struct InboxError(ErrorBody);

impl IntoResponse for InboxError {
    fn into_response(self) -> Response {
        let status = match self.0.code {
            ErrorCode::WorkNotFound | ErrorCode::IntentNotFound => StatusCode::NOT_FOUND,
            ErrorCode::IntentAlreadyClaimed
            | ErrorCode::IntentRevisionConflict
            | ErrorCode::WorkClosed
            | ErrorCode::QueueNotEmpty => StatusCode::CONFLICT,
            ErrorCode::Unauthorized => StatusCode::FORBIDDEN,
            _ => StatusCode::BAD_REQUEST,
        };
        (status, Json(self.0)).into_response()
    }
}
