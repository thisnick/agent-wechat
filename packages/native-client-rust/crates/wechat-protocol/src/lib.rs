//! Connect to WeChat, authenticate, synchronize and submit messages with [`NativeClient`].
//!
//! Most server operations are **methods on [`NativeClient`]**, rather than free
//! functions in this crate's Functions list. [`AccountState`] holds accepted
//! credentials; transfer it once into [`NativeClient::from_account`]. Account
//! operations then share that owner. It is secret data, not an API access token.
//!
//! | Task | Start here |
//! | --- | --- |
//! | QR login | [`NativeClient::start_qr_login`], [`NativeClient::poll_login`], [`NativeClient::complete_login`] |
//! | Receive a sync page | [`NativeClient::sync`], [`wire::decode_add_message`] |
//! | Send text | [`NativeClient::send_text`], [`TextSendResult`] |
//! | Contacts and rooms | [`NativeClient::lookup_contacts`], [`NativeClient::lookup_room`] |
//! | Attachments | [`media`], [`media_send`], [`NativeClient::send_file`] |
//! | Logout and shutdown | [`NativeClient::logout`], [`NativeClient::shutdown`], and `_blocking` forms of both |
//! | Classify a failure | [`ServiceRejected`], [`is_session_rejected`], [`AccountInterrupted`], [`NativeRequestNotSubmitted`] |
//! | Low-level encoding | [`wire`], [`protobuf`] |
//!
//! # Send one text message
//!
//! After an approved login, call the client method with the recipient's exact
//! WeChat username. Persist the task ID, timestamp and intended operation in your
//! application's recovery record before submission. A client ID is not a server
//! guarantee against duplicate sends.
//!
//! ```no_run
//! use wechat_protocol::{wire, NativeClient, TextSendOptions, TextSendResult};
//!
//! async fn send_once(
//!     client: &NativeClient,
//!     recipient: &str,
//!     text: &str,
//!     task_id: &str,
//!     created_at: u32,
//! ) -> anyhow::Result<TextSendResult> {
//!     let client_id = wire::client_message_id(task_id.as_bytes());
//!     anyhow::ensure!(client_id != 0, "Choose another task ID");
//!     client.send_text(recipient, text, TextSendOptions {
//!         created_at,
//!         client_message_id: client_id,
//!         source: None,
//!     }).await
//! }
//! ```
//!
//! The source package includes runnable `qr_login`, `send_text` and `sync_once`
//! examples. They keep credentials in memory and perform no profile writes.
//!
//! # Application responsibilities
//!
//! The crate implements supported wire operations, transport authentication and
//! response decoding. The embedding application owns credential storage, send
//! intents, uncertainty handling, sync recovery and official-profile writeback.
//! An accepted receipt establishes server acceptance, not recipient playback or
//! completed local history. An error after submission must not cause a blind resend.
//!
//! This library does not launch or load the official client. Peer responses pass
//! pinned MMTLS signature and record authentication before business decode.
//! Account-owned requests run blocking transport work on Tokio's blocking pool.
//! Login bootstrap and remaining low-level operations
//! are blocking and require a blocking worker in async applications.
//!
//! # Errors
//!
//! Functions return `anyhow::Error`. Branch on the types it carries rather than
//! on message text:
//!
//! - [`ServiceRejected`] holds the [`Service`] and the non-zero code of a
//!   verified response. [`is_session_rejected`] is true when the account needs
//!   a new login or renewal.
//! - [`AccountInterrupted`] says why the account owner ended a request or a
//!   notification wait: stopped, transferred, or superseded by a renewal.
//! - [`NativeRequestNotSubmitted`] says nothing was handed to transport. An
//!   interruption without it has an unknown outcome.
//!
//! # Blocking embedders
//!
//! Applications that run their own blocking workers need no Tokio runtime:
//! use [`NativeClient::request_blocking`],
//! [`NativeClient::open_push_connection_blocking`],
//! [`NativeClient::renew_session_blocking`], [`NativeClient::logout_blocking`],
//! [`PendingLogout::submit_blocking`] and [`NativeClient::shutdown_blocking`].
pub mod business;

pub mod calls;
mod client;
mod errors;
pub use errors::{is_session_rejected, AccountInterrupted, Service, ServiceRejected};
mod lan_login;
pub use lan_login::login_lan_error_reason;
pub mod contact_sync;
pub mod crypto;
pub mod file_upload;
pub mod media;
pub mod media_send;
pub mod mmtls;
mod notice;
pub mod openim;
pub use openim::{OpenImContact, OpenImServiceError};
pub mod protobuf;
pub mod push;
pub mod quote_send;
pub mod silk;
pub mod wire;
pub use client::*;

pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
