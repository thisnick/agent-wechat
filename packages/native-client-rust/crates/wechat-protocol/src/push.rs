//! Persistent authenticated incoming notifications. The outer longlink command
//! is distinct from the encrypted business CGI: heartbeat 238/518, sync 121/138.
// This module defines or still uses the blocking APIs deprecated in 3.2;
// 4.0 removes them (#701).
#![allow(deprecated)]
#[cfg(not(target_arch = "wasm32"))]
use crate::mmtls::Session;
use crate::{
    business::AuthenticatedPacket,
    mmtls::{AsyncSession, Longlink},
    transport::{Clock, StreamTransport},
    wire, AccountState,
};
use anyhow::{ensure, Result};
use std::{collections::VecDeque, sync::Arc, time::Duration};

const REPLY_OFFSET: u32 = 1_000_000_000;
const NOOP: u32 = 6;
const NOOP_SEQUENCE: u32 = u32::MAX;
const KEEPALIVE_INTERVAL_MS: u64 = 30_000;
const KEEPALIVE_TIMEOUT_MS: u64 = 15_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Notification {
    pub command: u32,
    pub selector: u32,
}

/// Unknown commands are retained as transport traffic, not interpreted as sync
/// work. Command 24 has a four-byte big-endian selector, not protobuf data.
fn notification(packet: &Longlink) -> Result<Option<Notification>> {
    if packet.sequence != 0 {
        return Ok(None);
    }
    if packet.command == 356 {
        use crate::protobuf as p;
        ensure!(
            packet.payload.len() <= 128 * 1024,
            "Contact hint exceeds limit"
        );
        let fields = p::decode(&packet.payload)?;
        if p::scalar(&fields, 2)? != 11 {
            return Ok(None);
        }
        let notice = p::decode(p::data(&fields, 3)?)?;
        if p::scalar(&notice, 1)? != 2048 {
            return Ok(None);
        }
        // Some namespace notices have no selector payload. They do not match
        // the mapped contact hint and must not interrupt an in-flight reply.
        let Some(raw_selectors) = p::optional_data(&notice, 2)? else {
            return Ok(None);
        };
        let selectors = p::decode(raw_selectors)?;
        if p::optional(&selectors, 1)?.is_none() {
            return Ok(None);
        }
        let selector = u32::try_from(p::scalar(&selectors, 1)?)?;
        // Contact notices vary their high selector bits with the kind of
        // change: a phone mute toggle carried 0x20004 and a phone remark edit
        // 0x4000004. Any notice sharing a contact-sync bit asks for the same
        // contactsync drain; the drain itself still uses CONTACT_SELECTOR.
        let contact_bits = crate::contact_sync::CONTACT_SELECTOR as u32;
        return Ok((selector & contact_bits != 0).then_some(Notification {
            command: 356,
            selector,
        }));
    }
    if packet.command != 24 {
        return Ok(None);
    }
    ensure!(packet.payload.len() == 4, "Invalid native push selector");
    let selector = u32::from_be_bytes(packet.payload.as_slice().try_into()?);
    Ok((selector & 7 != 0).then_some(Notification {
        command: 24,
        selector,
    }))
}

#[derive(Default)]
struct Inbox {
    pending: VecDeque<Notification>,
    noop_sent_ms: Option<u64>,
}
impl Inbox {
    fn observe(&mut self, packet: &Longlink) -> Result<()> {
        if packet.command == NOOP + REPLY_OFFSET && packet.sequence == NOOP_SEQUENCE {
            ensure!(packet.payload.is_empty(), "Invalid keepalive reply");
            self.noop_sent_ms = None;
        } else if let Some(next) = notification(packet)? {
            // Repeated notifications announce work; they are not message IDs.
            // Coalesce them while a business response is in flight.
            match self
                .pending
                .iter_mut()
                .find(|prior| prior.command == next.command)
            {
                Some(prior) => prior.selector |= next.selector,
                None => self.pending.push_back(next),
            }
        }
        Ok(())
    }
}

fn should_send_keepalive(
    now_ms: u64,
    last_keepalive_ms: u64,
    noop_sent_ms: Option<u64>,
) -> Result<bool> {
    ensure!(
        noop_sent_ms.is_none_or(|sent| now_ms.saturating_sub(sent) < KEEPALIVE_TIMEOUT_MS),
        "Native keepalive timed out"
    );
    Ok(now_ms.saturating_sub(last_keepalive_ms) >= KEEPALIVE_INTERVAL_MS && noop_sent_ms.is_none())
}

fn next_sequence(sequence: &mut u32) -> Result<u32> {
    let current = *sequence;
    *sequence = current
        .checked_add(1)
        .filter(|next| *next != NOOP_SEQUENCE)
        .ok_or_else(|| anyhow::anyhow!("Longlink sequence exhausted"))?;
    Ok(current)
}

// Legacy blocking push connection kept for 3.x native compatibility (#701).
// Prefer `AsyncConnection` for new code.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct Connection {
    session: Session,
    timeout: Duration,
    sequence: u32,
    last_keepalive_ms: u64,
    inbox: Inbox,
}
#[cfg(not(target_arch = "wasm32"))]
impl Connection {
    pub(crate) fn open(
        state: &AccountState,
        timeout: Duration,
        receive_budget: usize,
    ) -> Result<Self> {
        state.validate()?;
        Ok(Self {
            session: Session::connect(&state.long_host, timeout, receive_budget)?,
            timeout,
            sequence: 1,
            last_keepalive_ms: crate::transport::native_monotonic_ms(),
            inbox: Inbox::default(),
        })
    }
    fn keepalive(&mut self) -> Result<()> {
        let now = crate::transport::native_monotonic_ms();
        if should_send_keepalive(now, self.last_keepalive_ms, self.inbox.noop_sent_ms)? {
            self.session.send_application(NOOP, NOOP_SEQUENCE, &[])?;
            self.inbox.noop_sent_ms = Some(now);
            self.last_keepalive_ms = now;
        }
        Ok(())
    }
    fn rpc(
        &mut self,
        state: &mut AccountState,
        cgi: u16,
        command: u32,
        plain: &[u8],
        before_submit: impl FnOnce() -> Result<()>,
    ) -> Result<Vec<u8>> {
        self.keepalive()?;
        let sequence = next_sequence(&mut self.sequence)?;
        let packet = AuthenticatedPacket::prepare(state, cgi, plain)?;
        before_submit()?;
        self.session
            .send_application(command, sequence, &packet.packet)?;
        let timeout_ms = u64::try_from(self.timeout.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        let deadline = crate::transport::native_monotonic_ms().saturating_add(timeout_ms);
        for _ in 0..64 {
            let now = crate::transport::native_monotonic_ms();
            ensure!(now < deadline, "Native longlink response timed out");
            let remaining = Duration::from_millis(deadline - now);
            let reply = self
                .session
                .poll_application(remaining)?
                .ok_or_else(|| anyhow::anyhow!("Native longlink response timed out"))?;
            if reply.sequence != sequence {
                self.inbox.observe(&reply)?;
                continue;
            }
            ensure!(
                reply.command == command + REPLY_OFFSET,
                "Unexpected longlink response command"
            );
            let (plain, cookie) = packet.decode_verified_transport_reply(&reply.payload)?;
            if !cookie.is_empty() {
                state.cookie = cookie;
            }
            return Ok(plain);
        }
        anyhow::bail!("Native longlink response frame limit")
    }
    /// Also associates the authenticated account with this persistent socket.
    pub(crate) fn heartbeat_guarded(
        &mut self,
        state: &mut AccountState,
        cursor: &[u8],
        before_submit: impl FnOnce() -> Result<()>,
    ) -> Result<wire::Heartbeat> {
        let request = wire::heartbeat_request(state, cursor)?;
        wire::decode_heartbeat(&self.rpc(state, 518, 238, &request, before_submit)?)
    }

    /// One sticker piece (CGI 175) on the long link, which the official client
    /// prefers over the short link while connected. Notifications that arrive
    /// before the reply stay queued for [`Self::poll`].
    pub(crate) fn sticker_chunk_guarded(
        &mut self,
        state: &mut AccountState,
        chunk: &crate::sticker_send::StickerSendChunk<'_>,
        before_submit: impl FnOnce() -> Result<()>,
    ) -> Result<Vec<u8>> {
        let request = crate::sticker_send::sticker_send_request(state, chunk)?;
        self.rpc(state, 175, 68, &request, before_submit)
    }

    pub(crate) fn sync_guarded(
        &mut self,
        state: &mut AccountState,
        cursor: &[u8],
        scene: u32,
        before_submit: impl FnOnce() -> Result<()>,
    ) -> Result<wire::SyncBatch> {
        ensure!([1, 6, 7].contains(&scene), "Unsupported sync scene");
        let sent = if cursor.is_empty() { &[0][..] } else { cursor };
        let request = wire::sync_request(sent, &state.device.device_type, scene)?;
        wire::decode_sync(&self.rpc(state, 138, 121, &request, before_submit)?, cursor)
    }
    /// Waiting owns no account mutation lock. The caller rechecks its credential
    /// and logout fences under that lock before acting on a notification.
    pub fn poll(&mut self, timeout: Duration) -> Result<Option<Notification>> {
        self.keepalive()?;
        if !self.inbox.pending.is_empty() {
            return Ok(self.inbox.pending.pop_front());
        }
        let timeout_ms = u64::try_from(timeout.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        let deadline = crate::transport::native_monotonic_ms().saturating_add(timeout_ms);
        for _ in 0..64 {
            let now = crate::transport::native_monotonic_ms();
            if now >= deadline {
                return Ok(None);
            }
            let remaining = Duration::from_millis(deadline - now);
            let Some(packet) = self.session.poll_application(remaining)? else {
                return Ok(None);
            };
            self.inbox.observe(&packet)?;
            if !self.inbox.pending.is_empty() {
                return Ok(self.inbox.pending.pop_front());
            }
        }
        anyhow::bail!("Native notification frame limit")
    }
}

pub(crate) struct AsyncConnection {
    session: AsyncSession,
    clock: Arc<dyn Clock>,
    timeout: Duration,
    sequence: u32,
    last_keepalive_ms: u64,
    inbox: Inbox,
}

impl AsyncConnection {
    pub(crate) async fn open(
        state: &AccountState,
        timeout: Duration,
        receive_budget: usize,
        streams: &dyn StreamTransport,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        state.validate()?;
        let session = AsyncSession::connect(
            &state.long_host,
            timeout,
            receive_budget,
            streams,
            clock.clone(),
        )
        .await?;
        let now = clock.monotonic_ms();
        Ok(Self {
            session,
            clock,
            timeout,
            sequence: 1,
            last_keepalive_ms: now,
            inbox: Inbox::default(),
        })
    }

    async fn keepalive(&mut self) -> Result<()> {
        let now = self.clock.monotonic_ms();
        if should_send_keepalive(now, self.last_keepalive_ms, self.inbox.noop_sent_ms)? {
            self.session
                .send_application(NOOP, NOOP_SEQUENCE, &[])
                .await?;
            self.inbox.noop_sent_ms = Some(now);
            self.last_keepalive_ms = now;
        }
        Ok(())
    }

    async fn rpc(
        &mut self,
        state: &mut AccountState,
        cgi: u16,
        command: u32,
        plain: &[u8],
        before_submit: impl FnOnce() -> Result<()>,
    ) -> Result<Vec<u8>> {
        self.keepalive().await?;
        let sequence = next_sequence(&mut self.sequence)?;
        let packet = AuthenticatedPacket::prepare(state, cgi, plain)?;
        before_submit()?;
        self.session
            .send_application(command, sequence, &packet.packet)
            .await?;
        let timeout_ms = u64::try_from(self.timeout.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        let deadline = self.clock.monotonic_ms().saturating_add(timeout_ms);
        for _ in 0..64 {
            let now = self.clock.monotonic_ms();
            ensure!(now < deadline, "Native longlink response timed out");
            let remaining = Duration::from_millis(deadline - now);
            let reply = self
                .session
                .poll_application(remaining)
                .await?
                .ok_or_else(|| anyhow::anyhow!("Native longlink response timed out"))?;
            if reply.sequence != sequence {
                self.inbox.observe(&reply)?;
                continue;
            }
            ensure!(
                reply.command == command + REPLY_OFFSET,
                "Unexpected longlink response command"
            );
            let (plain, cookie) = packet.decode_verified_transport_reply(&reply.payload)?;
            if !cookie.is_empty() {
                state.cookie = cookie;
            }
            return Ok(plain);
        }
        anyhow::bail!("Native longlink response frame limit")
    }

    pub(crate) async fn heartbeat_guarded(
        &mut self,
        state: &mut AccountState,
        cursor: &[u8],
        before_submit: impl FnOnce() -> Result<()>,
    ) -> Result<wire::Heartbeat> {
        let now_secs = self.clock.unix_time_secs()?;
        let request = wire::heartbeat_request_at(state, cursor, now_secs)?;
        wire::decode_heartbeat(&self.rpc(state, 518, 238, &request, before_submit).await?)
    }

    pub(crate) async fn sticker_chunk_guarded(
        &mut self,
        state: &mut AccountState,
        chunk: &crate::sticker_send::StickerSendChunk<'_>,
        before_submit: impl FnOnce() -> Result<()>,
    ) -> Result<Vec<u8>> {
        let request = crate::sticker_send::sticker_send_request(state, chunk)?;
        self.rpc(state, 175, 68, &request, before_submit).await
    }

    pub(crate) async fn sync_guarded(
        &mut self,
        state: &mut AccountState,
        cursor: &[u8],
        scene: u32,
        before_submit: impl FnOnce() -> Result<()>,
    ) -> Result<wire::SyncBatch> {
        ensure!([1, 6, 7].contains(&scene), "Unsupported sync scene");
        let sent = if cursor.is_empty() { &[0][..] } else { cursor };
        let request = wire::sync_request(sent, &state.device.device_type, scene)?;
        wire::decode_sync(
            &self.rpc(state, 138, 121, &request, before_submit).await?,
            cursor,
        )
    }

    pub(crate) async fn poll(&mut self, timeout: Duration) -> Result<Option<Notification>> {
        self.keepalive().await?;
        if !self.inbox.pending.is_empty() {
            return Ok(self.inbox.pending.pop_front());
        }
        let timeout_ms = u64::try_from(timeout.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        let deadline = self.clock.monotonic_ms().saturating_add(timeout_ms);
        for _ in 0..64 {
            let now = self.clock.monotonic_ms();
            if now >= deadline {
                return Ok(None);
            }
            let remaining = Duration::from_millis(deadline - now);
            let Some(packet) = self.session.poll_application(remaining).await? else {
                return Ok(None);
            };
            self.inbox.observe(&packet)?;
            if !self.inbox.pending.is_empty() {
                return Ok(self.inbox.pending.pop_front());
            }
        }
        anyhow::bail!("Native notification frame limit")
    }

    pub(crate) async fn close(self) -> Result<()> {
        self.session.close().await
    }

    #[cfg(test)]
    pub(crate) fn from_session_for_test(session: AsyncSession, clock: Arc<dyn Clock>) -> Self {
        let now = clock.monotonic_ms();
        Self {
            session,
            clock,
            timeout: Duration::from_secs(5),
            sequence: 1,
            last_keepalive_ms: now,
            inbox: Inbox::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn push(sequence: u32, selector: &[u8]) -> Longlink {
        Longlink {
            command: 24,
            sequence,
            payload: selector.to_vec(),
        }
    }
    #[test]
    fn selectors_are_strict_big_endian_work_hints() {
        assert_eq!(
            notification(&push(0, &6u32.to_be_bytes())).unwrap(),
            Some(Notification {
                command: 24,
                selector: 6
            })
        );
        assert!(notification(&push(0, &8u32.to_be_bytes()))
            .unwrap()
            .is_none());
        assert!(notification(&push(12, &6u32.to_be_bytes()))
            .unwrap()
            .is_none());
        for invalid in [&[][..], &[6][..], &[0, 0, 6][..], &[0; 5][..]] {
            assert!(notification(&push(0, invalid)).is_err());
        }
    }
    #[test]
    fn unmapped_contact_notices_without_selectors_leave_the_inbox_intact() {
        use crate::protobuf as p;
        let mut inbox = Inbox::default();
        inbox.observe(&push(0, &2u32.to_be_bytes())).unwrap();
        for notice in [
            p::uint(1, 2048),
            p::message(&[p::uint(1, 2048), p::bytes(2, b"")]).unwrap(),
        ] {
            inbox
                .observe(&Longlink {
                    command: 356,
                    sequence: 0,
                    payload: p::message(&[p::uint(2, 11), p::bytes(3, &notice)]).unwrap(),
                })
                .unwrap();
        }
        assert_eq!(
            inbox.pending.pop_front().unwrap(),
            Notification {
                command: 24,
                selector: 2
            }
        );
        assert!(inbox.pending.is_empty());
    }
    #[test]
    fn contact_notices_with_other_contact_selector_bits_request_a_drain() {
        use crate::protobuf as p;
        let notice = |selector: u64| Longlink {
            command: 356,
            sequence: 0,
            payload: p::message(&[
                p::uint(2, 11),
                p::bytes(
                    3,
                    &p::message(&[p::uint(1, 2048), p::bytes(2, &p::uint(1, selector))]).unwrap(),
                ),
            ])
            .unwrap(),
        };
        for selector in [0x20004, 0x4000004, 0x20000] {
            assert_eq!(
                notification(&notice(selector)).unwrap(),
                Some(Notification {
                    command: 356,
                    selector: selector as u32
                })
            );
        }
        for selector in [0, 0x8, 0x4000000] {
            assert_eq!(notification(&notice(selector)).unwrap(), None);
        }
    }
    #[test]
    fn contact_hint_coalesces_separately_from_message_work() {
        use crate::protobuf as p;
        let hint = Longlink {
            command: 356,
            sequence: 0,
            payload: p::message(&[
                p::bytes(1, b"opaque"),
                p::uint(2, 11),
                p::bytes(
                    3,
                    &p::message(&[
                        p::uint(1, 2048),
                        p::bytes(2, &p::uint(1, crate::contact_sync::CONTACT_SELECTOR)),
                    ])
                    .unwrap(),
                ),
            ])
            .unwrap(),
        };
        let mut inbox = Inbox::default();
        inbox.observe(&push(0, &2u32.to_be_bytes())).unwrap();
        inbox.observe(&hint).unwrap();
        inbox.observe(&push(0, &4u32.to_be_bytes())).unwrap();
        inbox.observe(&hint).unwrap();
        assert_eq!(inbox.pending.len(), 2);
        assert_eq!(
            inbox.pending.pop_front().unwrap(),
            Notification {
                command: 24,
                selector: 6
            }
        );
        assert_eq!(
            inbox.pending.pop_front().unwrap(),
            Notification {
                command: 356,
                selector: 0x20004
            }
        );
        let mut malformed = hint;
        malformed.payload = p::message(&[p::uint(2, 11), p::bytes(3, b"invalid")]).unwrap();
        assert!(notification(&malformed).is_err());
    }
    #[test]
    fn interleaved_pushes_coalesce_without_confusing_keepalive_or_unknown_commands() {
        let mut inbox = Inbox {
            noop_sent_ms: Some(1_000),
            ..Default::default()
        };
        inbox.observe(&push(0, &2u32.to_be_bytes())).unwrap();
        inbox
            .observe(&Longlink {
                command: REPLY_OFFSET + NOOP,
                sequence: NOOP_SEQUENCE,
                payload: vec![],
            })
            .unwrap();
        inbox
            .observe(&Longlink {
                command: 912,
                sequence: 0,
                payload: vec![1, 2, 3],
            })
            .unwrap();
        inbox.observe(&push(0, &4u32.to_be_bytes())).unwrap();
        assert!(inbox.noop_sent_ms.is_none());
        assert_eq!(inbox.pending.pop_front().unwrap().selector, 6);
        assert!(inbox.pending.is_empty());
        assert!(inbox
            .observe(&Longlink {
                command: REPLY_OFFSET + NOOP,
                sequence: NOOP_SEQUENCE,
                payload: vec![1]
            })
            .is_err());
    }

    #[tokio::test]
    async fn async_connection_handles_interleaved_notifications_and_keepalive_timeout() {
        use crate::{
            mmtls, protobuf as p,
            transport::{AsyncByteStream, Clock, ReadOutcome, TransportFuture},
        };
        use std::collections::VecDeque;
        use std::sync::{
            atomic::{AtomicBool, AtomicU64, Ordering},
            Mutex,
        };

        struct FakeClock(AtomicU64);
        impl Clock for FakeClock {
            fn monotonic_ms(&self) -> u64 {
                self.0.load(Ordering::Acquire)
            }
            fn unix_time_ms(&self) -> Result<u64> {
                Ok(1_700_000_000_000 + self.monotonic_ms())
            }
        }

        enum Step {
            Bytes(Vec<u8>),
            TimedOut,
        }

        struct MockPushStream {
            reads: VecDeque<Step>,
            writes: Arc<Mutex<Vec<Vec<u8>>>>,
            closed: Arc<AtomicBool>,
        }

        impl AsyncByteStream for MockPushStream {
            fn read<'a>(
                &'a mut self,
                buf: &'a mut [u8],
                _timeout: Duration,
            ) -> TransportFuture<'a, Result<ReadOutcome>> {
                let outcome = match self.reads.pop_front() {
                    Some(Step::Bytes(mut chunk)) => {
                        let take = chunk.len().min(buf.len());
                        buf[..take].copy_from_slice(&chunk[..take]);
                        if take < chunk.len() {
                            chunk.drain(..take);
                            self.reads.push_front(Step::Bytes(chunk));
                        }
                        ReadOutcome::Bytes(take)
                    }
                    Some(Step::TimedOut) | None => ReadOutcome::TimedOut,
                };
                Box::pin(async move { Ok(outcome) })
            }
            fn write_all<'a>(
                &'a mut self,
                buf: &'a [u8],
                _timeout: Duration,
            ) -> TransportFuture<'a, Result<()>> {
                self.writes.lock().unwrap().push(buf.to_vec());
                Box::pin(async { Ok(()) })
            }
            fn close<'a>(&'a mut self) -> TransportFuture<'a, Result<()>> {
                self.closed.store(true, Ordering::Release);
                Box::pin(async { Ok(()) })
            }
        }

        let key = vec![0x11; 16];
        let iv = vec![0x22; 12];
        let mut state = AccountState {
            schema: 1,
            device: crate::DeviceProfile {
                device_id: [b"A".as_slice(), &[1; 15]].concat(),
                device_type: "UnifiedPCLinux x86_64".into(),
                client_version: -227271398,
            },
            uin: 42,
            own_username: "synthetic_push".into(),
            cookie: vec![],
            session_key: vec![0x33; 16],
            request_key: vec![0x44; 24],
            response_key: vec![0x55; 24],
            auto_auth_key: vec![],
            key_info: vec![],
            database_password: vec![],
            auth_snapshot: Default::default(),
            long_host: "long.weixin.qq.com".into(),
            short_host: "short.weixin.qq.com".into(),
        };

        // Build a synthetic heartbeat response business envelope (CGI 518, mode 13 AES-192-GCM).
        let hb_plain =
            p::message(&[p::bytes(1, &p::uint(1, 0)), p::uint(2, 60), p::uint(3, 0)]).unwrap();
        let hb_cipher =
            crate::crypto::envelope_seal(&hb_plain, &state.response_key, b"", [0x77; 12]).unwrap();
        let hb_cookie = b"ck";
        let mut hb_tail = Vec::new();
        for value in [518u64, hb_plain.len() as u64, hb_plain.len() as u64, 0, 0] {
            hb_tail.extend(p::varint(value));
        }
        let header_size = 10 + hb_cookie.len() + hb_tail.len();
        let flags =
            (13u16 << 12) | ((hb_cookie.len() as u16) << 8) | ((header_size as u16) << 2) | 2;
        let mut hb_env = flags.to_le_bytes().to_vec();
        hb_env.extend(0i32.to_be_bytes());
        hb_env.extend(state.uin.to_be_bytes());
        hb_env.extend(hb_cookie);
        hb_env.extend(hb_tail);
        hb_env.extend(hb_cipher);

        let notify_frame = mmtls::pack_longlink(24, 0, &6u32.to_be_bytes()).unwrap();
        let hb_reply_frame = mmtls::pack_longlink(REPLY_OFFSET + 238, 1, &hb_env).unwrap();
        let r1 = mmtls::seal(0x17, &notify_frame, &key, &iv, 5).unwrap();
        let r2 = mmtls::seal(0x17, &hb_reply_frame, &key, &iv, 6).unwrap();

        let clock = Arc::new(FakeClock(AtomicU64::new(1_000)));
        let writes = Arc::new(Mutex::new(Vec::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let stream = MockPushStream {
            reads: [Step::Bytes(r1), Step::Bytes(r2), Step::TimedOut]
                .into_iter()
                .collect(),
            writes: writes.clone(),
            closed: closed.clone(),
        };
        let session =
            mmtls::tests::async_session_for_test(Box::new(stream), clock.clone(), key, iv);
        let mut conn = AsyncConnection {
            session,
            clock: clock.clone(),
            timeout: Duration::from_secs(5),
            sequence: 1,
            last_keepalive_ms: 1_000,
            inbox: Inbox::default(),
        };

        // Heartbeat sees interleaved command 24 first, queues it, then decodes heartbeat reply + cookie.
        let hb = conn
            .heartbeat_guarded(&mut state, &[], || Ok(()))
            .await
            .unwrap();
        assert_eq!(hb.next_interval, 60);
        assert_eq!(state.cookie, b"ck");

        // Verify outgoing heartbeat protobuf used FakeClock's unix timestamp (1_700_000_001), not SystemTime.
        let sent_record = writes.lock().unwrap()[0].clone();
        let rec = mmtls::Record {
            header: sent_record[..5].try_into().unwrap(),
            body: sent_record[5..].to_vec(),
        };
        let frame_plain = mmtls::open(&rec, &[0x11; 16], &[0x22; 12], 3).unwrap();
        let frame = mmtls::unpack_longlink(&frame_plain).unwrap();
        assert_eq!(frame.command, 238);
        let env = crate::business::split(&frame.payload).unwrap();
        let hb_req_plain =
            crate::crypto::envelope_open(&env.payload, &state.request_key, b"").unwrap();
        let hb_req_fields = p::decode(&hb_req_plain).unwrap();
        assert_eq!(p::scalar(&hb_req_fields, 2).unwrap(), 1_700_000_001);

        // Queued notification is returned immediately on poll().
        let notif = conn.poll(Duration::from_secs(1)).await.unwrap().unwrap();
        assert_eq!(
            notif,
            Notification {
                command: 24,
                selector: 6
            }
        );

        // Advance past keepalive interval (30s): poll sends NOOP and returns None on idle timeout.
        clock.0.store(35_000, Ordering::Release);
        assert!(conn.poll(Duration::from_secs(1)).await.unwrap().is_none());
        assert_eq!(writes.lock().unwrap().len(), 2); // heartbeat + NOOP

        // Advance past NOOP reply timeout (15s): next poll fails closed.
        clock.0.store(55_000, Ordering::Release);
        assert!(conn.poll(Duration::from_secs(1)).await.is_err());

        conn.close().await.unwrap();
        assert!(closed.load(Ordering::Acquire));
    }
}
