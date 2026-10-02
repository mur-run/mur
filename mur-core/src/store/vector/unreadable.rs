//! Typed failure for a LanceDB table that exists on disk but cannot be read.
//!
//! LanceDB has no "format version mismatch" error variant (checked against
//! lancedb 0.37.1 / lance-core 10.0.0), so a table is classified by *stage plus
//! exclusion*: any failure opening or reading a table that exists is
//! `Unreadable`, except not-found (existing create/empty path) and
//! filesystem-level problems (`Io`), where a rebuild would not help.
//!
//! This type never names a rebuild command. Only the owner of a table knows
//! which command rebuilds it, so callers attach the hint with
//! [`with_rebuild_hint`].

use std::path::{Path, PathBuf};

use anyhow::Result;
use futures::TryStreamExt;
use lancedb::query::{ExecutableQuery, QueryBase};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Failure modes for an existing LanceDB table.
#[derive(Debug, thiserror::Error)]
pub enum VectorStoreError {
    /// The table exists but LanceDB could not open or read it. Every table MUR
    /// keeps in LanceDB is derived data, so the remedy is a rebuild.
    #[error("LanceDB table `{table}` could not be read")]
    Unreadable {
        table: String,
        #[source]
        cause: BoxError,
    },
    /// The table could not be reached at the filesystem level. Rebuilding
    /// would not help, so no rebuild hint is attached.
    #[error("LanceDB table `{table}` at {} is not accessible", path.display())]
    Io {
        table: String,
        path: PathBuf,
        #[source]
        cause: BoxError,
    },
}

/// Classify an error raised while opening or reading the existing `table`.
///
/// Errors that did not come from LanceDB (schema checks, bad input) pass
/// through untouched. `table_path` is used only in the `Io` message.
pub fn classify(table: &str, table_path: &Path, err: anyhow::Error) -> anyhow::Error {
    let Some(lance) = err.downcast_ref::<lancedb::Error>() else {
        return err;
    };
    match lance {
        lancedb::Error::TableNotFound { .. } | lancedb::Error::InvalidInput { .. } => err,
        lancedb::Error::NotSupported { .. } => io_error(table, table_path, err),
        _ if is_access_denied(lance) => io_error(table, table_path, err),
        _ => anyhow::Error::new(VectorStoreError::Unreadable {
            table: table.to_string(),
            cause: err.into(),
        }),
    }
}

fn io_error(table: &str, table_path: &Path, err: anyhow::Error) -> anyhow::Error {
    anyhow::Error::new(VectorStoreError::Io {
        table: table.to_string(),
        path: table_path.to_path_buf(),
        cause: err.into(),
    })
}

/// Run a read against an existing table, turning both LanceDB errors and
/// LanceDB panics into a classified error.
///
/// lance 10 panics instead of erroring on some malformed manifests
/// (`dataset.rs`, integer underflow while locating the metadata block), so an
/// error-only wrapper would still crash the process.
pub async fn guard<T, F>(table: &str, table_path: &Path, fut: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>>,
{
    use futures::FutureExt;
    match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(classify(table, table_path, e)),
        Err(panic) => {
            let msg = panic
                .downcast_ref::<&str>()
                .map(|s| (*s).to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "LanceDB panicked".to_string());
            Err(anyhow::Error::new(VectorStoreError::Unreadable {
                table: table.to_string(),
                cause: format!("LanceDB panicked while reading: {msg}").into(),
            }))
        }
    }
}

/// True when the error chain carries a filesystem access failure.
///
/// LanceDB flattens object_store errors into display text in some paths, so
/// the message is checked alongside the typed `io::Error` walk.
fn is_access_denied(err: &lancedb::Error) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = current {
        if let Some(io) = e.downcast_ref::<std::io::Error>()
            && matches!(
                io.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotADirectory
            )
        {
            return true;
        }
        let text = e.to_string();
        if text.contains("Permission denied") || text.contains("Not a directory") {
            return true;
        }
        current = e.source();
    }
    false
}

/// True when `err` (or anything it wraps) is [`VectorStoreError::Unreadable`].
pub fn is_unreadable(err: &anyhow::Error) -> bool {
    err.chain().any(|c| {
        matches!(
            c.downcast_ref::<VectorStoreError>(),
            Some(VectorStoreError::Unreadable { .. })
        )
    })
}

/// Attach the owner's rebuild command to an `Unreadable` error. Any other
/// error passes through untouched.
pub fn with_rebuild_hint(err: anyhow::Error, what: &str, command: &str) -> anyhow::Error {
    if is_unreadable(&err) {
        err.context(rebuild_message(what, command))
    } else {
        err
    }
}

/// Rebuild commands per index owner (spec §3). Kept here so every caller of
/// the same table prints the same command.
pub mod hint {
    pub const PATTERNS: (&str, &str) = ("pattern", "mur internals reindex");
    pub const SKILLS: (&str, &str) = ("skill embedding", "mur skill reindex-vec");
    pub const CODEBASE: (&str, &str) = ("codebase", "mur project index --rebuild");
    pub const CONVERSATIONS: (&str, &str) = ("conversations", "mur chat reindex");
    /// Source connectors rebuild per id, so the command takes it.
    pub fn sources(id: Option<&str>) -> (&'static str, String) {
        let id = id.unwrap_or("<id>");
        ("source", format!("mur source reindex {id}"))
    }
}

/// [`with_rebuild_hint`] taking a [`hint`] pair.
pub fn hinted(err: anyhow::Error, (what, command): (&str, &str)) -> anyhow::Error {
    with_rebuild_hint(err, what, command)
}

/// User-facing message for an unreadable index. It says "may", not "was":
/// the classification cannot prove a version mismatch was the cause.
pub fn rebuild_message(what: &str, command: &str) -> String {
    format!(
        "The {what} index could not be read (it may have been written by an older \
         LanceDB version). It is derived data; rebuild it with `{command}`"
    )
}

/// Probe `table` and drop it when it exists but is unreadable, so a rebuild
/// command can recreate it instead of failing on the same error it is meant
/// to fix. Returns `true` when the table was dropped.
pub async fn drop_if_unreadable(db: &lancedb::Connection, table: &str) -> Result<bool> {
    let names = db.table_names().execute().await?;
    if !names.iter().any(|n| n == table) {
        return Ok(false);
    }
    let table_path = Path::new(db.uri()).join(format!("{table}.lance"));
    let probe = guard(table, &table_path, async {
        let t = db.open_table(table).execute().await?;
        t.query()
            .limit(1)
            .execute()
            .await?
            .try_collect::<Vec<_>>()
            .await?;
        Ok(())
    })
    .await;
    match probe {
        Ok(()) => Ok(false),
        Err(e) if is_unreadable(&e) => {
            tracing::warn!(table, error = %format!("{e:#}"), "dropping unreadable LanceDB table");
            db.drop_table(table, &[]).await?;
            Ok(true)
        }
        Err(e) => Err(e),
    }
}

/// [`drop_if_unreadable`] for a LanceDB database directory, for rebuild
/// commands that hold a `dyn VectorStore` rather than a connection. A missing
/// directory has nothing to drop.
pub async fn drop_unreadable_at(db_dir: &Path, table: &str) -> Result<bool> {
    if !db_dir.exists() {
        return Ok(false);
    }
    let uri = db_dir
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF-8 index path: {}", db_dir.display()))?;
    let db = lancedb::connect(uri).execute().await?;
    let dropped = drop_if_unreadable(&db, table).await?;
    if dropped {
        eprintln!("note: dropped unreadable index table `{table}`; it will be rebuilt");
    }
    Ok(dropped)
}

#[cfg(test)]
#[path = "unreadable_tests.rs"]
mod tests;
