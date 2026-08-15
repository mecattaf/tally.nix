//! Shared authority and epoch-stamp contract for campaign attempt receipts.
//!
//! The append-only log keeps its historical `attempt-receipts-v1.jsonl` file
//! name. Individual records are versioned: schema 1 is the unstamped epsilon
//! history, while every newly written schema-2 record carries the authority
//! fields defined here.

use chrono::DateTime;
use serde::{Deserialize, Serialize};

pub const LEGACY_ATTEMPT_RECEIPT_SCHEMA_VERSION: u64 = 1;
pub const ATTEMPT_RECEIPT_SCHEMA_VERSION: u64 = 2;
pub const ATTEMPT_RECEIPT_AUTHORITY_SCHEMA_VERSION: u32 = 1;
pub const ATTEMPT_RECEIPT_AUTHORITY_FILE: &str = "receipt-authority-v1.json";
pub const ATTEMPT_RECEIPT_MACHINE_ACTOR: &str = "spec-build-driver";
/// Un-authored safety latch across every input epoch for one stable task ID.
pub const MAX_TASK_LIFETIME_ATTEMPTS: usize = 10;

/// The arm authority published beside one campaign's attempt-receipts log.
///
/// Writers read this file at append time. It deliberately contains no
/// timestamp: `writtenAt` is the time of the individual append, not the time
/// the arm was admitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AttemptReceiptAuthorityV1 {
    pub schema_version: u32,
    pub campaign: String,
    pub issue_number: String,
    pub arm_serial: u64,
    pub worklist_sha256: String,
}

impl AttemptReceiptAuthorityV1 {
    pub fn new(
        campaign: impl Into<String>,
        issue_number: impl Into<String>,
        arm_serial: u64,
        worklist_sha256: impl Into<String>,
    ) -> Result<Self, String> {
        let authority = Self {
            schema_version: ATTEMPT_RECEIPT_AUTHORITY_SCHEMA_VERSION,
            campaign: campaign.into(),
            issue_number: issue_number.into(),
            arm_serial,
            worklist_sha256: worklist_sha256.into(),
        };
        authority.validate_for(&authority.campaign, &authority.issue_number)?;
        Ok(authority)
    }

    pub fn validate_for(&self, campaign: &str, issue_number: &str) -> Result<(), String> {
        if self.schema_version != ATTEMPT_RECEIPT_AUTHORITY_SCHEMA_VERSION {
            return Err(format!(
                "schemaVersion must equal {ATTEMPT_RECEIPT_AUTHORITY_SCHEMA_VERSION}"
            ));
        }
        if self.campaign != campaign || self.issue_number != issue_number {
            return Err("campaign or issueNumber does not match the receipt log".to_owned());
        }
        if self.arm_serial == 0 {
            return Err("armSerial must be a positive integer".to_owned());
        }
        if !is_sha256_identity(&self.worklist_sha256) {
            return Err("worklistSha256 must be a lowercase SHA-256 identity".to_owned());
        }
        Ok(())
    }
}

#[must_use]
pub fn is_sha256_identity(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Validate the common fields required on a schema-2 receipt.
pub fn validate_attempt_receipt_stamp(
    arm_serial: Option<u64>,
    worklist_sha256: Option<&str>,
    written_at: Option<&str>,
    actor: Option<&str>,
) -> Result<(), String> {
    if arm_serial.is_none_or(|value| value == 0) {
        return Err("armSerial must be a positive integer".to_owned());
    }
    if worklist_sha256.is_none_or(|value| !is_sha256_identity(value)) {
        return Err("worklistSha256 must be a lowercase SHA-256 identity".to_owned());
    }
    let written_at =
        written_at.ok_or_else(|| "writtenAt must be an RFC3339 timestamp".to_owned())?;
    DateTime::parse_from_rfc3339(written_at)
        .map_err(|_| "writtenAt must be an RFC3339 timestamp".to_owned())?;
    let actor = actor.ok_or_else(|| "actor must be non-empty text".to_owned())?;
    if actor.is_empty()
        || actor.chars().count() > 128
        || actor.chars().any(|character| character < '\u{20}')
    {
        return Err(
            "actor must be non-empty text of at most 128 characters without controls".to_owned(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_authority_and_stamp_validate_the_epoch_contract() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let authority = AttemptReceiptAuthorityV1::new("epsilon", "1", 3, digest.clone()).unwrap();
        authority.validate_for("epsilon", "1").unwrap();
        validate_attempt_receipt_stamp(
            Some(authority.arm_serial),
            Some(&authority.worklist_sha256),
            Some("2026-08-15T10:11:12.345Z"),
            Some(ATTEMPT_RECEIPT_MACHINE_ACTOR),
        )
        .unwrap();

        assert!(authority.validate_for("other", "1").is_err());
        assert!(validate_attempt_receipt_stamp(
            Some(0),
            Some(&digest),
            Some("2026-08-15T10:11:12Z"),
            Some("actor"),
        )
        .is_err());
        assert!(validate_attempt_receipt_stamp(
            Some(1),
            Some(&digest),
            Some("not-a-time"),
            Some("actor"),
        )
        .is_err());
    }
}
