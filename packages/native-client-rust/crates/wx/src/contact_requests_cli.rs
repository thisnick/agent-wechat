use super::*;
pub async fn requests(
    r: &Remote,
    config: &settings::File,
    command: ContactRequestCommand,
) -> Result<Value> {
    match command {
        ContactRequestCommand::List {
            direction,
            actionable,
            search,
            limit,
            cursor,
        } => {
            r.get(&query(
                r.path("contact-requests")?,
                &[
                    ("direction", direction),
                    ("actionable", actionable.then(|| "true".into())),
                    ("search", search),
                    ("limit", limit.map(|n| n.to_string())),
                    ("cursor", cursor),
                ],
            ))
            .await
        }
        ContactRequestCommand::Get { id } => {
            r.get(&r.path(&format!("contact-requests/{}", encode(&id)))?)
                .await
        }
        ContactRequestCommand::Send {
            recipient_ref,
            message,
            options,
        } => {
            client_actions::mutation(
                r,
                config,
                reqwest::Method::POST,
                "contact-requests",
                json!({"recipientRef":recipient_ref,"message":message}),
                options,
            )
            .await
        }
        ContactRequestCommand::Accept {
            id,
            expected_version,
            options,
        } => {
            client_actions::mutation(
                r,
                config,
                reqwest::Method::POST,
                &format!("contact-requests/{}/accept", encode(&id)),
                json!({"expectedVersion":expected_version}),
                options,
            )
            .await
        }
        ContactRequestCommand::Delete {
            id,
            expected_version,
            options,
        } => {
            client_actions::mutation(
                r,
                config,
                reqwest::Method::DELETE,
                &format!("contact-requests/{}", encode(&id)),
                json!({"expectedVersion":expected_version}),
                options,
            )
            .await
        }
    }
}
