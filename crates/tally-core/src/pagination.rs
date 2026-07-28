use std::collections::VecDeque;

use serde_json::Value;
use thiserror::Error;

pub const DEFAULT_PAGE_ITEMS: usize = 100;
pub const MAX_PAGE_ITEMS: usize = 1_000;
const PAGE_RESULT_CAP_BYTES: usize = 48 * 1024;
const MAX_SNAPSHOTS: usize = 32;
// Total approximate bytes of retained snapshots. The count cap alone lets 32
// arbitrarily large result sets pin unbounded memory; the byte budget evicts
// the oldest snapshots first once the cache would exceed it.
const SNAPSHOT_BUDGET_BYTES: usize = 64 * 1024 * 1024;

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
    // Approximate retained size: serialized template plus serialized items.
    bytes: usize,
}

#[derive(Debug)]
pub struct PageCache {
    next_id: u64,
    snapshots: VecDeque<Snapshot>,
    budget_bytes: usize,
    used_bytes: usize,
}

impl Default for PageCache {
    fn default() -> Self {
        Self {
            next_id: 1,
            snapshots: VecDeque::new(),
            budget_bytes: SNAPSHOT_BUDGET_BYTES,
            used_bytes: 0,
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
            let bytes = approximate_size(&template)?.saturating_add(
                items
                    .iter()
                    .map(approximate_size)
                    .try_fold(0_usize, |total, size| {
                        size.map(|size| total.saturating_add(size))
                    })?,
            );
            while !self.snapshots.is_empty()
                && (self.snapshots.len() == MAX_SNAPSHOTS
                    || self.used_bytes.saturating_add(bytes) > self.budget_bytes)
            {
                let evicted = self
                    .snapshots
                    .pop_front()
                    .expect("nonempty snapshot deque pops");
                self.used_bytes = self.used_bytes.saturating_sub(evicted.bytes);
            }
            self.used_bytes = self.used_bytes.saturating_add(bytes);
            self.snapshots.push_back(Snapshot {
                id,
                method: method.to_owned(),
                fingerprint: fingerprint.to_owned(),
                template,
                items,
                bytes,
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

        // Running byte accounting reproduces the serialized candidate size
        // exactly: page cursors are fixed-width, so the rendered envelope with
        // N items is the empty render plus each item's serialized bytes plus
        // one comma separator per item after the first. This keeps page
        // boundaries byte-identical to full re-serialization while assembling
        // each page in O(page) instead of O(page^2) bytes.
        let empty_render = render(
            &snapshot.template,
            &[],
            Some(page_cursor(snapshot.id, offset)),
        )?;
        let mut used = serde_json::to_vec(&empty_render)
            .map_err(|_| PaginationError::InvalidEnvelope)?
            .len();
        let mut page_items = Vec::new();
        let mut next_offset = offset;
        while next_offset < snapshot.items.len() && page_items.len() < limit {
            let item_bytes = approximate_size(&snapshot.items[next_offset])?;
            let candidate = used
                .saturating_add(item_bytes)
                .saturating_add(usize::from(!page_items.is_empty()));
            if candidate > PAGE_RESULT_CAP_BYTES {
                if page_items.is_empty() {
                    return Err(PaginationError::ItemTooLarge);
                }
                break;
            }
            page_items.push(snapshot.items[next_offset].clone());
            used = candidate;
            next_offset += 1;
        }
        let next_cursor =
            (next_offset < snapshot.items.len()).then(|| page_cursor(snapshot.id, next_offset));
        render(&snapshot.template, &page_items, next_cursor)
    }
}

fn approximate_size(value: &Value) -> Result<usize, PaginationError> {
    serde_json::to_vec(value)
        .map(|encoded| encoded.len())
        .map_err(|_| PaginationError::InvalidEnvelope)
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
            assert!(serde_json::to_vec(&items).unwrap().len() > PAGE_RESULT_CAP_BYTES);
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
                assert!(serde_json::to_vec(&page).unwrap().len() <= PAGE_RESULT_CAP_BYTES);
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

    // Characterization: cursor and page-boundary behavior that any cache
    // eviction or byte-budget change must preserve exactly.
    #[test]
    fn characterization_cursor_stays_stable_while_snapshot_is_retained() {
        let mut cache = PageCache::default();
        let envelope = serde_json::json!({
            "snapshot": {"cursor": "fixture"},
            "items": (0..10).map(|index| serde_json::json!({"index": index})).collect::<Vec<_>>(),
            "nextCursor": null,
        });
        let first = cache
            .page("query.jobs", "stable", Some(4), None, Some(envelope))
            .unwrap();
        let cursor = first["nextCursor"].as_str().unwrap().to_owned();
        let reference = cache
            .page("query.jobs", "stable", Some(4), Some(&cursor), None)
            .unwrap();

        // Churn newer snapshots up to one below the retention cap: the cursor
        // must keep serving byte-identical pages.
        for index in 0..(MAX_SNAPSHOTS - 2) {
            cache
                .page(
                    "query.log",
                    &format!("churn-{index}"),
                    Some(2),
                    None,
                    Some(serde_json::json!({"items": [{"churn": index}], "nextCursor": null})),
                )
                .unwrap();
        }
        let replayed = cache
            .page("query.jobs", "stable", Some(4), Some(&cursor), None)
            .unwrap();
        assert_eq!(replayed, reference);

        // Continuation from the replayed page walks to the exact end.
        let last_cursor = replayed["nextCursor"].as_str().unwrap().to_owned();
        let last = cache
            .page("query.jobs", "stable", Some(4), Some(&last_cursor), None)
            .unwrap();
        assert_eq!(
            last["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["index"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![8, 9]
        );
        assert!(last["nextCursor"].is_null());
    }

    #[test]
    fn characterization_page_boundaries_are_deterministic_under_the_byte_cap() {
        // Items sized so the 48KiB response cap, not the item limit, decides
        // the boundary. A byte-accounting refactor must not move it.
        let items = (0..64)
            .map(|index| {
                serde_json::json!({
                    "index": index,
                    "padding": "x".repeat(4_096),
                })
            })
            .collect::<Vec<_>>();
        let envelope = serde_json::json!({"items": items, "nextCursor": null});
        let walk = || {
            let mut boundaries = Vec::new();
            let mut cache = PageCache::default();
            let mut cursor: Option<String> = None;
            loop {
                let page = cache
                    .page(
                        "query.trace",
                        "cap",
                        Some(1_000),
                        cursor.as_deref(),
                        cursor.is_none().then(|| envelope.clone()),
                    )
                    .unwrap();
                assert!(serde_json::to_vec(&page).unwrap().len() <= PAGE_RESULT_CAP_BYTES);
                let page_items = page["items"].as_array().unwrap();
                assert!(!page_items.is_empty());
                boundaries.push((
                    page_items.first().unwrap()["index"].as_u64().unwrap(),
                    page_items.len(),
                ));
                cursor = page["nextCursor"].as_str().map(ToOwned::to_owned);
                if cursor.is_none() {
                    break;
                }
            }
            boundaries
        };
        let boundaries = walk();
        let total: usize = boundaries.iter().map(|(_, len)| len).sum();
        assert_eq!(total, 64);
        // Boundary starts must be the running sum of page lengths: no overlap
        // and no gap.
        let mut expected_start = 0;
        for (start, len) in &boundaries {
            assert_eq!(*start, expected_start);
            expected_start += *len as u64;
        }
        assert!(
            boundaries.len() >= 4,
            "byte cap did not shape pages: {boundaries:?}"
        );
        // Walking the identical snapshot again must reproduce the identical
        // boundaries: page assembly is deterministic, not incidental.
        assert_eq!(walk(), boundaries);
    }

    #[test]
    fn byte_budget_evicts_oldest_snapshots_before_the_count_cap() {
        let mut cache = PageCache {
            budget_bytes: 8 * 1024,
            ..PageCache::default()
        };
        let envelope = |tag: usize| {
            serde_json::json!({
                "items": [{"tag": tag, "padding": "x".repeat(3_000)}],
                "nextCursor": null,
            })
        };
        let first = cache
            .page("query.jobs", "budget-0", Some(1), None, Some(envelope(0)))
            .unwrap();
        assert_eq!(first["items"][0]["tag"], 0);
        let first_cursor = page_cursor(1, 0);
        // Two more ~3KiB snapshots blow the 8KiB budget: the oldest goes even
        // though only 3 of 32 count slots are used.
        for tag in 1..3 {
            cache
                .page(
                    "query.jobs",
                    &format!("budget-{tag}"),
                    Some(1),
                    None,
                    Some(envelope(tag)),
                )
                .unwrap();
        }
        assert_eq!(
            cache
                .page("query.jobs", "budget-0", Some(1), Some(&first_cursor), None)
                .unwrap_err(),
            PaginationError::CursorExpired
        );
        assert!(cache.snapshots.len() < 3);
        assert!(cache.used_bytes <= cache.budget_bytes);
    }

    #[test]
    fn running_byte_accounting_matches_full_serialization_boundaries() {
        // Differential check: every page must be maximal under the byte cap
        // exactly as full re-serialization would compute it — the page fits,
        // and appending the next item (with a continuation cursor) would not.
        let items = (0..48)
            .map(|index| {
                serde_json::json!({
                    "index": index,
                    "padding": "y".repeat(3_000 + (index % 7) * 411),
                })
            })
            .collect::<Vec<_>>();
        let envelope = serde_json::json!({
            "schemaVersion": 1,
            "snapshot": {"cursor": "differential"},
            "items": items.clone(),
            "nextCursor": null,
        });
        let mut cache = PageCache::default();
        let mut cursor: Option<String> = None;
        let mut consumed = 0_usize;
        loop {
            let page = cache
                .page(
                    "query.log",
                    "diff",
                    Some(1_000),
                    cursor.as_deref(),
                    cursor.is_none().then(|| envelope.clone()),
                )
                .unwrap();
            let rendered = serde_json::to_vec(&page).unwrap();
            assert!(rendered.len() <= PAGE_RESULT_CAP_BYTES);
            let page_len = page["items"].as_array().unwrap().len();
            assert!(page_len > 0);
            consumed += page_len;
            if consumed < items.len() {
                // Re-render this page with one more item and a cursor, the way
                // the pre-optimization code sized candidates: it must overflow.
                let mut widened = page["items"].as_array().unwrap().clone();
                widened.push(items[consumed].clone());
                let mut candidate = page.clone();
                candidate["items"] = Value::Array(widened);
                candidate["nextCursor"] = Value::String(page_cursor(1, consumed + 1));
                assert!(
                    serde_json::to_vec(&candidate).unwrap().len() > PAGE_RESULT_CAP_BYTES,
                    "page ending at {consumed} is not maximal"
                );
            }
            cursor = page["nextCursor"].as_str().map(ToOwned::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(consumed, items.len());
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
