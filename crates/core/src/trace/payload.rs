//! Payload rule: string fields larger than `trace.inline_payload_max_bytes`
//! are moved to blobs. The string is replaced in place by
//! `{"$blob": "<sha256>", "bytes": n}`; readers resolve it with
//! [`crate::trace::TraceStore::read_blob`].

use serde_json::{Map, Value};

use super::BlobId;
use super::error::TraceError;

/// Key of an inline blob reference object.
pub const BLOB_REF_KEY: &str = "$blob";

/// Media type given to spilled strings.
pub const SPILLED_MEDIA_TYPE: &str = "text/plain; charset=utf-8";

/// Replaces every string longer than `max` bytes with a blob reference;
/// `store` receives the string and returns its blob id.
pub(super) fn spill_strings(
    value: &mut Value,
    max: usize,
    store: &mut dyn FnMut(&str) -> Result<BlobId, TraceError>,
) -> Result<usize, TraceError> {
    let mut spilled = 0;
    match value {
        Value::String(s) if s.len() > max => {
            let id = store(s)?;
            let mut obj = Map::with_capacity(2);
            obj.insert(BLOB_REF_KEY.to_owned(), Value::String(id.into_string()));
            obj.insert("bytes".to_owned(), Value::from(s.len()));
            *value = Value::Object(obj);
            spilled += 1;
        }
        Value::Array(items) => {
            for item in items {
                spilled += spill_strings(item, max, store)?;
            }
        }
        Value::Object(fields) => {
            for field in fields.values_mut() {
                spilled += spill_strings(field, max, store)?;
            }
        }
        _ => {}
    }
    Ok(spilled)
}

/// Blob ids referenced from inside a payload (spilled strings and any
/// explicit `{"$blob": id}` objects such as `raw_sse_blob_id`).
pub fn blob_refs(value: &Value) -> Vec<BlobId> {
    let mut out = Vec::new();
    collect(value, &mut out);
    out
}

fn collect(value: &Value, out: &mut Vec<BlobId>) {
    match value {
        Value::Object(fields) => {
            if let Some(Value::String(id)) = fields.get(BLOB_REF_KEY) {
                out.push(BlobId::from(id.as_str()));
            }
            for v in fields.values() {
                collect(v, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                collect(v, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_oversized_strings_move() {
        let mut v = json!({
            "short": "abc",
            "long": "x".repeat(10),
            "nested": [{"deep": "y".repeat(20)}, "z"],
            "n": 42
        });
        let mut calls = Vec::new();
        let n = spill_strings(&mut v, 5, &mut |s| {
            calls.push(s.len());
            Ok(BlobId::from(format!("id{}", s.len())))
        })
        .unwrap();
        assert_eq!(n, 2);
        assert_eq!(calls, vec![10, 20]);
        assert_eq!(v["short"], "abc");
        assert_eq!(v["long"], json!({"$blob": "id10", "bytes": 10}));
        assert_eq!(
            v["nested"][0]["deep"],
            json!({"$blob": "id20", "bytes": 20})
        );
        assert_eq!(v["nested"][1], "z");
        assert_eq!(
            blob_refs(&v),
            vec![BlobId::from("id10"), BlobId::from("id20")]
        );
    }
}
