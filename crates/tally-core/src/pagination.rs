use std::collections::VecDeque;

use serde_json::Value;
use thiserror::Error;

use crate::wire::FRAME_CAP_BYTES;

pub const DEFAULT_PAGE_ITEMS: usize = 100;
pub const MAX_PAGE_ITEMS: usize = 1_000;
const PAGE_RESULT_CAP_BYTES: usize = FRAME_CAP_BYTES * 3 / 4;
const MAX_SNAPSHOTS: usize = 32;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PaginationError {
    #[error("page limit must be between 1 and {MAX_PAGE_ITEMS}")]
    InvalidLimit,
    #[error("invalid pagination cursor")]
    InvalidCursor,
    #[error("pagination cursor expired")]
    CursorExpired,
    #[error("pagination cursor belongs to a different query")]
    CursorMismatch,
    #[error("collection response has no items array")]
    InvalidEnvelope,
    #[error("one collection item exceeds the bounded response size")]
    ItemTooLarge,
}

#[derive(Debug, Clone)]
struct Snapshot {
    id: u64,
    method: String,
    fingerprint: String,
    template: Value,
    items: Vec<Value>,
}

#[derive(Debug)]
pub struct PageCache {
    next_id: u64,
    snapshots: VecDeque<Snapshot>,
}

impl Default for PageCache {
    fn default() -> Self {
        Self {
            next_id: 1,
            snapshots: VecDeque::new(),
        }
    }
}

impl PageCache {
    pub fn page(
        &mut self,
        method: &str,
        fingerprint: &str,
        limit: Option<usize>,
        cursor: Option<&str>,
        envelope: Option<Value>,
    ) -> Result<Value, PaginationError> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_ITEMS);
        if !(1..=MAX_PAGE_ITEMS).contains(&limit) {
            return Err(PaginationError::InvalidLimit);
        }
        let (snapshot_id, offset) = if let Some(cursor) = cursor {
            parse_cursor(cursor)?
        } else {
            let mut template = envelope.ok_or(PaginationError::InvalidEnvelope)?;
            let object = template
                .as_object_mut()
                .ok_or(PaginationError::InvalidEnvelope)?;
            let items = object
                .remove("items")
                .and_then(|items| items.as_array().cloned())
                .ok_or(PaginationError::InvalidEnvelope)?;
            object.insert("items".to_owned(), Value::Array(Vec::new()));
            object.insert("nextCursor".to_owned(), Value::Null);
            let id = self.next_id;
            self.next_id = self.next_id.checked_add(1).unwrap_or(1);
            if self.snapshots.len() == MAX_SNAPSHOTS {
                self.snapshots.pop_front();
            }
            self.snapshots.push_back(Snapshot {
                id,
                method: method.to_owned(),
                fingerprint: fingerprint.to_owned(),
                template,
                items,
            });
            (id, 0)
        };
        let snapshot = self
            .snapshots
            .iter()
            .find(|snapshot| snapshot.id == snapshot_id)
            .ok_or(PaginationError::CursorExpired)?;
        if snapshot.method != method || snapshot.fingerprint != fingerprint {
            return Err(PaginationError::CursorMismatch);
        }
        if offset > snapshot.items.len() {
            return Err(PaginationError::InvalidCursor);
        }

        let mut page_items = Vec::new();
        let mut next_offset = offset;
        while next_offset < snapshot.items.len() && page_items.len() < limit {
            page_items.push(snapshot.items[next_offset].clone());
            let candidate = render(
                &snapshot.template,
                &page_items,
                Some(page_cursor(snapshot.id, next_offset + 1)),
            )?;
            if serde_json::to_vec(&candidate)
                .map_err(|_| PaginationError::InvalidEnvelope)?
                .len()
                > PAGE_RESULT_CAP_BYTES
            {
                page_items.pop();
                if page_items.is_empty() {
                    return Err(PaginationError::ItemTooLarge);
                }
                break;
            }
            next_offset += 1;
        }
        let next_cursor =
            (next_offset < snapshot.items.len()).then(|| page_cursor(snapshot.id, next_offset));
        render(&snapshot.template, &page_items, next_cursor)
    }
}

fn render(
    template: &Value,
    items: &[Value],
    next_cursor: Option<String>,
) -> Result<Value, PaginationError> {
    let mut result = template.clone();
    let object = result
        .as_object_mut()
        .ok_or(PaginationError::InvalidEnvelope)?;
    object.insert("items".to_owned(), Value::Array(items.to_vec()));
    object.insert(
        "nextCursor".to_owned(),
        next_cursor.map_or(Value::Null, Value::String),
    );
    Ok(result)
}

fn page_cursor(snapshot: u64, offset: usize) -> String {
    format!("page-v1:{snapshot:020}:{offset:020}")
}

fn parse_cursor(cursor: &str) -> Result<(u64, usize), PaginationError> {
    let mut parts = cursor.split(':');
    let version = parts.next();
    let snapshot = parts.next().and_then(|value| value.parse().ok());
    let offset = parts.next().and_then(|value| value.parse().ok());
    if version != Some("page-v1")
        || snapshot.is_none()
        || offset.is_none()
        || parts.next().is_some()
    {
        return Err(PaginationError::InvalidCursor);
    }
    Ok((snapshot.unwrap(), offset.unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acceptance_24_2_oversized_jobs_log_and_trace_page_exactly_once() {
        for (method, item_kind) in [
            ("query.jobs", "job"),
            ("query.log", "lifecycle"),
            ("query.trace", "provider-record"),
        ] {
            let items = (0..4_257)
                .map(|index| {
                    serde_json::json!({
                        "index": index,
                        "kind": item_kind,
                        "identity": format!("{item_kind}-{index:05}"),
                        "raw": "x".repeat(48),
                    })
                })
                .collect::<Vec<_>>();
            assert!(serde_json::to_vec(&items).unwrap().len() > FRAME_CAP_BYTES);
            let envelope = serde_json::json!({
                "schemaVersion": 1,
                "protocolVersion": 3,
                "items": items,
                "nextCursor": null,
                "snapshot": {"cursor": "fixture"},
            });
            let mut cache = PageCache::default();
            let mut cursor = None;
            let mut observed = Vec::new();
            loop {
                let page = cache
                    .page(
                        method,
                        "{}",
                        Some(1_000),
                        cursor.as_deref(),
                        cursor.is_none().then(|| envelope.clone()),
                    )
                    .unwrap();
                assert!(serde_json::to_vec(&page).unwrap().len() < FRAME_CAP_BYTES);
                observed.extend(
                    page["items"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|item| item["index"].as_u64().unwrap()),
                );
                cursor = page["nextCursor"].as_str().map(ToOwned::to_owned);
                if cursor.is_none() {
                    break;
                }
            }
            assert_eq!(observed, (0..4_257).collect::<Vec<_>>(), "{method}");
        }
    }

    #[test]
    fn cursors_are_snapshot_bound_and_expire_explicitly() {
        let envelope = |value| {
            serde_json::json!({
                "items": [{"index": value}, {"index": value + 1}],
                "nextCursor": null,
            })
        };
        let mut cache = PageCache::default();
        let first = cache
            .page("query.jobs", "filter-a", Some(1), None, Some(envelope(10)))
            .unwrap();
        let cursor = first["nextCursor"].as_str().unwrap().to_owned();
        assert_eq!(first["items"][0]["index"], 10);
        assert_eq!(
            cache
                .page("query.jobs", "filter-b", Some(1), Some(&cursor), None)
                .unwrap_err(),
            PaginationError::CursorMismatch
        );
        for index in 0..MAX_SNAPSHOTS {
            cache
                .page(
                    "query.log",
                    &format!("snapshot-{index}"),
                    Some(2),
                    None,
                    Some(envelope(index)),
                )
                .unwrap();
        }
        assert_eq!(
            cache
                .page("query.jobs", "filter-a", Some(1), Some(&cursor), None)
                .unwrap_err(),
            PaginationError::CursorExpired
        );
    }
}
