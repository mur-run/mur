//! Version-neutral dump + diff for LanceDB tables, used to prove a LanceDB
//! upgrade does not silently reinterpret data already on disk.
//!
//! Run the same dump against the same frozen table copy once on the old build
//! and once on the new one, then compare the two dumps.
//!
//! ```sh
//! # 1. dump (run once per build)
//! LANCE_EXPORT_TABLE=/abs/path/index/patterns.lance \
//! LANCE_EXPORT_OUT=/abs/path/patterns.before.jsonl \
//!   cargo test -p mur-core --test lance_export -- --ignored --exact export_table_from_env
//!
//! # 2. compare
//! LANCE_COMPARE_A=/abs/path/patterns.before.jsonl \
//! LANCE_COMPARE_B=/abs/path/patterns.after.jsonl \
//!   cargo test -p mur-core --test lance_export -- --ignored --exact compare_dumps_from_env
//! ```
//!
//! Optional: `LANCE_EXPORT_KEY` (primary-key column; default: first of
//! `chunk_id`, `id`, `name` present), `LANCE_EXPORT_SAMPLE` (rows; default 5),
//! `LANCE_COMPARE_TOLERANCE` (absolute f32 epsilon; default 0 = bit-exact).
//!
//! Sandboxed runners: if the agent sandbox forbids executing files under
//! `$TMPDIR`, rustdoc and some tests fail spuriously. Point it inside the
//! target dir: `TMPDIR=$PWD/target/tmp cargo test ...`.
//!
//! Dump format (JSON Lines):
//! - line 1 `{"kind":"meta",...}`: lancedb/lance versions (from Cargo.lock),
//!   export time, table, key, encoding. Never compared; printed for the record.
//! - line 2 `{"kind":"schema","fields":[...]}`: canonical type strings, not
//!   arrow `Debug` output, so an arrow major bump does not produce a false diff.
//! - then `{"kind":"row","row":{...}}`, sorted by key (ties by full row).
//!   Every Float32 — scalar or vector element — is `{"f32_bits": u32}` /
//!   `{"f32_bits": [u32, ...]}`: IEEE-754 bits, lossless.

use arrow_array::cast::AsArray;
use arrow_array::types::{Float32Type, Int8Type, Int64Type, UInt32Type, UInt64Type};
use arrow_array::{Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use futures::TryStreamExt;
use lancedb::query::ExecutableQuery;
use serde_json::{Map, Value, json};
use std::path::Path;

const KEY_CANDIDATES: &[&str] = &["chunk_id", "id", "name"];
const DEFAULT_SAMPLE: usize = 5;
const CARGO_LOCK: &str = include_str!("../../Cargo.lock");
const LOCKED_CRATES: &[&str] = &["lancedb", "lance"];

// ---------------------------------------------------------------- export ---

/// Dump `table_path` (a `<name>.lance` directory) as JSON Lines. Panics with a
/// readable message on any failure: this is a test tool, loud is correct.
pub async fn export_table(table_path: &Path, key: Option<&str>, sample: usize) -> Vec<String> {
    let parent = table_path.parent().expect("table path has a parent dir");
    let name = table_path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".lance"))
        .unwrap_or_else(|| panic!("{} is not a <name>.lance dir", table_path.display()));
    let db = lancedb::connect(parent.to_str().expect("utf-8 path"))
        .execute()
        .await
        .unwrap_or_else(|e| panic!("connect {}: {e}", parent.display()));
    let table = db
        .open_table(name)
        .execute()
        .await
        .unwrap_or_else(|e| panic!("open_table {name}: {e}"));
    let schema = table
        .schema()
        .await
        .unwrap_or_else(|e| panic!("schema: {e}"));
    let key = match key {
        Some(k) => {
            assert!(
                schema.field_with_name(k).is_ok(),
                "key column {k:?} not in table"
            );
            k.to_string()
        }
        None => KEY_CANDIDATES
            .iter()
            .find(|k| schema.field_with_name(k).is_ok())
            .unwrap_or_else(|| {
                panic!("no key column among {KEY_CANDIDATES:?}; set LANCE_EXPORT_KEY")
            })
            .to_string(),
    };
    let batches: Vec<RecordBatch> = table
        .query()
        .execute()
        .await
        .unwrap_or_else(|e| panic!("query: {e}"))
        .try_collect()
        .await
        .unwrap_or_else(|e| panic!("collect: {e}"));

    let mut rows: Vec<(String, Map<String, Value>)> = Vec::new();
    for batch in &batches {
        for i in 0..batch.num_rows() {
            let row = row_to_json(batch, i);
            rows.push((row[&key].to_string(), row));
        }
    }
    let total = rows.len();
    // Sort by key, ties broken by the whole row, so the order never depends
    // on how Lance happens to lay out fragments.
    rows.sort_by_cached_key(|(k, r)| (k.clone(), Value::Object(r.clone()).to_string()));

    let mut out = vec![
        json!({
            "kind": "meta",
            "table": table_path.display().to_string(),
            "key": key,
            "total_rows": total,
            "sample": sample,
            "versions": locked_versions(),
            "exported_at": chrono::Utc::now().to_rfc3339(),
            "float_encoding": "f32_bits: IEEE-754 bits as u32 (scalar or array)",
        })
        .to_string(),
        json!({"kind": "schema", "fields": schema_json(&schema)}).to_string(),
    ];
    out.extend(
        rows.into_iter()
            .take(sample)
            .map(|(_, r)| json!({"kind": "row", "row": r}).to_string()),
    );
    out
}

fn locked_versions() -> Map<String, Value> {
    let mut m = Map::new();
    let mut lines = CARGO_LOCK.lines();
    while let Some(line) = lines.next() {
        for c in LOCKED_CRATES {
            if line == format!("name = \"{c}\"")
                && let Some(v) = lines.next().and_then(|l| l.strip_prefix("version = "))
            {
                m.insert(
                    (*c).to_string(),
                    Value::String(v.trim_matches('"').to_string()),
                );
            }
        }
    }
    m
}

fn type_str(dt: &DataType) -> String {
    match dt {
        DataType::FixedSizeList(f, n) => format!("FixedSizeList<{}>[{n}]", item_str(f)),
        DataType::List(f) => format!("List<{}>", item_str(f)),
        other => format!("{other:?}"),
    }
}

fn item_str(f: &Field) -> String {
    let null = if f.is_nullable() {
        "nullable"
    } else {
        "non-null"
    };
    format!("{},{null}", type_str(f.data_type()))
}

fn schema_json(schema: &Schema) -> Vec<Value> {
    schema
        .fields()
        .iter()
        .map(|f| json!({"name": f.name(), "type": type_str(f.data_type()), "nullable": f.is_nullable()}))
        .collect()
}

fn row_to_json(batch: &RecordBatch, i: usize) -> Map<String, Value> {
    batch
        .schema()
        .fields()
        .iter()
        .zip(batch.columns())
        .map(|(f, col)| (f.name().clone(), cell(col.as_ref(), i, f.name())))
        .collect()
}

fn cell(col: &dyn Array, i: usize, name: &str) -> Value {
    if col.is_null(i) {
        return Value::Null;
    }
    match col.data_type() {
        DataType::Utf8 => json!(col.as_string::<i32>().value(i)),
        DataType::LargeUtf8 => json!(col.as_string::<i64>().value(i)),
        DataType::Boolean => json!(col.as_boolean().value(i)),
        DataType::Int8 => json!(col.as_primitive::<Int8Type>().value(i)),
        DataType::Int64 => json!(col.as_primitive::<Int64Type>().value(i)),
        DataType::UInt32 => json!(col.as_primitive::<UInt32Type>().value(i)),
        DataType::UInt64 => json!(col.as_primitive::<UInt64Type>().value(i)),
        DataType::Float32 => {
            json!({"f32_bits": col.as_primitive::<Float32Type>().value(i).to_bits()})
        }
        DataType::FixedSizeList(_, _) => f32_list(col.as_fixed_size_list().value(i).as_ref(), name),
        DataType::List(_) => f32_list(col.as_list::<i32>().value(i).as_ref(), name),
        other => panic!("column {name}: unsupported type {other:?}; extend cell()"),
    }
}

fn f32_list(values: &dyn Array, name: &str) -> Value {
    let Some(v) = values.as_primitive_opt::<Float32Type>() else {
        panic!(
            "column {name}: list of {:?}, only Float32 supported",
            values.data_type()
        );
    };
    let bits: Vec<Value> = (0..v.len())
        .map(|j| {
            if v.is_null(j) {
                Value::Null
            } else {
                json!(v.value(j).to_bits())
            }
        })
        .collect();
    json!({"f32_bits": bits})
}

// --------------------------------------------------------------- compare ---

/// Compare two dumps. `meta` is ignored; schema must match exactly; rows must
/// match field-for-field. Floats are bit-exact at `tolerance == 0.0`, else
/// equal within an absolute epsilon (NaN equals NaN).
pub fn compare_dumps(a: &str, b: &str, tolerance: f32) -> Result<(), Vec<String>> {
    let (a, b) = (parse_dump(a), parse_dump(b));
    let mut errs = Vec::new();
    if a.0 != b.0 {
        errs.push(format!("schema differs:\n  A: {}\n  B: {}", a.0, b.0));
    }
    if a.1.len() != b.1.len() {
        errs.push(format!(
            "row count differs: A={} B={}",
            a.1.len(),
            b.1.len()
        ));
    }
    for (n, (ra, rb)) in a.1.iter().zip(&b.1).enumerate() {
        let mut fields: Vec<&String> = ra.keys().chain(rb.keys()).collect();
        fields.sort();
        fields.dedup();
        for f in fields {
            let (va, vb) = (ra.get(f), rb.get(f));
            if !value_eq(va, vb, tolerance) {
                errs.push(format!("row {n} field {f}: A={va:?} B={vb:?}"));
            }
        }
    }
    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

fn parse_dump(s: &str) -> (Value, Vec<Map<String, Value>>) {
    let mut schema = Value::Null;
    let mut rows = Vec::new();
    for line in s.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).unwrap_or_else(|e| panic!("bad dump line: {e}"));
        match v["kind"].as_str() {
            Some("schema") => schema = v["fields"].clone(),
            Some("row") => rows.push(v["row"].as_object().cloned().expect("row object")),
            _ => {}
        }
    }
    (schema, rows)
}

fn value_eq(a: Option<&Value>, b: Option<&Value>, tol: f32) -> bool {
    let bits = |v: Option<&Value>| v.and_then(|v| v.get("f32_bits")).cloned();
    match (bits(a), bits(b)) {
        (Some(Value::Array(x)), Some(Value::Array(y))) => {
            x.len() == y.len() && x.iter().zip(&y).all(|(p, q)| f32_eq(p, q, tol))
        }
        (Some(x), Some(y)) => f32_eq(&x, &y, tol),
        _ => a == b,
    }
}

fn f32_eq(a: &Value, b: &Value, tol: f32) -> bool {
    match (a.as_u64(), b.as_u64()) {
        (Some(x), Some(y)) if tol == 0.0 => x == y,
        (Some(x), Some(y)) => {
            let (x, y) = (f32::from_bits(x as u32), f32::from_bits(y as u32));
            (x.is_nan() && y.is_nan()) || (x - y).abs() <= tol
        }
        _ => a == b, // both null, or a type mismatch that must fail
    }
}

// ----------------------------------------------------------------- tests ---

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::{
        FixedSizeListArray, Float32Array, RecordBatchIterator, StringArray, UInt32Array,
    };
    use std::sync::Arc;

    const DIM: i32 = 3;

    fn schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new("line_start", DataType::UInt32, false),
            Field::new("score", DataType::Float32, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), DIM),
                true,
            ),
        ]))
    }

    /// Rows inserted deliberately out of key order.
    async fn make_table(dir: &Path) -> std::path::PathBuf {
        let ids = vec!["c", "a", "d", "b"];
        let vectors: Vec<f32> = vec![
            3.0, 3.1, 3.2, //
            1.0, 1.1, 1.2, //
            4.0, 4.1, 4.2, //
            2.0, 2.1, 2.2,
        ];
        let item = Arc::new(Field::new("item", DataType::Float32, true));
        let vec_arr =
            FixedSizeListArray::new(item, DIM, Arc::new(Float32Array::from(vectors)), None);
        let batch = RecordBatch::try_new(
            schema(),
            vec![
                Arc::new(StringArray::from(ids)),
                Arc::new(UInt32Array::from(vec![30, 10, 40, 20])),
                Arc::new(Float32Array::from(vec![0.3, 0.1, 0.4, 0.2])),
                Arc::new(vec_arr),
            ],
        )
        .unwrap();
        let db = lancedb::connect(dir.to_str().unwrap())
            .execute()
            .await
            .unwrap();
        let reader: Box<dyn arrow_array::RecordBatchReader + Send> =
            Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema()));
        db.create_table("t", reader).execute().await.unwrap();
        dir.join("t.lance")
    }

    fn rows(dump: &[String]) -> Vec<Value> {
        dump.iter()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|v| v["kind"] == "row")
            .map(|v| v["row"].clone())
            .collect()
    }

    #[tokio::test]
    async fn export_sorts_by_key_and_samples() {
        let tmp = tempfile::tempdir().unwrap();
        let dump = export_table(&make_table(tmp.path()).await, None, 3).await;
        let r = rows(&dump);
        let ids: Vec<&str> = r.iter().map(|v| v["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(r[0]["line_start"], 10);
    }

    #[tokio::test]
    async fn export_floats_are_lossless_bits() {
        let tmp = tempfile::tempdir().unwrap();
        let dump = export_table(&make_table(tmp.path()).await, Some("id"), 1).await;
        let r = &rows(&dump)[0];
        assert_eq!(r["score"]["f32_bits"], 0.1f32.to_bits());
        let v: Vec<u64> = r["vector"]["f32_bits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_u64().unwrap())
            .collect();
        let want: Vec<u64> = [1.0f32, 1.1, 1.2]
            .iter()
            .map(|f| f.to_bits() as u64)
            .collect();
        assert_eq!(v, want);
    }

    #[tokio::test]
    async fn export_schema_is_canonical_and_meta_has_versions() {
        let tmp = tempfile::tempdir().unwrap();
        let dump = export_table(&make_table(tmp.path()).await, None, 1).await;
        let meta: Value = serde_json::from_str(&dump[0]).unwrap();
        assert_eq!(meta["kind"], "meta");
        assert_eq!(meta["key"], "id");
        assert!(
            meta["versions"]["lancedb"]
                .as_str()
                .unwrap()
                .starts_with("0.")
        );
        let schema: Value = serde_json::from_str(&dump[1]).unwrap();
        assert_eq!(schema["kind"], "schema");
        assert_eq!(
            schema["fields"][3],
            json!({"name": "vector", "type": "FixedSizeList<Float32,nullable>[3]", "nullable": true})
        );
    }

    #[tokio::test]
    async fn export_is_reproducible_and_self_compares_clean() {
        let tmp = tempfile::tempdir().unwrap();
        let path = make_table(tmp.path()).await;
        let a = export_table(&path, None, 5).await.join("\n");
        let b = export_table(&path, None, 5).await.join("\n");
        assert_eq!(compare_dumps(&a, &b, 0.0), Ok(()));
    }

    fn dump_with(score: f32) -> String {
        [
            json!({"kind": "meta", "exported_at": score.to_string()}),
            json!({"kind": "schema", "fields": [{"name": "id", "type": "Utf8", "nullable": false}]}),
            json!({"kind": "row", "row": {"id": "a", "score": {"f32_bits": score.to_bits()}}}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
    }

    #[test]
    fn compare_bit_exact_by_default_and_tolerance_relaxes() {
        let (a, b) = (dump_with(1.0), dump_with(1.0 + f32::EPSILON));
        let errs = compare_dumps(&a, &b, 0.0).unwrap_err();
        assert!(errs[0].contains("score"), "{errs:?}");
        assert_eq!(compare_dumps(&a, &b, 1e-6), Ok(()));
    }

    #[test]
    fn compare_flags_schema_and_value_changes() {
        let a = dump_with(1.0);
        let b = a
            .replace("\"Utf8\"", "\"LargeUtf8\"")
            .replace("\"a\"", "\"z\"");
        let errs = compare_dumps(&a, &b, 1.0).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("schema")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("id")), "{errs:?}");
    }

    // --------------------------------------------------- env-driven runs ---

    fn env(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("set {name}; see the header of this file"))
    }

    #[tokio::test]
    #[ignore = "manual: needs LANCE_EXPORT_TABLE and LANCE_EXPORT_OUT"]
    async fn export_table_from_env() {
        let key = std::env::var("LANCE_EXPORT_KEY").ok();
        let sample = std::env::var("LANCE_EXPORT_SAMPLE")
            .map(|s| s.parse().expect("LANCE_EXPORT_SAMPLE must be an integer"))
            .unwrap_or(DEFAULT_SAMPLE);
        let dump = export_table(
            Path::new(&env("LANCE_EXPORT_TABLE")),
            key.as_deref(),
            sample,
        )
        .await;
        let out = env("LANCE_EXPORT_OUT");
        std::fs::write(&out, dump.join("\n") + "\n").unwrap();
        println!("wrote {} lines to {out}", dump.len());
    }

    #[test]
    #[ignore = "manual: needs LANCE_COMPARE_A and LANCE_COMPARE_B"]
    fn compare_dumps_from_env() {
        let read = |n| std::fs::read_to_string(env(n)).unwrap();
        let tol: f32 = std::env::var("LANCE_COMPARE_TOLERANCE")
            .map(|s| s.parse().expect("LANCE_COMPARE_TOLERANCE must be a float"))
            .unwrap_or(0.0);
        let (a, b) = (read("LANCE_COMPARE_A"), read("LANCE_COMPARE_B"));
        for d in [&a, &b] {
            println!("{}", d.lines().next().unwrap_or(""));
        }
        if let Err(errs) = compare_dumps(&a, &b, tol) {
            panic!(
                "{} difference(s), tolerance {tol}:\n{}",
                errs.len(),
                errs.join("\n")
            );
        }
        println!("identical (tolerance {tol})");
    }
}
