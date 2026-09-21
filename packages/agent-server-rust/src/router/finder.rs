use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};

use crate::tools::finder_short_link::{
    browser_status, resolve_short_link, show_browser, FinderShortLinkError,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortLinkInput {
    object_id: String,
    object_nonce_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ShortLinkOutput<'a> {
    object_id: &'a str,
    short_url: String,
}

pub async fn short_link(Json(input): Json<ShortLinkInput>) -> impl IntoResponse {
    match resolve_short_link(&input.object_id, &input.object_nonce_id).await {
        Ok(short_url) => (
            StatusCode::OK,
            Json(
                serde_json::to_value(ShortLinkOutput {
                    object_id: &input.object_id,
                    short_url,
                })
                .expect("short-link response must serialize"),
            ),
        ),
        Err(error) => {
            let status = match error {
                FinderShortLinkError::InvalidObjectId | FinderShortLinkError::InvalidNonceId => {
                    StatusCode::UNPROCESSABLE_ENTITY
                }
                FinderShortLinkError::SessionUnavailable => StatusCode::SERVICE_UNAVAILABLE,
                _ => StatusCode::BAD_GATEWAY,
            };
            (status, Json(serde_json::json!({"error": error.code()})))
        }
    }
}

pub async fn session_status() -> impl IntoResponse {
    browser_response(browser_status().await)
}

pub async fn show_session() -> impl IntoResponse {
    browser_response(show_browser().await)
}

fn browser_response(
    result: Result<crate::tools::finder_short_link::FinderBrowserStatus, FinderShortLinkError>,
) -> (StatusCode, Json<serde_json::Value>) {
    match result {
        Ok(status) => (
            StatusCode::OK,
            Json(serde_json::to_value(status).expect("browser status must serialize")),
        ),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"status": "unavailable"})),
        ),
    }
}
