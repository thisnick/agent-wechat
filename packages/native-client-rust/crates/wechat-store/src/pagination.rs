use super::*;
use rusqlite::OptionalExtension;

pub struct PaginationView {
    pub query_hash: String,
    pub event_cursor: String,
    pub expires_at: i64,
    pub payload: Vec<u8>,
}
impl Store {
    pub fn pagination_views_exist(&self) -> Result<bool> {
        for account in self.accounts()? {
            if self.control(&account.id, |db| {
                Ok(
                    db.query_row("SELECT EXISTS(SELECT 1 FROM pagination_views)", [], |r| {
                        r.get::<_, bool>(0)
                    })?,
                )
            })? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub fn event_boundary(&self, account: &str) -> Result<String> {
        self.control(account, |db| {
            Ok(db
                .query_row("SELECT coalesce(max(sequence),0) FROM events", [], |r| {
                    r.get::<_, i64>(0)
                })?
                .to_string())
        })
    }
    pub fn save_pagination_view(
        &self,
        account: &str,
        id: &str,
        query_hash: &str,
        event_cursor: &str,
        expires_at: i64,
        payload: &[u8],
    ) -> Result<()> {
        if payload.len() > 16 * 1024 * 1024 {
            bail!("pagination_capacity_exceeded");
        }
        self.control(account, |db| {
            let tx = db.transaction()?;
            tx.execute("DELETE FROM pagination_views WHERE expires_at<=?1", [unix_seconds()])?;
            // Evicted views fail explicitly on continuation, never restart silently.
            loop {
                let (count, bytes):(i64,i64)=tx.query_row("SELECT count(*),coalesce(sum(length(payload)),0) FROM pagination_views", [], |r|Ok((r.get(0)?,r.get(1)?)))?;
                if count<64 && bytes+(payload.len() as i64)<=64*1024*1024 { break; }
                tx.execute("DELETE FROM pagination_views WHERE id=(SELECT id FROM pagination_views ORDER BY created_at,id LIMIT 1)", [])?;
            }
            tx.execute("INSERT INTO pagination_views VALUES(?1,?2,?3,?4,?5,?6)", params![id,query_hash,event_cursor,expires_at,payload,unix_seconds()])?;
            tx.commit()?;
            Ok(())
        })
    }
    pub fn pagination_view(&self, account: &str, id: &str) -> Result<PaginationView> {
        self.control(account, |db| {
            let row = db.query_row("SELECT query_hash,event_cursor,expires_at,payload FROM pagination_views WHERE id=?1", [id], |r| Ok(PaginationView { query_hash:r.get(0)?,event_cursor:r.get(1)?,expires_at:r.get(2)?,payload:r.get(3)? })).optional()?;
            let view=row.context("pagination_cursor_expired")?;
            if view.expires_at<=unix_seconds() { bail!("pagination_cursor_expired"); }
            Ok(view)
        })
    }
}
