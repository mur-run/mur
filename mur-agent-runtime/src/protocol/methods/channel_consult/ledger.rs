//! E3 · single-use ledger of answered `(asker, nonce)` pairs (§D3.5), kept in
//! the answerer's own home.
//!
//! Defence in depth only: the signature already binds `channel_id`, and §D5
//! explains why a replay reveals nothing new. Entries are pruned once their
//! `Ask` has expired, because the expiry check (§D3.4) refuses those before
//! the ledger is consulted, so the file never outgrows one expiry window.
//!
//! The ledger is shared by every consult this runtime serves concurrently, so
//! a check-and-record is one critical section under an in-process mutex;
//! [`Ledger::release`] gives the pair back when the turn never ran.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

/// File name under the agent home.
pub const LEDGER_FILE: &str = "consult-ledger.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    asker: String,
    nonce: String,
    expires_at_ms: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Book {
    answered: Vec<Entry>,
}

pub struct Ledger {
    path: PathBuf,
    lock: Mutex<()>,
}

/// Outcome of [`Ledger::claim`].
#[derive(Debug, PartialEq, Eq)]
pub enum Claim {
    /// Newly recorded; this consult may answer.
    Fresh,
    /// Already recorded; refuse.
    Replayed,
}

impl Ledger {
    pub fn in_home(agent_home: &Path) -> Self {
        Self {
            path: agent_home.join(LEDGER_FILE),
            lock: Mutex::new(()),
        }
    }

    /// Record `(asker, nonce)` unless it is already there. An unreadable
    /// ledger is an error, never an empty one: forgetting what was answered
    /// would turn every replay into a fresh consult.
    pub async fn claim(
        &self,
        asker: &str,
        nonce: &str,
        expires_at_ms: i64,
        now_ms: i64,
    ) -> std::io::Result<Claim> {
        let _g = self.lock.lock().await;
        let mut book = self.read()?;
        book.answered.retain(|e| e.expires_at_ms >= now_ms);
        if book
            .answered
            .iter()
            .any(|e| e.asker == asker && e.nonce == nonce)
        {
            return Ok(Claim::Replayed);
        }
        book.answered.push(Entry {
            asker: asker.to_string(),
            nonce: nonce.to_string(),
            expires_at_ms,
        });
        self.write(&book)?;
        Ok(Claim::Fresh)
    }

    /// Give a claim back: the consult never started its turn, so a later
    /// consult may still answer the same `Ask`.
    pub async fn release(&self, asker: &str, nonce: &str) -> std::io::Result<()> {
        let _g = self.lock.lock().await;
        let mut book = self.read()?;
        let before = book.answered.len();
        book.answered
            .retain(|e| !(e.asker == asker && e.nonce == nonce));
        if book.answered.len() != before {
            self.write(&book)?;
        }
        Ok(())
    }

    fn read(&self) -> std::io::Result<Book> {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Book::default()),
            Err(e) => Err(e),
        }
    }

    fn write(&self, book: &Book) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(book)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        crate::tools::atomic_write::replace_contents(&self.path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_second_claim_for_the_same_pair_is_a_replay() {
        let td = tempfile::tempdir().unwrap();
        let l = Ledger::in_home(td.path());
        assert_eq!(l.claim("pm", "n1", 100, 0).await.unwrap(), Claim::Fresh);
        assert_eq!(l.claim("pm", "n1", 100, 0).await.unwrap(), Claim::Replayed);
        // Another asker, or another nonce, is its own pair.
        assert_eq!(l.claim("qa", "n1", 100, 0).await.unwrap(), Claim::Fresh);
        assert_eq!(l.claim("pm", "n2", 100, 0).await.unwrap(), Claim::Fresh);
    }

    #[tokio::test]
    async fn the_ledger_survives_a_restart() {
        let td = tempfile::tempdir().unwrap();
        Ledger::in_home(td.path())
            .claim("pm", "n1", 100, 0)
            .await
            .unwrap();
        let again = Ledger::in_home(td.path());
        assert_eq!(
            again.claim("pm", "n1", 100, 0).await.unwrap(),
            Claim::Replayed
        );
    }

    #[tokio::test]
    async fn a_released_claim_can_be_made_again() {
        let td = tempfile::tempdir().unwrap();
        let l = Ledger::in_home(td.path());
        l.claim("pm", "n1", 100, 0).await.unwrap();
        l.release("pm", "n1").await.unwrap();
        assert_eq!(l.claim("pm", "n1", 100, 0).await.unwrap(), Claim::Fresh);
    }

    #[tokio::test]
    async fn expired_entries_are_pruned() {
        let td = tempfile::tempdir().unwrap();
        let l = Ledger::in_home(td.path());
        l.claim("pm", "old", 10, 0).await.unwrap();
        l.claim("pm", "new", 1_000, 50).await.unwrap();
        let book = l.read().unwrap();
        assert_eq!(book.answered.len(), 1);
        assert_eq!(book.answered[0].nonce, "new");
    }

    #[tokio::test]
    async fn a_corrupt_ledger_is_an_error_not_an_empty_one() {
        let td = tempfile::tempdir().unwrap();
        std::fs::write(td.path().join(LEDGER_FILE), b"not json").unwrap();
        let l = Ledger::in_home(td.path());
        assert!(l.claim("pm", "n1", 100, 0).await.is_err());
    }
}
