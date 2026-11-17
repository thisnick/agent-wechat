use super::*;
use wechat_core::ContactMutation;
pub(super) fn routes() -> Router<App> {
    Router::new()
        .api_route("/v1/accounts/{a}/search/contacts", post(search))
        .api_route("/v1/accounts/{a}/contact-requests", get(list).post(send))
        .api_route(
            "/v1/accounts/{a}/contact-requests/{id}",
            get(one).delete(delete),
        )
        .api_route(
            "/v1/accounts/{a}/contact-requests/{id}/accept",
            post(accept),
        )
        .api_route("/v1/accounts/{a}/contacts/{id}/privacy", get(privacy))
        .api_route(
            "/v1/accounts/{a}/contacts/{id}",
            axum::routing::delete(remove).patch(update_contact),
        )
        .api_route(
            "/v1/accounts/{a}/contact-mutations/by-idempotency-key",
            get(by_key),
        )
}
async fn search(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    Json(input): Json<contracts::NativeContactSearchRequest>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    checked::<contracts::NativeContactSearch>(app.core.search_contacts(&a, &input.query).await?)
}
async fn list(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    Query(q): Query<contracts::NativeContactRequestQuery>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    let q = wechat_core::ContactRequestQuery {
        direction: q.direction,
        actionable: q.actionable,
        search: q.search,
        limit: q.limit,
        cursor: q.cursor,
    };
    let value = app
        .core
        .read(&a.clone(), move |core| core.contact_requests(&a, &q))
        .await?;
    checked::<contracts::NativeContactRequestPage>(value)
}
async fn one(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "read")?;
    let value = app
        .core
        .read(&a.clone(), move |core| core.contact_request(&a, &id))
        .await?;
    checked::<contracts::NativeContactRequest>(value)
}
async fn submit(
    app: App,
    c: Claims,
    a: String,
    headers: HeaderMap,
    action: ContactMutation,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "admin")?;
    let op = app
        .core
        .mutate_contact(&a, action, idempotency_key(&headers)?)
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        checked::<contracts::NativeOperation>(
            serde_json::to_value(op).map_err(anyhow::Error::from)?,
        )?,
    ))
}
async fn send(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    headers: HeaderMap,
    Json(input): Json<contracts::NativeContactRequestSend>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    submit(
        app,
        c,
        a,
        headers,
        ContactMutation::SendRequest {
            recipient_ref: input.recipient_ref,
            message: input.message,
            privacy: input.privacy.map(privacy_settings),
        },
    )
    .await
}
async fn accept(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<contracts::NativeContactRequestAccept>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    submit(
        app,
        c,
        a,
        headers,
        ContactMutation::AcceptRequest {
            id,
            expected_version: input.expected_version,
            privacy: input.privacy.map(privacy_settings),
        },
    )
    .await
}
async fn delete(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<contracts::NativeContactRequestVersion>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    submit(
        app,
        c,
        a,
        headers,
        ContactMutation::DeleteRequest {
            id,
            expected_version: input.expected_version,
        },
    )
    .await
}
fn privacy_settings(
    input: contracts::NativeContactPrivacySettings,
) -> wechat_core::ContactPrivacySettings {
    wechat_core::ContactPrivacySettings {
        chats_only: input.chats_only,
        hide_my_posts: input.hide_my_posts,
        hide_their_posts: input.hide_their_posts,
    }
}
async fn privacy(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "admin")?;
    let value = app
        .core
        .read(&a.clone(), move |core| core.contact_privacy(&a, &id))
        .await?;
    checked::<contracts::NativeContactPrivacy>(value)
}
async fn update_contact(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path((a, id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<contracts::NativeContactUpdate>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    c.require(Some(&a), "admin")?;
    let operation = app
        .core
        .update_contact(
            &a,
            &id,
            wechat_core::ContactUpdate {
                privacy: privacy_settings(input.privacy),
                expected_version: input.expected_version,
            },
            idempotency_key(&headers)?,
        )
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        checked::<contracts::NativeOperation>(
            serde_json::to_value(operation).map_err(anyhow::Error::from)?,
        )?,
    ))
}
async fn remove(
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
        ContactMutation::RemoveContact { username: id },
    )
    .await
}
async fn by_key(
    State(app): State<App>,
    Extension(c): Extension<Claims>,
    Path(a): Path<String>,
    Query(q): Query<ContactMutationKeyQuery>,
) -> ApiResult<Json<Value>> {
    c.require(Some(&a), "admin")?;
    let op = app
        .core
        .store
        .contact_mutation_by_key(&a, &q.key, None)?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))?
        .operation;
    checked::<contracts::NativeOperation>(serde_json::to_value(op).map_err(anyhow::Error::from)?)
}

/// Original key for inspecting a contact mutation.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct ContactMutationKeyQuery {
    /// Original submitted idempotency key.
    key: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;
    #[tokio::test]
    async fn every_contact_mutation_requires_account_admin_and_a_key() {
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
            (
                "POST",
                "contact-requests",
                json!({"recipientRef":"opaque","message":"hello"}),
            ),
            (
                "POST",
                "contact-requests/fr_peer/accept",
                json!({"expectedVersion":"current"}),
            ),
            (
                "DELETE",
                "contact-requests/fr_peer",
                json!({"expectedVersion":"current"}),
            ),
            ("DELETE", "contacts/peer", json!({})),
            (
                "PATCH",
                "contacts/peer",
                json!({"privacy":{"chatsOnly":true,"hideMyPosts":false,"hideTheirPosts":false},"expectedVersion":"current"}),
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
    #[tokio::test]
    async fn privacy_read_requires_admin_on_the_selected_account() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&temp.path().join("data")).unwrap());
        store.create_account("one").unwrap();
        let read = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        store
            .issue_token(
                "reader",
                &format!("{:x}", Sha256::digest(read.as_bytes())),
                &["one".into()],
                &["read".into()],
                None,
            )
            .unwrap();
        let app = crate::router(
            Core::new(store, None).unwrap(),
            "0123456789abcdef0123456789abcdef",
        );
        let request = Request::builder()
            .uri("/v1/accounts/one/contacts/peer/privacy")
            .header("authorization", format!("Bearer {read}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
}
