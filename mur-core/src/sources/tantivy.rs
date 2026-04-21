//! Tantivy-backed BM25 index for source chunks.
//!
//! Unified across vector backends (see design spec §8.3): regardless of
//! whether LanceDB or Qdrant holds the vectors, BM25 results stay
//! byte-identical so swapping backends doesn't change rank order.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tantivy::{
    Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, doc,
    query::QueryParser,
    schema::{STORED, STRING, Schema, TEXT, Value},
};

/// A single BM25 hit.
#[derive(Debug, Clone)]
pub struct Bm25Hit {
    pub chunk_id: String,
    pub source_id: String,
    pub external_id: String,
    pub score: f32,
}

/// Opens / creates a tantivy index at `<root>/tantivy/sources/`.
pub struct TantivyIndex {
    index: Index,
    reader: IndexReader,
    #[allow(dead_code)]
    dir: PathBuf,
}

impl TantivyIndex {
    pub fn open_or_create(root: &Path) -> Result<Self> {
        let dir = root.join("tantivy").join("sources");
        std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;

        let mut builder = Schema::builder();
        builder.add_text_field("chunk_id", STRING | STORED);
        builder.add_text_field("source_id", STRING | STORED);
        builder.add_text_field("external_id", STRING | STORED);
        builder.add_text_field("text", TEXT);
        let schema = builder.build();

        let index =
            Index::open_in_dir(&dir).or_else(|_| Index::create_in_dir(&dir, schema.clone()))?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;

        Ok(Self { index, reader, dir })
    }

    /// Upsert by chunk_id: delete-then-add.
    pub fn upsert(&self, rows: &[(String, String, String, String)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let schema = self.index.schema();
        let chunk_id_f = schema.get_field("chunk_id").unwrap();
        let source_id_f = schema.get_field("source_id").unwrap();
        let external_id_f = schema.get_field("external_id").unwrap();
        let text_f = schema.get_field("text").unwrap();

        let mut writer: IndexWriter = self.index.writer(50_000_000)?;
        for (chunk_id, _, _, _) in rows {
            writer.delete_term(tantivy::Term::from_field_text(chunk_id_f, chunk_id));
        }
        for (chunk_id, source_id, external_id, text) in rows {
            writer.add_document(doc!(
                chunk_id_f => chunk_id.as_str(),
                source_id_f => source_id.as_str(),
                external_id_f => external_id.as_str(),
                text_f => text.as_str(),
            ))?;
        }
        writer.commit()?;
        self.reader.reload()?;
        Ok(())
    }

    pub fn search(
        &self,
        query: &str,
        k: usize,
        source_ids: Option<&[String]>,
    ) -> Result<Vec<Bm25Hit>> {
        let searcher = self.reader.searcher();
        let schema = self.index.schema();
        let text_f = schema.get_field("text").unwrap();
        let chunk_id_f = schema.get_field("chunk_id").unwrap();
        let source_id_f = schema.get_field("source_id").unwrap();
        let external_id_f = schema.get_field("external_id").unwrap();

        let parser = QueryParser::for_index(&self.index, vec![text_f]);
        let parsed = match parser.parse_query(query) {
            Ok(q) => q,
            Err(_) => return Ok(vec![]),
        };
        let top_k = searcher.search(&parsed, &tantivy::collector::TopDocs::with_limit(k * 4))?;

        let mut hits: Vec<Bm25Hit> = Vec::new();
        for (score, addr) in top_k {
            let d: TantivyDocument = searcher.doc(addr)?;
            let chunk_id = d
                .get_first(chunk_id_f)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let source_id = d
                .get_first(source_id_f)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let external_id = d
                .get_first(external_id_f)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if source_ids.is_some_and(|allow| !allow.iter().any(|s| s == &source_id)) {
                continue;
            }
            hits.push(Bm25Hit {
                chunk_id,
                source_id,
                external_id,
                score,
            });
            if hits.len() >= k {
                break;
            }
        }
        Ok(hits)
    }

    #[allow(dead_code)] // called by sync remove path wired in P1.4
    pub fn delete_by_chunk_ids(&self, chunk_ids: &[String]) -> Result<()> {
        if chunk_ids.is_empty() {
            return Ok(());
        }
        let chunk_id_f = self.index.schema().get_field("chunk_id").unwrap();
        let mut writer: IndexWriter = self.index.writer(50_000_000)?;
        for id in chunk_ids {
            writer.delete_term(tantivy::Term::from_field_text(chunk_id_f, id));
        }
        writer.commit()?;
        self.reader.reload()?;
        Ok(())
    }

    pub fn delete_by_source(&self, source_id: &str) -> Result<()> {
        let source_id_f = self.index.schema().get_field("source_id").unwrap();
        let mut writer: IndexWriter = self.index.writer(50_000_000)?;
        writer.delete_term(tantivy::Term::from_field_text(source_id_f, source_id));
        writer.commit()?;
        self.reader.reload()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn mk_row(cid: &str, sid: &str, ext: &str, text: &str) -> (String, String, String, String) {
        (cid.into(), sid.into(), ext.into(), text.into())
    }

    #[test]
    fn upsert_and_search_basic() {
        let tmp = TempDir::new().unwrap();
        let idx = TantivyIndex::open_or_create(tmp.path()).unwrap();
        idx.upsert(&[
            mk_row("c1", "o:a", "doc1.md", "rust async programming with tokio"),
            mk_row("c2", "o:a", "doc2.md", "JVM garbage collection overview"),
        ])
        .unwrap();
        let hits = idx.search("tokio async", 5, None).unwrap();
        assert!(!hits.is_empty(), "BM25 returned nothing");
        assert_eq!(hits[0].chunk_id, "c1");
    }

    #[test]
    fn source_filter_works() {
        let tmp = TempDir::new().unwrap();
        let idx = TantivyIndex::open_or_create(tmp.path()).unwrap();
        idx.upsert(&[
            mk_row("c1", "o:a", "d1", "rust async"),
            mk_row("c2", "o:b", "d2", "rust async"),
        ])
        .unwrap();
        let only_a = idx.search("rust async", 5, Some(&["o:a".into()])).unwrap();
        assert_eq!(only_a.len(), 1);
        assert_eq!(only_a[0].source_id, "o:a");
    }

    #[test]
    fn delete_by_chunk_ids_removes_entries() {
        let tmp = TempDir::new().unwrap();
        let idx = TantivyIndex::open_or_create(tmp.path()).unwrap();
        idx.upsert(&[mk_row("c1", "s", "d1", "alpha beta")])
            .unwrap();
        idx.delete_by_chunk_ids(&["c1".into()]).unwrap();
        let hits = idx.search("alpha", 5, None).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn delete_by_source_clears() {
        let tmp = TempDir::new().unwrap();
        let idx = TantivyIndex::open_or_create(tmp.path()).unwrap();
        idx.upsert(&[
            mk_row("c1", "s:keep", "d1", "alpha"),
            mk_row("c2", "s:drop", "d2", "alpha"),
        ])
        .unwrap();
        idx.delete_by_source("s:drop").unwrap();
        let hits = idx.search("alpha", 5, None).unwrap();
        assert!(hits.iter().all(|h| h.source_id == "s:keep"));
    }
}
