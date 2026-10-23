use super::*;
use wechat_core::GroupMutation;

pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/v1/accounts/{a}/groups", post(create))
        .route("/v1/accounts/{a}/groups/{id}", axum::routing::patch(rename))
        .route("/v1/accounts/{a}/groups/{id}/leave", post(leave))
        .route(
            "/v1/accounts/{a}/groups/{room}/members",
            post(add).delete(remove),
        )
        .route(
            "/v1/accounts/{a}/groups/{id}/members/{username}",
            get(member),
        )
        .route(
            "/v1/accounts/{a}/groups/{id}/my-name",
            axum::routing::put(my_name),
        )
        .route(
            "/v1/accounts/{a}/groups/{id}/announcement",
            axum::routing::delete(clear_announcement),
        )
        .route(
            "/v1/accounts/{a}/threads/{id}/notifications",
            axum::routing::put(notifications),
        )
        .route(
            "/v1/accounts/{a}/group-mutations/by-idempotency-key",
            get(by_key),
        )
}
async fn submit(
    app: App,
    c: Claims,
    a: String,
    headers: HeaderMap,
    action: GroupMutation,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "admin")?;
    let key = idempotency_key(&headers)?;
    let op = app.core.mutate_group(&a, action, key).await?;
    Ok((
        StatusCode::ACCEPTED,
        checked::<contracts::NativeOperation>(
            serde_json::to_value(op).map_err(anyhow::Error::from)?,
        )?,
    ))
}
async fn create(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    headers: HeaderMap,
    Json(input): Json<contracts::NativeGroupCreateRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    submit(
        app,
        c,
        a,
        headers,
        GroupMutation::Create {
            members: input.members,
            name: input.name,
        },
    )
    .await
}
async fn rename(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<contracts::NativeGroupNameRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    submit(
        app,
        c,
        a,
        headers,
        GroupMutation::Rename {
            group_id: id,
            name: input.name,
        },
    )
    .await
}
async fn leave(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<Value>)> {
    submit(app, c, a, headers, GroupMutation::Leave { group_id: id }).await
}
async fn add(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<contracts::NativeGroupMembersRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    submit(
        app,
        c,
        a,
        headers,
        GroupMutation::AddMembers {
            group_id: id,
            members: input.members,
        },
    )
    .await
}
async fn remove(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<contracts::NativeGroupMembersRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    submit(
        app,
        c,
        a,
        headers,
        GroupMutation::RemoveMembers {
            group_id: id,
            members: input.members,
        },
    )
    .await
}
async fn my_name(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<contracts::NativeGroupMyNameRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    submit(
        app,
        c,
        a,
        headers,
        GroupMutation::MyName {
            group_id: id,
            display_name: input.display_name,
        },
    )
    .await
}
async fn clear_announcement(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<Value>)> {
    submit(
        app,
        c,
        a,
        headers,
        GroupMutation::ClearAnnouncement { group_id: id },
    )
    .await
}
async fn notifications(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<contracts::NativeThreadNotificationsRequest>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    submit(
        app,
        c,
        a,
        headers,
        GroupMutation::Mute {
            thread_id: id,
            muted: input.muted,
        },
    )
    .await
}
async fn member(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id, username)): Path<(String, String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    let guard = app.core.lock_account(&a).await?;
    let value = work(move || {
        let _guard = guard;
        app.core.group_member(&a, &id, &username)
    })
    .await?;
    checked::<contracts::NativeGroupMemberLookup>(value)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Key {
    key: String,
}
async fn by_key(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    Query(q): Query<Key>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "admin")?;
    let op = app
        .core
        .store
        .group_mutation_by_key(&a, &q.key, None)?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))?
        .operation;
    checked::<contracts::NativeOperation>(serde_json::to_value(op).map_err(anyhow::Error::from)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    #[tokio::test]
    async fn every_group_mutation_requires_account_admin_and_a_key() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&temp.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let token = "0123456789abcdef0123456789abcdef";
        let read = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned();
        store
            .issue_token(
                "reader",
                &format!("{:x}", Sha256::digest(read.as_bytes())),
                &["one".into()],
                &["read".into()],
                None,
            )
            .unwrap();
        let app = crate::router(Core::new(store, None).unwrap(), token);
        for (method, path, body) in [
            ("POST", "groups", json!({"members":["peer"]})),
            ("PATCH", "groups/room%40chatroom", json!({"name":"New"})),
            ("POST", "groups/room%40chatroom/leave", json!({})),
            (
                "POST",
                "groups/room%40chatroom/members",
                json!({"members":["peer"]}),
            ),
            (
                "DELETE",
                "groups/room%40chatroom/members",
                json!({"members":["peer"]}),
            ),
            (
                "PUT",
                "groups/room%40chatroom/my-name",
                json!({"displayName":"Me"}),
            ),
            ("DELETE", "groups/room%40chatroom/announcement", json!({})),
            (
                "PUT",
                "threads/room%40chatroom/notifications",
                json!({"muted":true}),
            ),
        ] {
            for (bearer, key, status) in [
                (&read, Some("key"), StatusCode::FORBIDDEN),
                (&token.to_string(), None, StatusCode::BAD_REQUEST),
            ] {
                let mut request = Request::builder()
                    .method(method)
                    .uri(format!("/v1/accounts/one/{path}"))
                    .header("authorization", format!("Bearer {bearer}"))
                    .header("content-type", "application/json");
                if let Some(key) = key {
                    request = request.header("idempotency-key", key);
                }
                let response = app
                    .clone()
                    .oneshot(request.body(Body::from(body.to_string())).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), status, "{method} {path}");
            }
        }
    }
}
