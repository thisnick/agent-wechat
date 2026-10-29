//! Connect to WeChat, authenticate, synchronize and submit messages with [`NativeClient`].
//!
//! Most server operations are **methods on [`NativeClient`]**, rather than free
//! functions in this crate's Functions list. [`AccountState`] holds accepted
//! credentials; it is secret, mutable state, not an API access token.
//!
//! | Task | Start here |
//! | --- | --- |
//! | QR login | [`NativeClient::start_qr_login`], [`NativeClient::poll_login`], [`NativeClient::complete_login`] |
//! | Receive a sync page | [`NativeClient::sync`], [`wire::decode_add_message`] |
//! | Send text | [`NativeClient::send_text`], [`TextSendResult`] |
//! | Contacts and rooms | [`NativeClient::lookup_contacts`], [`NativeClient::lookup_room`] |
//! | Attachments | [`media`], [`media_send`], [`NativeClient::send_file`] |
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
//! use wechat_protocol::{wire, AccountState, NativeClient, TextSendResult};
//!
//! fn send_once(
//!     client: &NativeClient,
//!     state: &mut AccountState,
//!     recipient: &str,
//!     text: &str,
//!     task_id: &str,
//!     created_at: u32,
//! ) -> anyhow::Result<TextSendResult> {
//!     let client_id = wire::client_message_id(task_id.as_bytes());
//!     anyhow::ensure!(client_id != 0, "Choose another task ID");
//!     client.send_text(state, recipient, text, created_at, client_id)
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
//! Network methods block; async applications must use a blocking worker.
pub mod business;
mod client;
mod lan_login;
pub use lan_login::login_lan_error_reason;
pub mod contact_sync;
pub mod crypto;
pub mod file_upload;
pub mod media;
pub mod media_send;
pub mod mmtls;
mod notice;
pub mod protobuf;
pub mod push;
pub mod quote_send;
pub mod silk;
pub mod wire;
pub use client::*;

pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
