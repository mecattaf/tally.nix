//! Derived verified indexes over the canonical witness ledger.
//!
//! This cache may be dropped and rebuilt from `witness.jsonl`; it never writes
//! ledger bytes or originates a fact.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;

use crate::witness::{verify_suffix_bytes, ChainHead, VerifyReport, WitnessError, WitnessRecord};

/// The daemon's verified in-memory view of the witness ledger.
///
/// Built from one full verification pass; afterwards every read verifies only
/// the bytes appended past the cached verified prefix, so queries and dedup
/// probes are O(new records) instead of O(ledger). Per-task and per-dedup-key
/// indexes answer the governing-record lookups that previously scanned the
/// whole ledger linearly. Prefix tampering after the initial pass is caught
/// at startup, view rebuilds, and explicit `tally witness verify` runs —
/// suffix tampering is caught here on the next read.
pub(crate) struct WitnessView {
    path: PathBuf,
    records: Arc<Vec<WitnessRecord>>,
    head: ChainHead,
    verified_offset: u64,
    by_task: HashMap<String, Vec<usize>>,
    by_dedup: HashMap<String, Vec<usize>>,
}

fn serialized_line_len(record: &WitnessRecord) -> u64 {
    // Records parse from canonical compact lines. Re-serialization may order
    // extension fields differently, but a JSON object's serialized length is
    // invariant under key order, so the line length (plus the LF) is exact.
    serde_json::to_vec(record).map_or(0, |encoded| encoded.len() as u64 + 1)
}

impl WitnessView {
    pub(crate) fn open(path: PathBuf) -> Result<Self, WitnessError> {
        let (report, records) = crate::witness::read_verified_records(&path)?;
        if !report.ok {
            return Err(WitnessError::Corrupt(
                serde_json::to_string(&report.problems)
                    .unwrap_or_else(|_| "verification failed".to_owned()),
            ));
        }
        Ok(Self::from_records(path, records))
    }

    /// Build the view from records an earlier full verification pass in this
    /// process already proved, without re-reading the ledger.
    pub(crate) fn from_records(path: PathBuf, records: Vec<WitnessRecord>) -> Self {
        let verified_offset = records.iter().map(serialized_line_len).sum();
        let head = records
            .last()
            .map_or_else(ChainHead::default, |record| ChainHead {
                seq: record.seq,
                hash: record.hash.clone(),
            });
        let mut view = Self {
            path,
            records: Arc::new(Vec::new()),
            head,
            verified_offset,
            by_task: HashMap::new(),
            by_dedup: HashMap::new(),
        };
        for (index, record) in records.iter().enumerate() {
            view.index_record(record, index);
        }
        view.records = Arc::new(records);
        view
    }

    fn index_record(&mut self, record: &WitnessRecord, index: usize) {
        if let Some(task_uuid) = &record.task_uuid {
            self.by_task
                .entry(task_uuid.clone())
                .or_default()
                .push(index);
        }
        if let Some(dedup_key) = &record.dedup_key {
            self.by_dedup
                .entry(dedup_key.clone())
                .or_default()
                .push(index);
        }
    }

    /// Absorb any bytes appended past the verified prefix. A shrunken or
    /// rewritten ledger triggers one full re-verification; a broken suffix
    /// fails closed.
    pub(crate) fn refresh(&mut self) -> Result<(), WitnessError> {
        let length = match std::fs::metadata(&self.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(source) => {
                return Err(WitnessError::Io {
                    path: self.path.clone(),
                    source,
                })
            }
        };
        if length == self.verified_offset {
            return Ok(());
        }
        if length < self.verified_offset {
            *self = Self::open(self.path.clone())?;
            return Ok(());
        }
        let mut file = File::open(&self.path).map_err(|source| WitnessError::Io {
            path: self.path.clone(),
            source,
        })?;
        file.seek(SeekFrom::Start(self.verified_offset))
            .map_err(|source| WitnessError::Io {
                path: self.path.clone(),
                source,
            })?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|source| WitnessError::Io {
                path: self.path.clone(),
                source,
            })?;
        // Only complete LF-terminated lines are verifiable; a torn tail from
        // an in-flight append is left for the writer to finish or repair.
        let complete_len = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |position| position + 1);
        bytes.truncate(complete_len);
        if bytes.is_empty() {
            return Ok(());
        }
        let appended = verify_suffix_bytes(&bytes, &self.head)?;
        if let Some(last) = appended.last() {
            self.head = ChainHead {
                seq: last.seq,
                hash: last.hash.clone(),
            };
        }
        let records = Arc::make_mut(&mut self.records);
        for record in appended {
            let index = records.len();
            if let Some(task_uuid) = &record.task_uuid {
                self.by_task
                    .entry(task_uuid.clone())
                    .or_default()
                    .push(index);
            }
            if let Some(dedup_key) = &record.dedup_key {
                self.by_dedup
                    .entry(dedup_key.clone())
                    .or_default()
                    .push(index);
            }
            records.push(record);
        }
        self.verified_offset += complete_len as u64;
        Ok(())
    }

    /// A cheap shared snapshot of the verified records.
    pub(crate) fn records(&mut self) -> Result<Arc<Vec<WitnessRecord>>, WitnessError> {
        self.refresh()?;
        Ok(Arc::clone(&self.records))
    }

    /// The report a full verification of the cached prefix would produce.
    pub(crate) fn report(&self) -> VerifyReport {
        VerifyReport {
            ok: true,
            records: self.records.len(),
            first_seq: self.records.first().map(|record| record.seq),
            last_seq: self.records.last().map(|record| record.seq),
            problems: Vec::new(),
        }
    }

    pub(crate) fn head_seq(&self) -> u64 {
        self.head.seq
    }

    /// The governing (maximum-seq) record for a task, optionally pinned to an
    /// attempt. Indexes are seq-ordered, so the newest match is the last one.
    pub(crate) fn latest_for_task(
        &mut self,
        task_uuid: &str,
        attempt: Option<u32>,
    ) -> Result<Option<WitnessRecord>, WitnessError> {
        self.refresh()?;
        Ok(self.by_task.get(task_uuid).and_then(|indexes| {
            indexes
                .iter()
                .rev()
                .map(|index| &self.records[*index])
                .find(|record| attempt.is_none_or(|attempt| record.attempt == attempt))
                .cloned()
        }))
    }

    /// The governing (maximum-seq) record for a dedup key: exactly the record
    /// a linear `max_by_key(seq)` scan would choose.
    pub(crate) fn governing_for_dedup(
        &mut self,
        dedup_key: &str,
    ) -> Result<Option<WitnessRecord>, WitnessError> {
        self.refresh()?;
        Ok(self
            .by_dedup
            .get(dedup_key)
            .and_then(|indexes| indexes.last())
            .map(|index| self.records[*index].clone()))
    }
}

#[cfg(test)]
mod tests {
    use proptest::collection::vec;
    use proptest::prelude::*;

    use super::*;
    use crate::taskdb::{AdmissionOrigin, EnqueueSource};
    use crate::witness::{LaborClass, Verdict, WitnessBody, WitnessLedger};

    fn body(task: u8, dedup: u8, verdict: Verdict) -> WitnessBody {
        WitnessBody {
            task_uuid: Some(format!("b2c40001-0000-4000-8000-0000000000{task:02x}")),
            transition_timestamp: "2026-07-28T10:00:01.100Z".to_owned(),
            verdict,
            exit_code: match verdict {
                Verdict::Pass | Verdict::Reused | Verdict::Substituted => 0,
                _ => 1,
            },
            artifact_content_hash: Some(format!("sha256:{}", "a".repeat(64))),
            store_paths: None,
            drv: None,
            gpu_seconds: None,
            wall_clock: 1.0,
            attempt: 1,
            lease_epoch: 1,
            dedup_key: Some(format!("key:{dedup}")),
            payload_hash: None,
            brief_hash: None,
            origin: AdmissionOrigin::direct(EnqueueSource::Manual),
            orchestration: None,
            labor_class: LaborClass::Fresh,
            trace_ref: None,
            pools: vec!["slot".to_owned()],
            executor: None,
            host_id: None,
            charge: None,
            model: None,
            evidence_class: None,
            manifest_hash: None,
            completion: None,
            error: None,
            result_revision: None,
            authorship: None,
            authorship_sessions: None,
        }
    }

    proptest! {
        // The index must preserve exactly the governing-record choice the
        // linear max_by_key(seq) scans made before the view existed.
        #[test]
        fn indexed_governing_choice_matches_linear_scan(
            entries in vec((0_u8..6, 0_u8..6, prop_oneof![
                Just(Verdict::Pass),
                Just(Verdict::Failed),
                Just(Verdict::Cancelled),
            ]), 1..40),
        ) {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("witness.jsonl");
            {
                let mut ledger = WitnessLedger::open(&path).unwrap();
                for (task, dedup, verdict) in &entries {
                    ledger.append(body(*task, *dedup, *verdict)).unwrap();
                }
            }
            let mut view = WitnessView::open(path.clone()).unwrap();
            let records = view.records().unwrap();
            for key in 0_u8..6 {
                let dedup_key = format!("key:{key}");
                let linear = records
                    .iter()
                    .filter(|record| record.dedup_key.as_deref() == Some(dedup_key.as_str()))
                    .max_by_key(|record| record.seq)
                    .cloned();
                prop_assert_eq!(view.governing_for_dedup(&dedup_key).unwrap(), linear);

                let task_uuid = format!("b2c40001-0000-4000-8000-0000000000{key:02x}");
                let linear_task = records
                    .iter()
                    .filter(|record| record.task_uuid.as_deref() == Some(task_uuid.as_str()))
                    .max_by_key(|record| record.seq)
                    .cloned();
                prop_assert_eq!(view.latest_for_task(&task_uuid, None).unwrap(), linear_task);
            }
        }
    }

    #[test]
    fn refresh_absorbs_appends_and_rejects_suffix_tamper() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("witness.jsonl");
        let mut ledger = WitnessLedger::open(&path).unwrap();
        ledger.append(body(1, 1, Verdict::Pass)).unwrap();

        let mut view = WitnessView::open(path.clone()).unwrap();
        assert_eq!(view.records().unwrap().len(), 1);

        ledger.append(body(2, 1, Verdict::Failed)).unwrap();
        assert_eq!(view.records().unwrap().len(), 2);
        assert_eq!(view.head_seq(), 2);
        assert_eq!(
            view.governing_for_dedup("key:1").unwrap().unwrap().verdict,
            Verdict::Failed
        );

        // Verify only new bytes: seed the chain builder directly to confirm a
        // tampered suffix is refused on refresh.
        ledger.append(body(3, 2, Verdict::Pass)).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let target = bytes.len() - 40;
        bytes[target] ^= 0x01;
        std::fs::write(&path, &bytes).unwrap();
        assert!(matches!(view.records(), Err(WitnessError::Corrupt(_))));
    }

    #[test]
    fn from_records_offset_matches_the_durable_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("witness.jsonl");
        let mut ledger = WitnessLedger::open(&path).unwrap();
        for index in 0..4 {
            ledger.append(body(index, index, Verdict::Pass)).unwrap();
        }
        let (report, records) = crate::witness::read_verified_records(&path).unwrap();
        assert!(report.ok);
        let mut view = WitnessView::from_records(path.clone(), records);
        assert_eq!(
            view.verified_offset,
            std::fs::metadata(&path).unwrap().len()
        );
        // A later append is absorbed as a suffix, proving the computed offset
        // aligned exactly with the durable prefix.
        ledger.append(body(9, 9, Verdict::Pass)).unwrap();
        assert_eq!(view.records().unwrap().len(), 5);
    }
}
