//! Persistent authenticated incoming notifications. The outer longlink command
//! is distinct from the encrypted business CGI: heartbeat 238/518, sync 121/138.
use crate::{
    business::AuthenticatedPacket,
    mmtls::{Longlink, Session},
    wire, AccountState,
};
use anyhow::{ensure, Result};
use std::time::{Duration, Instant};

const REPLY_OFFSET: u32 = 1_000_000_000;
const NOOP: u32 = 6;
const NOOP_SEQUENCE: u32 = u32::MAX;
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Notification {
    pub command: u32,
    pub selector: u32,
}

/// Unknown commands are retained as transport traffic, not interpreted as sync
/// work. Command 24 has a four-byte big-endian selector, not protobuf data.
fn notification(packet: &Longlink) -> Result<Option<Notification>> {
    if packet.command != 24 || packet.sequence != 0 {
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
    pending: Option<Notification>,
    noop_sent: Option<Instant>,
}
impl Inbox {
    fn observe(&mut self, packet: &Longlink) -> Result<()> {
        if packet.command == NOOP + REPLY_OFFSET && packet.sequence == NOOP_SEQUENCE {
            ensure!(packet.payload.is_empty(), "Invalid keepalive reply");
            self.noop_sent = None;
        } else if let Some(next) = notification(packet)? {
            // Repeated notifications announce work; they are not message IDs.
            // Coalesce them while a business response is in flight.
            match &mut self.pending {
                Some(prior) => prior.selector |= next.selector,
                None => self.pending = Some(next),
            }
        }
        Ok(())
    }
}

pub struct Connection {
    session: Session,
    timeout: Duration,
    sequence: u32,
    last_keepalive: Instant,
    inbox: Inbox,
}
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
            last_keepalive: Instant::now(),
            inbox: Inbox::default(),
        })
    }
    fn keepalive(&mut self) -> Result<()> {
        let now = Instant::now();
        ensure!(
            self.inbox
                .noop_sent
                .is_none_or(|sent| now.duration_since(sent) < KEEPALIVE_TIMEOUT),
            "Native keepalive timed out"
        );
        if now.duration_since(self.last_keepalive) >= KEEPALIVE_INTERVAL
            && self.inbox.noop_sent.is_none()
        {
            self.session.send_application(NOOP, NOOP_SEQUENCE, &[])?;
            self.inbox.noop_sent = Some(now);
            self.last_keepalive = now;
        }
        Ok(())
    }
    fn rpc(
        &mut self,
        state: &mut AccountState,
        cgi: u16,
        command: u32,
        plain: &[u8],
    ) -> Result<Vec<u8>> {
        self.keepalive()?;
        let sequence = self.sequence;
        self.sequence = self
            .sequence
            .checked_add(1)
            .filter(|next| *next != NOOP_SEQUENCE)
            .ok_or_else(|| anyhow::anyhow!("Longlink sequence exhausted"))?;
        let packet = AuthenticatedPacket::prepare(state, cgi, plain)?;
        self.session
            .send_application(command, sequence, &packet.packet)?;
        let deadline = Instant::now() + self.timeout;
        for _ in 0..64 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(), "Native longlink response timed out");
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
    pub fn heartbeat(
        &mut self,
        state: &mut AccountState,
        cursor: &[u8],
    ) -> Result<wire::Heartbeat> {
        let request = wire::heartbeat_request(state, cursor)?;
        wire::decode_heartbeat(&self.rpc(state, 518, 238, &request)?)
    }
    pub fn sync(
        &mut self,
        state: &mut AccountState,
        cursor: &[u8],
        scene: u32,
    ) -> Result<wire::SyncBatch> {
        ensure!([1, 6, 7].contains(&scene), "Unsupported sync scene");
        let sent = if cursor.is_empty() { &[0][..] } else { cursor };
        let request = wire::sync_request(sent, &state.device.device_type, scene)?;
        wire::decode_sync(&self.rpc(state, 138, 121, &request)?, cursor)
    }
    /// Waiting owns no account mutation lock. The caller rechecks its credential
    /// and logout fences under that lock before acting on a notification.
    pub fn poll(&mut self, timeout: Duration) -> Result<Option<Notification>> {
        self.keepalive()?;
        if self.inbox.pending.is_some() {
            return Ok(self.inbox.pending.take());
        }
        let deadline = Instant::now() + timeout;
        for _ in 0..64 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let Some(packet) = self.session.poll_application(remaining)? else {
                return Ok(None);
            };
            self.inbox.observe(&packet)?;
            if self.inbox.pending.is_some() {
                return Ok(self.inbox.pending.take());
            }
        }
        anyhow::bail!("Native notification frame limit")
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
    fn interleaved_pushes_coalesce_without_confusing_keepalive_or_unknown_commands() {
        let mut inbox = Inbox {
            noop_sent: Some(Instant::now()),
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
                command: 356,
                sequence: 0,
                payload: vec![1, 2, 3],
            })
            .unwrap();
        inbox.observe(&push(0, &4u32.to_be_bytes())).unwrap();
        assert!(inbox.noop_sent.is_none());
        assert_eq!(inbox.pending.take().unwrap().selector, 6);
        assert!(inbox.pending.is_none());
        assert!(inbox
            .observe(&Longlink {
                command: REPLY_OFFSET + NOOP,
                sequence: NOOP_SEQUENCE,
                payload: vec![1]
            })
            .is_err());
    }
}
