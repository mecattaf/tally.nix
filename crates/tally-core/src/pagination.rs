//! Derived, process-local pagination snapshots.
//!
//! A page cache retains no authority and is rebuilt from a query projection;
//! restart or eviction may expire a cursor without losing a canonical fact.

use std::collections::VecDeque;

use serde_json::Value;
use thiserror::Error;

pub const DEFAULT_PAGE_ITEMS: usize = 100;
pub const MAX_PAGE_ITEMS: usize = 1_000;
const PAGE_RESULT_CAP_BYTES: usize = 48 * 1024;
/// Bytes of an over-large string field retained when the field has to be
/// elided so its item can be served at all. Enough to keep an argv element or
/// a brief-derived field recognisable to a reader.
const ELISION_KEEP_BYTES: usize = 256;
/// Key the elision marker is written under on an elided item.
pub const ELISION_MARKER_KEY: &str = "elided";
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

/// A collection envelope split into its retained-snapshot parts, with the
/// per-item size accounting already paid.
///
/// [`prepare_snapshot`] is deliberately a free function: splitting and sizing
/// an envelope touches every item, which at estate scale is corpus-sized work,
/// and the daemon runs it on the blocking pool next to the query construction
/// that built the envelope (#431). Inserting the prepared snapshot into the
/// cache ([`PageCache::page_prepared`]) is what needs the cache borrow, and
/// that is O(evictions), not O(items).
#[derive(Debug)]
pub struct PreparedSnapshot {
    template: Value,
    items: Vec<Value>,
    bytes: usize,
}

pub fn prepare_snapshot(mut envelope: Value) -> Result<PreparedSnapshot, PaginationError> {
    let object = envelope
        .as_object_mut()
        .ok_or(PaginationError::InvalidEnvelope)?;
    let items = match object.remove("items") {
        Some(Value::Array(items)) => items,
        _ => return Err(PaginationError::InvalidEnvelope),
    };
    object.insert("items".to_owned(), Value::Array(Vec::new()));
    object.insert("nextCursor".to_owned(), Value::Null);
    let bytes = approximate_size(&envelope)?.saturating_add(
        items
            .iter()
            .map(approximate_size)
            .try_fold(0_usize, |total, size| {
                size.map(|size| total.saturating_add(size))
            })?,
    );
    Ok(PreparedSnapshot {
        template: envelope,
        items,
        bytes,
    })
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
        match cursor {
            Some(cursor) => self.page_at(method, fingerprint, limit, Some(cursor), None),
            None => {
                let envelope = envelope.ok_or(PaginationError::InvalidEnvelope)?;
                self.page_prepared(method, fingerprint, limit, prepare_snapshot(envelope)?)
            }
        }
    }

    /// Serve the first page of an envelope whose snapshot split was already
    /// paid by [`prepare_snapshot`].
    pub fn page_prepared(
        &mut self,
        method: &str,
        fingerprint: &str,
        limit: Option<usize>,
        prepared: PreparedSnapshot,
    ) -> Result<Value, PaginationError> {
        self.page_at(method, fingerprint, limit, None, Some(prepared))
    }

    fn page_at(
        &mut self,
        method: &str,
        fingerprint: &str,
        limit: Option<usize>,
        cursor: Option<&str>,
        prepared: Option<PreparedSnapshot>,
    ) -> Result<Value, PaginationError> {
        let limit = limit.unwrap_or(DEFAULT_PAGE_ITEMS);
        if !(1..=MAX_PAGE_ITEMS).contains(&limit) {
            return Err(PaginationError::InvalidLimit);
        }
        let (snapshot_id, offset) = if let Some(cursor) = cursor {
            parse_cursor(cursor)?
        } else {
            let PreparedSnapshot {
                template,
                items,
                bytes,
            } = prepared.ok_or(PaginationError::InvalidEnvelope)?;
            let id = self.next_id;
            self.next_id = self.next_id.checked_add(1).unwrap_or(1);
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
        //
        // `truncated` and `elidedItems` are part of that fixed overhead. Only
        // a page's leading item can ever be elided, so `elidedItems` is 0 or 1
        // and its width never changes; `truncated: false` co-occurs only with
        // a null `nextCursor`, which is 47 bytes shorter than the fixed-width
        // cursor the sizing render carries, so the final render is never
        // larger than the one this accounting priced.
        let empty_render = render(
            &snapshot.template,
            &[],
            Some(page_cursor(snapshot.id, offset)),
            0,
        )?;
        let mut used = serde_json::to_vec(&empty_render)
            .map_err(|_| PaginationError::InvalidEnvelope)?
            .len();
        let mut page_items = Vec::new();
        let mut elided = 0_usize;
        let mut next_offset = offset;
        while next_offset < snapshot.items.len() && page_items.len() < limit {
            let item_bytes = approximate_size(&snapshot.items[next_offset])?;
            let candidate = used
                .saturating_add(item_bytes)
                .saturating_add(usize::from(!page_items.is_empty()));
            if candidate > PAGE_RESULT_CAP_BYTES {
                if page_items.is_empty() {
                    // One monstrous item must not destroy the page: a campaign
                    // runner whose argv embeds an issue body would otherwise
                    // make the whole run unmonitorable. Elide its largest
                    // string fields, mark the elision on the item, and serve
                    // it. Only an item that cannot be shrunk this way is still
                    // a hard error.
                    let room = PAGE_RESULT_CAP_BYTES.saturating_sub(used);
                    let shrunk = elide_to_fit(&snapshot.items[next_offset], room)
                        .ok_or(PaginationError::ItemTooLarge)?;
                    page_items.push(shrunk);
                    elided += 1;
                    next_offset += 1;
                }
                break;
            }
            page_items.push(snapshot.items[next_offset].clone());
            used = candidate;
            next_offset += 1;
        }
        let next_cursor =
            (next_offset < snapshot.items.len()).then(|| page_cursor(snapshot.id, next_offset));
        render(&snapshot.template, &page_items, next_cursor, elided)
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
    elided: usize,
) -> Result<Value, PaginationError> {
    let mut result = template.clone();
    let object = result
        .as_object_mut()
        .ok_or(PaginationError::InvalidEnvelope)?;
    object.insert("items".to_owned(), Value::Array(items.to_vec()));
    // `truncated` is the field a reader can trust without reasoning about
    // cursors: it is true exactly when this response is not the whole window.
    object.insert("truncated".to_owned(), Value::Bool(next_cursor.is_some()));
    object.insert("elidedItems".to_owned(), Value::from(elided));
    object.insert(
        "nextCursor".to_owned(),
        next_cursor.map_or(Value::Null, Value::String),
    );
    Ok(result)
}

/// Shrink `item` until it serializes within `room` bytes by truncating its
/// largest string leaves, largest first, and recording what was cut on the
/// item itself. Returns `None` when no amount of string elision gets there —
/// an item whose bulk is structure rather than text.
fn elide_to_fit(item: &Value, room: usize) -> Option<Value> {
    if !item.is_object() {
        return None;
    }
    let original_bytes = serde_json::to_vec(item).ok()?.len();
    let mut candidates = Vec::new();
    collect_string_leaves(item, &mut String::new(), &mut candidates);
    // Largest first, path order breaking ties so the result is deterministic.
    candidates.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let mut work = item.clone();
    let mut cut = Vec::new();
    for (pointer, length) in candidates {
        if length <= ELISION_KEEP_BYTES {
            break;
        }
        elide_at(&mut work, &pointer)?;
        cut.push(Value::String(pointer));
        let marker = serde_json::json!({
            "fields": Value::Array(cut.clone()),
            "originalBytes": original_bytes,
            "reason": "item exceeded the bounded response size",
        });
        work.as_object_mut()?
            .insert(ELISION_MARKER_KEY.to_owned(), marker);
        if serde_json::to_vec(&work).ok()?.len() <= room {
            return Some(work);
        }
    }
    None
}

fn collect_string_leaves(value: &Value, pointer: &mut String, out: &mut Vec<(String, usize)>) {
    match value {
        Value::String(text) => out.push((pointer.clone(), text.len())),
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                let mark = pointer.len();
                pointer.push('/');
                pointer.push_str(&index.to_string());
                collect_string_leaves(item, pointer, out);
                pointer.truncate(mark);
            }
        }
        Value::Object(fields) => {
            for (key, field) in fields {
                if key == ELISION_MARKER_KEY {
                    continue;
                }
                let mark = pointer.len();
                pointer.push('/');
                pointer.push_str(&key.replace('~', "~0").replace('/', "~1"));
                collect_string_leaves(field, pointer, out);
                pointer.truncate(mark);
            }
        }
        _ => {}
    }
}

fn elide_at(value: &mut Value, pointer: &str) -> Option<()> {
    let target = value.pointer_mut(pointer)?;
    let text = target.as_str()?;
    let mut keep = ELISION_KEEP_BYTES.min(text.len());
    while keep > 0 && !text.is_char_boundary(keep) {
        keep -= 1;
    }
    let dropped = text.len() - keep;
    *target = Value::String(format!("{}…<{dropped} bytes elided>", &text[..keep]));
    Some(())
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

    /// #316: a campaign runner row whose argv embeds an issue body used to
    /// make `query jobs --flow-run` fail outright with `ItemTooLarge`, which
    /// made the whole run unmonitorable. The oversized field is elided and
    /// marked; the page is served.
    #[test]
    fn an_oversized_item_is_elided_and_marked_instead_of_destroying_the_page() {
        let monstrous = serde_json::json!({
            "anchor": "00000000-0000-4000-8000-000000000001",
            "argv": ["tally", "campaign", "run", "x".repeat(200 * 1024)],
            "liveState": "running",
        });
        let envelope = serde_json::json!({
            "schemaVersion": 1,
            "items": [monstrous, serde_json::json!({"anchor": "second"})],
            "nextCursor": null,
        });
        let mut cache = PageCache::default();
        let page = cache
            .page("query.jobs", "oversized", Some(100), None, Some(envelope))
            .expect("an oversized item must not fail the query");
        assert!(serde_json::to_vec(&page).unwrap().len() <= PAGE_RESULT_CAP_BYTES);
        assert_eq!(page["elidedItems"], 1);
        assert_eq!(page["truncated"], true);
        let item = &page["items"][0];
        assert_eq!(item["anchor"], "00000000-0000-4000-8000-000000000001");
        assert_eq!(item["liveState"], "running", "unrelated fields survive");
        assert_eq!(
            item[ELISION_MARKER_KEY]["fields"],
            serde_json::json!(["/argv/3"])
        );
        assert!(item[ELISION_MARKER_KEY]["originalBytes"].as_u64().unwrap() > 200 * 1024);
        let elided = item["argv"][3].as_str().unwrap();
        assert!(elided.starts_with(&"x".repeat(ELISION_KEEP_BYTES)));
        assert!(elided.ends_with("bytes elided>"));

        // The rest of the window still pages normally behind it.
        let cursor = page["nextCursor"].as_str().unwrap().to_owned();
        let rest = cache
            .page("query.jobs", "oversized", Some(100), Some(&cursor), None)
            .unwrap();
        assert_eq!(rest["items"][0]["anchor"], "second");
        assert_eq!(rest["elidedItems"], 0);
        assert_eq!(rest["truncated"], false);
    }

    /// Elision shrinks text, not structure. An item that is huge because of
    /// its shape cannot be rescued, and that stays a hard error rather than a
    /// silently short page.
    #[test]
    fn an_item_too_large_to_elide_is_still_a_hard_error() {
        let structural = Value::Array(
            (0..20_000)
                .map(|index| serde_json::json!({"n": index}))
                .collect(),
        );
        let envelope = serde_json::json!({
            "items": [serde_json::json!({"rows": structural})],
            "nextCursor": null,
        });
        let mut cache = PageCache::default();
        assert_eq!(
            cache
                .page("query.jobs", "structural", Some(100), None, Some(envelope))
                .unwrap_err(),
            PaginationError::ItemTooLarge
        );
    }

    /// `truncated` is the field a monitor can read without reasoning about
    /// cursors: the #247 report was a reader who could not tell a capped page
    /// from a quiet run.
    #[test]
    fn truncated_marks_every_incomplete_page_and_only_those() {
        let envelope = serde_json::json!({
            "items": (0..5).map(|index| serde_json::json!({"index": index})).collect::<Vec<_>>(),
            "nextCursor": null,
        });
        let mut cache = PageCache::default();
        let first = cache
            .page("query.log", "flags", Some(2), None, Some(envelope))
            .unwrap();
        assert_eq!(first["truncated"], true);
        assert_eq!(first["elidedItems"], 0);
        let mut cursor = first["nextCursor"].as_str().unwrap().to_owned();
        loop {
            let page = cache
                .page("query.log", "flags", Some(2), Some(&cursor), None)
                .unwrap();
            match page["nextCursor"].as_str() {
                Some(next) => {
                    assert_eq!(page["truncated"], true);
                    cursor = next.to_owned();
                }
                None => {
                    assert_eq!(page["truncated"], false);
                    break;
                }
            }
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
