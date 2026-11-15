//! Sticker pieces sent on a connected native push session's long link.
//!
//! The official client routes `sendemoji` to the persistent long link while one
//! is connected and to the short link otherwise. The push session owns its
//! socket, so a send job hands each piece to that session and waits; the session
//! runs queued pieces between notification polls. A piece the session never
//! started is reported as not sent, so the caller may use the short link
//! instead. Once started, its outcome belongs to the long link, and a failure is
//! never retried on another transport.
use anyhow::Result;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};

/// How long a piece may wait for the session to start it.
const START_WAIT: Duration = Duration::from_secs(20);
/// How long a started piece may take before its outcome is unknown.
const REPLY_WAIT: Duration = Duration::from_secs(180);

/// One owned sticker piece queued for the long link.
pub(crate) struct LongLinkStickerChunk {
    pub(crate) id: String,
    pub(crate) recipient: String,
    pub(crate) client_id: String,
    pub(crate) source: Arc<[u8]>,
    pub(crate) offset: u32,
    pub(crate) probe: bool,
}
impl LongLinkStickerChunk {
    pub(crate) fn borrowed(&self) -> wechat_protocol::sticker_send::StickerSendChunk<'_> {
        wechat_protocol::sticker_send::StickerSendChunk {
            id: &self.id,
            recipient: &self.recipient,
            client_id: &self.client_id,
            source: &self.source,
            offset: self.offset,
            probe: self.probe,
        }
    }
}

struct Request {
    chunk: LongLinkStickerChunk,
    taken: Arc<AtomicBool>,
    reply: mpsc::SyncSender<Result<Vec<u8>>>,
}

/// Result of handing a piece to the push session.
pub(crate) enum LongLinkOutcome {
    /// The session submitted the piece; this is its result.
    Sent(Result<Vec<u8>>),
    /// The session never started the piece; nothing was submitted.
    NotSent,
}

/// Pieces waiting for one account's push session.
#[derive(Default)]
pub(crate) struct LongLinkMailbox {
    queue: Mutex<VecDeque<Request>>,
    closed: AtomicBool,
}
impl LongLinkMailbox {
    /// Run every queued piece with `send`. Only the owning push session calls
    /// this, on its own connection, between polls.
    pub(crate) fn run(&self, mut send: impl FnMut(&LongLinkStickerChunk) -> Result<Vec<u8>>) {
        loop {
            // Marked taken under the queue lock, so a waiting caller can never
            // withdraw a piece that is about to be submitted.
            let request = {
                let Ok(mut queue) = self.queue.lock() else {
                    return;
                };
                let Some(request) = queue.pop_front() else {
                    return;
                };
                request.taken.store(true, Ordering::Release);
                request
            };
            let _ = request.reply.send(send(&request.chunk));
        }
    }
    /// Whether any piece is waiting.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> bool {
        self.queue.lock().is_ok_and(|queue| !queue.is_empty())
    }
    /// Hand one piece to the session and wait for its result.
    pub(crate) fn submit(&self, chunk: LongLinkStickerChunk) -> LongLinkOutcome {
        self.submit_within(chunk, START_WAIT, REPLY_WAIT)
    }
    fn submit_within(
        &self,
        chunk: LongLinkStickerChunk,
        start_wait: Duration,
        reply_wait: Duration,
    ) -> LongLinkOutcome {
        let taken = Arc::new(AtomicBool::new(false));
        let (reply, receive) = mpsc::sync_channel(1);
        {
            let Ok(mut queue) = self.queue.lock() else {
                return LongLinkOutcome::NotSent;
            };
            if self.closed.load(Ordering::Acquire) {
                return LongLinkOutcome::NotSent;
            }
            queue.push_back(Request {
                chunk,
                taken: taken.clone(),
                reply,
            });
        }
        let started = Instant::now();
        loop {
            match receive.recv_timeout(Duration::from_millis(100)) {
                Ok(result) => return LongLinkOutcome::Sent(result),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            if !taken.load(Ordering::Acquire) && started.elapsed() >= start_wait {
                break;
            }
            if started.elapsed() >= start_wait + reply_wait {
                break;
            }
        }
        // Withdraw the piece unless the session has started it.
        let Ok(mut queue) = self.queue.lock() else {
            return LongLinkOutcome::Sent(Err(anyhow::anyhow!("sticker_longlink_outcome_unknown")));
        };
        if taken.load(Ordering::Acquire) {
            drop(queue);
            return match receive.try_recv() {
                Ok(result) => LongLinkOutcome::Sent(result),
                Err(_) => {
                    LongLinkOutcome::Sent(Err(anyhow::anyhow!("sticker_longlink_outcome_unknown")))
                }
            };
        }
        queue.retain(|request| !Arc::ptr_eq(&request.taken, &taken));
        LongLinkOutcome::NotSent
    }
    /// Stop accepting pieces and release queued ones unsent.
    pub(crate) fn close(&self) {
        if let Ok(mut queue) = self.queue.lock() {
            self.closed.store(true, Ordering::Release);
            queue.clear();
        }
    }
}

impl crate::Core {
    /// The connected push session's mailbox, when this account has one.
    pub(crate) fn long_link(&self, account: &str) -> Option<Arc<LongLinkMailbox>> {
        self.long_links.lock().ok()?.get(account).cloned()
    }
    /// Publish a push session's mailbox until the returned registration drops.
    pub(crate) fn open_long_link(self: &Arc<Self>, account: &str) -> Option<LongLinkRegistration> {
        let mailbox = Arc::new(LongLinkMailbox::default());
        let mut links = self.long_links.lock().ok()?;
        if let Some(previous) = links.insert(account.to_owned(), mailbox.clone()) {
            previous.close();
        }
        Some(LongLinkRegistration {
            core: self.clone(),
            account: account.to_owned(),
            mailbox,
        })
    }
}

/// Removes a session's mailbox when the session ends, releasing waiting pieces
/// as not sent.
pub(crate) struct LongLinkRegistration {
    core: Arc<crate::Core>,
    account: String,
    pub(crate) mailbox: Arc<LongLinkMailbox>,
}
impl Drop for LongLinkRegistration {
    fn drop(&mut self) {
        if let Ok(mut links) = self.core.long_links.lock() {
            if links
                .get(&self.account)
                .is_some_and(|current| Arc::ptr_eq(current, &self.mailbox))
            {
                links.remove(&self.account);
            }
        }
        self.mailbox.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(offset: u32) -> LongLinkStickerChunk {
        LongLinkStickerChunk {
            id: "0".repeat(32),
            recipient: "filehelper".into(),
            client_id: "client".into(),
            source: Arc::from(&b"GIF89a"[..]),
            offset,
            probe: offset == 0,
        }
    }
    fn sent(outcome: LongLinkOutcome) -> Option<String> {
        match outcome {
            LongLinkOutcome::Sent(result) => Some(match result {
                Ok(reply) => String::from_utf8(reply).unwrap(),
                Err(error) => error.to_string(),
            }),
            LongLinkOutcome::NotSent => None,
        }
    }

    #[test]
    fn the_session_runs_queued_pieces_and_returns_their_replies() {
        let mailbox = Arc::new(LongLinkMailbox::default());
        let session = mailbox.clone();
        let worker = std::thread::spawn(move || {
            while !session.pending() {
                std::thread::sleep(Duration::from_millis(5));
            }
            session.run(|chunk| Ok(format!("ack {}", chunk.offset).into_bytes()));
        });
        let reply =
            mailbox.submit_within(chunk(8192), Duration::from_secs(5), Duration::from_secs(5));
        worker.join().unwrap();
        assert_eq!(sent(reply).as_deref(), Some("ack 8192"));
    }

    #[test]
    fn a_piece_the_session_never_started_is_withdrawn_as_not_sent() {
        let mailbox = LongLinkMailbox::default();
        let reply =
            mailbox.submit_within(chunk(0), Duration::from_millis(150), Duration::from_secs(5));
        assert!(sent(reply).is_none());
        assert!(!mailbox.pending(), "the withdrawn piece cannot run later");
        // A closed mailbox accepts nothing.
        mailbox.close();
        let reply = mailbox.submit_within(chunk(0), Duration::from_secs(5), Duration::from_secs(5));
        assert!(sent(reply).is_none());
    }

    #[test]
    fn closing_releases_waiting_pieces_unsent_but_a_started_piece_keeps_its_outcome() {
        let mailbox = Arc::new(LongLinkMailbox::default());
        let closer = mailbox.clone();
        let worker = std::thread::spawn(move || {
            while !closer.pending() {
                std::thread::sleep(Duration::from_millis(5));
            }
            closer.close();
        });
        let reply = mailbox.submit_within(chunk(0), Duration::from_secs(5), Duration::from_secs(5));
        worker.join().unwrap();
        assert!(sent(reply).is_none());

        // A started piece whose session fails reports that failure, never "not sent".
        let mailbox = Arc::new(LongLinkMailbox::default());
        let session = mailbox.clone();
        let worker = std::thread::spawn(move || {
            while !session.pending() {
                std::thread::sleep(Duration::from_millis(5));
            }
            session.run(|_| Err(anyhow::anyhow!("Native longlink response timed out")));
        });
        let reply = mailbox.submit_within(chunk(0), Duration::from_secs(5), Duration::from_secs(5));
        worker.join().unwrap();
        assert_eq!(
            sent(reply).as_deref(),
            Some("Native longlink response timed out")
        );
    }
}
