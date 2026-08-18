//! The campaign lease: activation held while work flows, lapsing into
//! durable completion.
//!
//! A registration is an identity, not a lifecycle. What is live about a
//! campaign is a **lease** binding one admitted worklist to one host's
//! resources: acquired at activation, renewed by every pass that finds work
//! flowing, and lapsed exactly once — when the last admitted task is terminal
//! under a head the chapter gate proved and the base branch already
//! publishes. The lapse is a written fact, so completion is something a
//! release *reads* rather than something an operator infers from silence.
//!
//! Three properties are structural rather than procedural.
//!
//! 1. **One pass at a time.** Acquisition takes an exclusive lock on the
//!    lease file and holds it for the life of the pass, so two concurrent
//!    reconcile passes over one identity cannot both admit an epoch and
//!    dispatch its frontier. A crashed holder releases the lock with its file
//!    descriptors, which is the automatic reclamation the lease model wants;
//!    nothing an operator types is in that path.
//! 2. **A lapsed lease is a closed door.** Re-acquiring against the same
//!    admitted graph is refused, so a poll against a quiescent, complete
//!    campaign is a no-op by construction rather than by a timer somebody
//!    remembered to stop. A *different* graph — the push that re-admits the
//!    identity — is a fresh activation, and only that reopens it.
//! 3. **A lapse names a proven revision.** [`CampaignLeaseGuard::lapse`]
//!    refuses a proof reference that does not name the revision it is closing
//!    over, exactly as [`crate::campaign_publish::PublishReceiptV1`] refuses
//!    an unproven publication. A completion fact that could name an ungated
//!    head would be worth no more than the silence it replaced.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::campaign_publish::{is_object_id, proof_names_revision, PublishProof};

pub const CAMPAIGN_LEASE_SCHEMA_VERSION: u32 = 1;
pub const CAMPAIGN_LEASE_FILE: &str = "campaign-lease-v1.json";
pub const CAMPAIGN_LEASE_LOCK_FILE: &str = "campaign-lease.lock";
const MAX_CAMPAIGN_LEASE_BYTES: u64 = 64 * 1024;
/// The worklist digest a checkpoint ref carries between the task id and the
/// revision it proved.
const WORKLIST_DIGEST_HEX: usize = 64;
/// The revision prefix a merge receipt ref carries after its task id.
const MERGE_REVISION_HEX: usize = 16;

#[derive(Debug, Error)]
pub enum CampaignLeaseError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("campaign lease {path} is not readable as schema {CAMPAIGN_LEASE_SCHEMA_VERSION}")]
    Decode {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("campaign lease {path} is not a bounded regular file")]
    NotBounded { path: String },
    #[error("campaign {campaign} is already leased by a live pass ({holder})")]
    Held { campaign: String, holder: String },
    #[error("campaign {campaign} lapsed at {lapsed_at} on published head {sha}")]
    Lapsed {
        campaign: String,
        sha: String,
        lapsed_at: String,
    },
    #[error("a campaign lease lapses on a revision its chapter gate proved: {reference} does not name {sha}")]
    UnprovenLapse { reference: String, sha: String },
    #[error("campaign lease field {field} is invalid: {detail}")]
    InvalidRecord { field: &'static str, detail: String },
}

impl CampaignLeaseError {
    fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

/// Which side of the lifecycle one campaign identity is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CampaignLeaseState {
    /// Activation is held: passes may admit epochs and dispatch frontiers.
    Held,
    /// The work is finished and the fact is written. Nothing dispatches under
    /// this graph again.
    Lapsed,
}

/// The process that last acquired a held lease.
///
/// This is legibility, not enforcement: the exclusive lock is what keeps a
/// second pass out, and it disappears with the holder's file descriptors
/// whether the holder exited or crashed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CampaignLeaseHolder {
    pub pid: u32,
    pub since: String,
}

/// The written completion fact a release reads instead of observing silence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CampaignLapseV1 {
    /// The admitted graph whose last task went terminal.
    pub graph_digest: String,
    /// The epoch that graph was admitted under.
    pub arm_serial: u64,
    /// Every task the graph carried, all of them terminal.
    pub tasks: Vec<String>,
    /// The one revision this campaign finished on: gate-proven and published.
    pub sha: String,
    /// The chapter gate's own durable proof of that revision.
    pub proven_by: PublishProof,
    pub lapsed_at: String,
}

/// The durable lease record for one campaign identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CampaignLeaseV1 {
    pub schema_version: u32,
    pub campaign: String,
    pub repository: String,
    pub worklist: String,
    pub state: CampaignLeaseState,
    /// The epoch the pass that last held this lease was serving.
    pub arm_serial: u64,
    /// The admitted graph the lease is bound to. A different one is a
    /// different activation.
    pub graph_digest: String,
    pub acquired_at: String,
    pub renewed_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<CampaignLeaseHolder>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lapse: Option<CampaignLapseV1>,
}

impl CampaignLeaseV1 {
    /// Whether this identity is finished under the graph the record names.
    #[must_use]
    pub const fn is_lapsed(&self) -> bool {
        matches!(self.state, CampaignLeaseState::Lapsed)
    }

    fn validate(&self) -> Result<(), CampaignLeaseError> {
        if self.schema_version != CAMPAIGN_LEASE_SCHEMA_VERSION {
            return Err(CampaignLeaseError::InvalidRecord {
                field: "schemaVersion",
                detail: format!("must equal {CAMPAIGN_LEASE_SCHEMA_VERSION}"),
            });
        }
        if self.arm_serial == 0 {
            return Err(CampaignLeaseError::InvalidRecord {
                field: "armSerial",
                detail: "must be a positive integer".to_owned(),
            });
        }
        for (field, value) in [
            ("acquiredAt", &self.acquired_at),
            ("renewedAt", &self.renewed_at),
        ] {
            DateTime::parse_from_rfc3339(value).map_err(|_| CampaignLeaseError::InvalidRecord {
                field,
                detail: "must be an RFC 3339 timestamp".to_owned(),
            })?;
        }
        match (&self.state, &self.lapse) {
            (CampaignLeaseState::Lapsed, None) => Err(CampaignLeaseError::InvalidRecord {
                field: "lapse",
                detail: "a lapsed lease carries the completion fact it lapsed on".to_owned(),
            }),
            (CampaignLeaseState::Held, Some(_)) => Err(CampaignLeaseError::InvalidRecord {
                field: "lapse",
                detail: "a held lease has not completed anything".to_owned(),
            }),
            _ => Ok(()),
        }
    }
}

/// What one activation binds: the identity, its admitted graph, and the epoch
/// the acquiring pass serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CampaignActivation {
    pub campaign: String,
    pub repository: String,
    pub worklist: String,
    pub arm_serial: u64,
    pub graph_digest: String,
}

/// How the acquiring pass came to hold the lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CampaignLeaseAcquisition {
    /// No lease existed: this is the identity's activation.
    Activated,
    /// A held lease whose last holder is gone — the ordinary next pass, and
    /// equally the reclamation after a crash.
    Resumed,
    /// A lapsed identity reopened by a graph it never finished.
    Reactivated,
}

/// A held campaign lease. Dropping it releases the exclusive lock; the
/// durable record stays exactly as the last transition wrote it.
#[derive(Debug)]
pub struct CampaignLeaseGuard {
    store: CampaignLeaseStore,
    record: CampaignLeaseV1,
    acquisition: CampaignLeaseAcquisition,
    // Held for the guard's lifetime: the lock does its work by existing, and
    // the kernel drops it whether this process exits or dies.
    #[allow(dead_code)]
    lock: File,
}

impl CampaignLeaseGuard {
    #[must_use]
    pub const fn record(&self) -> &CampaignLeaseV1 {
        &self.record
    }

    #[must_use]
    pub const fn acquisition(&self) -> CampaignLeaseAcquisition {
        self.acquisition
    }

    /// Renew the lease because this pass found work still flowing.
    pub fn renew(&mut self, now: DateTime<Utc>) -> Result<(), CampaignLeaseError> {
        self.record.renewed_at = rfc3339(now);
        self.store.write(&self.record)
    }

    /// Bind the held lease to an epoch this pass just admitted.
    ///
    /// Re-admission happens under the lease, never beside it: the pass that
    /// holds activation is the only one that can move the epoch, so the arm
    /// serial on the lease and the arm serial on the registration cannot
    /// disagree about which epoch is being served.
    pub fn admit(&mut self, arm_serial: u64, now: DateTime<Utc>) -> Result<(), CampaignLeaseError> {
        if arm_serial == 0 {
            return Err(CampaignLeaseError::InvalidRecord {
                field: "armSerial",
                detail: "must be a positive integer".to_owned(),
            });
        }
        self.record.arm_serial = arm_serial;
        self.renew(now)
    }

    /// Write the lapse fact and give up activation.
    ///
    /// The proof must name the revision being lapsed on, so the durable
    /// completion fact cannot say a campaign finished on a head no gate saw.
    pub fn lapse(
        mut self,
        sha: &str,
        proven_by: PublishProof,
        tasks: Vec<String>,
        now: DateTime<Utc>,
    ) -> Result<CampaignLapseV1, CampaignLeaseError> {
        if !proof_names_revision(&proven_by.reference, sha) {
            return Err(CampaignLeaseError::UnprovenLapse {
                reference: proven_by.reference,
                sha: sha.to_owned(),
            });
        }
        let lapsed_at = rfc3339(now);
        let lapse = CampaignLapseV1 {
            graph_digest: self.record.graph_digest.clone(),
            arm_serial: self.record.arm_serial,
            tasks,
            sha: sha.to_owned(),
            proven_by,
            lapsed_at: lapsed_at.clone(),
        };
        self.record.state = CampaignLeaseState::Lapsed;
        self.record.renewed_at = lapsed_at;
        self.record.holder = None;
        self.record.lapse = Some(lapse.clone());
        self.store.write(&self.record)?;
        Ok(lapse)
    }
}

/// The durable lease file for one campaign identity.
///
/// The directory is named for the identity — repository and worklist — and
/// never for the registration that happens to be serving it. A durable fact
/// named after a registration is a fact that dies with the registration, and
/// release reads this one long after any pass has exited.
#[derive(Debug, Clone)]
pub struct CampaignLeaseStore {
    directory: PathBuf,
    path: PathBuf,
    lock_path: PathBuf,
}

impl CampaignLeaseStore {
    #[must_use]
    pub fn new(state_dir: &Path, repository: &str, worklist: &str) -> Self {
        let directory = state_dir
            .join("campaigns/lease")
            .join(campaign_lease_scope(repository, worklist));
        Self {
            path: directory.join(CAMPAIGN_LEASE_FILE),
            lock_path: directory.join(CAMPAIGN_LEASE_LOCK_FILE),
            directory,
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the durable fact without taking the lease.
    ///
    /// This is the release-side and quiescence-side reader: it answers "is
    /// this identity finished, and on what revision" with no live pass, no
    /// registration, and no host in the picture.
    pub fn read(&self) -> Result<Option<CampaignLeaseV1>, CampaignLeaseError> {
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(CampaignLeaseError::io(
                    format!("cannot open campaign lease {}", self.path.display()),
                    error,
                ))
            }
        };
        let metadata = file.metadata().map_err(|error| {
            CampaignLeaseError::io(
                format!("cannot stat campaign lease {}", self.path.display()),
                error,
            )
        })?;
        if !metadata.is_file() || metadata.len() > MAX_CAMPAIGN_LEASE_BYTES {
            return Err(CampaignLeaseError::NotBounded {
                path: self.path.display().to_string(),
            });
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|error| {
            CampaignLeaseError::io(
                format!("cannot read campaign lease {}", self.path.display()),
                error,
            )
        })?;
        let record: CampaignLeaseV1 =
            serde_json::from_slice(&bytes).map_err(|source| CampaignLeaseError::Decode {
                path: self.path.display().to_string(),
                source,
            })?;
        record.validate()?;
        Ok(Some(record))
    }

    /// Take activation for one pass.
    ///
    /// Refuses with [`CampaignLeaseError::Held`] while another pass holds it
    /// and with [`CampaignLeaseError::Lapsed`] once this graph is finished.
    /// Both refusals are the whole point: the first is why one frontier never
    /// dispatches twice, the second is why a poll against a complete campaign
    /// costs nothing.
    pub fn acquire(
        &self,
        activation: &CampaignActivation,
        now: DateTime<Utc>,
    ) -> Result<CampaignLeaseGuard, CampaignLeaseError> {
        let lock = self.open_lock()?;
        if let Err(error) = lock.try_lock_exclusive() {
            if error.kind() != fs2::lock_contended_error().kind() {
                return Err(CampaignLeaseError::io(
                    format!("cannot lock campaign lease {}", self.lock_path.display()),
                    error,
                ));
            }
            // The holder is named from the record when it has been written.
            // A pass that took the lock microseconds ago may not have written
            // one yet, and the refusal is no less true for that.
            let holder = self.read()?.and_then(|record| record.holder);
            return Err(CampaignLeaseError::Held {
                campaign: activation.campaign.clone(),
                holder: holder.map_or_else(
                    || "holder not yet recorded".to_owned(),
                    |holder| format!("pid {}, since {}", holder.pid, holder.since),
                ),
            });
        }
        let stamp = rfc3339(now);
        let prior = self.read()?;
        let (acquired_at, acquisition) = match &prior {
            None => (stamp.clone(), CampaignLeaseAcquisition::Activated),
            Some(record) if !record.is_lapsed() => (
                record.acquired_at.clone(),
                CampaignLeaseAcquisition::Resumed,
            ),
            Some(record) => {
                // The lease lapsed. Only a graph this identity has not
                // finished reopens it; anything else is the no-op that makes
                // polling a complete campaign free.
                let lapse = record
                    .lapse
                    .as_ref()
                    .expect("a lapsed record carries its lapse fact");
                if record.graph_digest == activation.graph_digest {
                    return Err(CampaignLeaseError::Lapsed {
                        campaign: record.campaign.clone(),
                        sha: lapse.sha.clone(),
                        lapsed_at: lapse.lapsed_at.clone(),
                    });
                }
                (stamp.clone(), CampaignLeaseAcquisition::Reactivated)
            }
        };
        let record = CampaignLeaseV1 {
            schema_version: CAMPAIGN_LEASE_SCHEMA_VERSION,
            campaign: activation.campaign.clone(),
            repository: activation.repository.clone(),
            worklist: activation.worklist.clone(),
            state: CampaignLeaseState::Held,
            arm_serial: activation.arm_serial,
            graph_digest: activation.graph_digest.clone(),
            acquired_at,
            renewed_at: stamp.clone(),
            holder: Some(CampaignLeaseHolder {
                pid: std::process::id(),
                since: stamp,
            }),
            lapse: None,
        };
        record.validate()?;
        self.write(&record)?;
        Ok(CampaignLeaseGuard {
            store: self.clone(),
            record,
            acquisition,
            lock,
        })
    }

    fn open_lock(&self) -> Result<File, CampaignLeaseError> {
        fs::create_dir_all(&self.directory).map_err(|error| {
            CampaignLeaseError::io(
                format!(
                    "cannot create campaign lease directory {}",
                    self.directory.display()
                ),
                error,
            )
        })?;
        fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700)).map_err(
            |error| {
                CampaignLeaseError::io(
                    format!(
                        "cannot secure campaign lease directory {}",
                        self.directory.display()
                    ),
                    error,
                )
            },
        )?;
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.lock_path)
            .map_err(|error| {
                CampaignLeaseError::io(
                    format!(
                        "cannot open campaign lease lock {}",
                        self.lock_path.display()
                    ),
                    error,
                )
            })?;
        if !lock
            .metadata()
            .map_err(|error| {
                CampaignLeaseError::io(
                    format!(
                        "cannot stat campaign lease lock {}",
                        self.lock_path.display()
                    ),
                    error,
                )
            })?
            .is_file()
        {
            return Err(CampaignLeaseError::NotBounded {
                path: self.lock_path.display().to_string(),
            });
        }
        Ok(lock)
    }

    fn write(&self, record: &CampaignLeaseV1) -> Result<(), CampaignLeaseError> {
        let temporary = self.directory.join(format!(
            ".{}.{}.tmp",
            record.arm_serial,
            uuid::Uuid::now_v7()
        ));
        let mut bytes =
            serde_json::to_vec(record).map_err(|source| CampaignLeaseError::Decode {
                path: self.path.display().to_string(),
                source,
            })?;
        bytes.push(b'\n');
        let write = |path: &Path| -> std::io::Result<()> {
            use std::io::Write as _;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC)
                .open(path)?;
            file.write_all(&bytes)?;
            file.sync_all()
        };
        write(&temporary).map_err(|error| {
            CampaignLeaseError::io(
                format!("cannot stage campaign lease {}", temporary.display()),
                error,
            )
        })?;
        fs::rename(&temporary, &self.path).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            CampaignLeaseError::io(
                format!("cannot publish campaign lease {}", self.path.display()),
                error,
            )
        })?;
        File::open(&self.directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                CampaignLeaseError::io(
                    format!(
                        "cannot flush campaign lease directory {}",
                        self.directory.display()
                    ),
                    error,
                )
            })
    }
}

/// The durable name of one campaign identity's lease, derived from the
/// identity itself.
#[must_use]
pub fn campaign_lease_scope(repository: &str, worklist: &str) -> String {
    let mut identity = repository.as_bytes().to_vec();
    identity.push(0);
    identity.extend_from_slice(worklist.as_bytes());
    format!("{:x}", Sha256::digest(&identity))[..24].to_owned()
}

/// One admitted task, as the lease reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CampaignLeaseTask {
    pub id: String,
    /// Checkpoint tasks are the campaign's gates; the last one is the chapter
    /// gate whose proof publication rests on.
    pub checkpoint: bool,
}

impl CampaignLeaseTask {
    #[must_use]
    pub fn new(id: impl Into<String>, checkpoint: bool) -> Self {
        Self {
            id: id.into(),
            checkpoint,
        }
    }
}

/// The witnessed facts one lease decision rests on.
///
/// Every field is an observation. `campaign_refs` and `published_head` are
/// what the identity's authority remote reports, and `live_nodes` is what the
/// pass has in flight — nothing here is a prediction about work to come.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CampaignLeaseFacts {
    /// The campaign-scoped hidden-ref namespace, without a trailing slash.
    pub state_prefix: String,
    /// Every task the admitted graph carries, in admitted order.
    pub tasks: Vec<CampaignLeaseTask>,
    /// Campaign-scoped refs at the authority remote: name to object ID.
    pub campaign_refs: BTreeMap<String, String>,
    /// The base branch head at the authority remote.
    pub published_head: String,
    /// Nodes this identity still has running.
    pub live_nodes: usize,
}

/// What a pass must do with the lease it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CampaignLeaseDisposition {
    /// Work is still flowing, or its proof is not in yet. The reason is the
    /// sentence a poll event carries.
    Renew { reason: String },
    /// The last task is terminal on a proven, published head.
    Lapse {
        sha: String,
        proven_by: PublishProof,
        tasks: Vec<String>,
        reason: String,
    },
}

impl CampaignLeaseDisposition {
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Renew { reason } | Self::Lapse { reason, .. } => reason,
        }
    }

    #[must_use]
    pub const fn lapses(&self) -> bool {
        matches!(self, Self::Lapse { .. })
    }

    fn renew(reason: impl Into<String>) -> Self {
        Self::Renew {
            reason: reason.into(),
        }
    }
}

/// Decide the lease's fate from witnessed facts alone.
///
/// The order is the model: work in flight outranks everything, an
/// un-terminated task outranks the gate, and no proof of the published head
/// means no lapse however finished the tasks look.
#[must_use]
pub fn lease_disposition(facts: &CampaignLeaseFacts) -> CampaignLeaseDisposition {
    if facts.live_nodes > 0 {
        return CampaignLeaseDisposition::renew(format!(
            "{} node(s) are still running under this lease",
            facts.live_nodes
        ));
    }
    let outstanding = facts
        .tasks
        .iter()
        .filter(|task| !task_is_terminal(&facts.state_prefix, &facts.campaign_refs, task))
        .map(|task| task.id.as_str())
        .collect::<Vec<_>>();
    if !outstanding.is_empty() {
        return CampaignLeaseDisposition::renew(format!(
            "{} of {} admitted task(s) carry no completion fact: {}",
            outstanding.len(),
            facts.tasks.len(),
            bounded_list(&outstanding)
        ));
    }
    let Some(gate) = facts.tasks.iter().rev().find(|task| task.checkpoint) else {
        return CampaignLeaseDisposition::renew(
            "the admitted graph declares no chapter gate, so no head is proven".to_owned(),
        );
    };
    if !is_object_id(&facts.published_head) {
        return CampaignLeaseDisposition::renew(format!(
            "the authority remote reports no publishable base head ({})",
            facts.published_head
        ));
    }
    let Some(reference) = gate_proof_reference(
        &facts.state_prefix,
        &facts.campaign_refs,
        &gate.id,
        &facts.published_head,
    ) else {
        return CampaignLeaseDisposition::renew(format!(
            "chapter gate {} has not proven published head {}",
            gate.id, facts.published_head
        ));
    };
    CampaignLeaseDisposition::Lapse {
        sha: facts.published_head.clone(),
        proven_by: PublishProof {
            task_id: gate.id.clone(),
            reference,
        },
        tasks: facts.tasks.iter().map(|task| task.id.clone()).collect(),
        reason: format!(
            "every admitted task is terminal and chapter gate {} proved published head {}",
            gate.id, facts.published_head
        ),
    }
}

/// Whether a durable repository fact says this task is finished.
///
/// An implementation task ends in a merge receipt; a checkpoint task ends in
/// the checkpoint ref that records what it proved.
fn task_is_terminal(
    state_prefix: &str,
    campaign_refs: &BTreeMap<String, String>,
    task: &CampaignLeaseTask,
) -> bool {
    let (kind, matches): (&str, fn(&str, &str) -> bool) = if task.checkpoint {
        ("checkpoint", checkpoint_names_task)
    } else {
        ("merge", merge_names_task)
    };
    let namespace = format!("{state_prefix}/{kind}/");
    campaign_refs.keys().any(|reference| {
        reference
            .strip_prefix(&namespace)
            .is_some_and(|suffix| matches(suffix, &task.id))
    })
}

/// The chapter gate's own proof of one revision, when the remote carries it.
fn gate_proof_reference(
    state_prefix: &str,
    campaign_refs: &BTreeMap<String, String>,
    gate_id: &str,
    revision: &str,
) -> Option<String> {
    let namespace = format!("{state_prefix}/checkpoint/");
    campaign_refs
        .iter()
        .find(|(reference, target)| {
            target.as_str() == revision
                && proof_names_revision(reference, revision)
                && reference
                    .strip_prefix(&namespace)
                    .is_some_and(|suffix| checkpoint_names_task(suffix, gate_id))
        })
        .map(|(reference, _)| reference.clone())
}

/// `{task}` or `{task}-{16 hex}` — the two shapes a merge receipt ref takes.
///
/// The revision suffix is measured rather than assumed: task IDs carry
/// hyphens too, and `merge/the-lease` must not read as task `the` finished at
/// some revision.
fn merge_names_task(suffix: &str, task_id: &str) -> bool {
    match suffix.strip_prefix(task_id) {
        Some("") => true,
        Some(rest) => rest
            .strip_prefix('-')
            .is_some_and(|revision| is_lowercase_hex(revision, MERGE_REVISION_HEX)),
        None => false,
    }
}

/// `{task}-{worklist digest}/{revision}` — the checkpoint ref shape.
fn checkpoint_names_task(suffix: &str, task_id: &str) -> bool {
    let Some((head, revision)) = suffix.split_once('/') else {
        return false;
    };
    is_object_id(revision)
        && head.strip_prefix(task_id).is_some_and(|rest| {
            rest.strip_prefix('-')
                .is_some_and(|digest| is_lowercase_hex(digest, WORKLIST_DIGEST_HEX))
        })
}

fn is_lowercase_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn bounded_list(values: &[&str]) -> String {
    const SHOWN: usize = 5;
    let head = values
        .iter()
        .take(SHOWN)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    if values.len() > SHOWN {
        format!("{head}, and {} more", values.len() - SHOWN)
    } else {
        head
    }
}

fn rfc3339(now: DateTime<Utc>) -> String {
    now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &str = "refs/tally/spec-build/v1/049836c3e38c7ecc9c638e9c";

    fn head(byte: char) -> String {
        std::iter::repeat_n(byte, 40).collect()
    }

    fn worklist_digest() -> String {
        "c".repeat(WORKLIST_DIGEST_HEX)
    }

    fn tasks() -> Vec<CampaignLeaseTask> {
        vec![
            CampaignLeaseTask::new("foundation", false),
            CampaignLeaseTask::new("the-lease", false),
            CampaignLeaseTask::new("chapter-gate-c2", true),
        ]
    }

    fn finished_refs(revision: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                format!(
                    "{PREFIX}/merge/foundation-{}",
                    "a".repeat(MERGE_REVISION_HEX)
                ),
                head('1'),
            ),
            (format!("{PREFIX}/merge/the-lease"), head('2')),
            (
                format!(
                    "{PREFIX}/checkpoint/chapter-gate-c2-{}/{revision}",
                    worklist_digest()
                ),
                revision.to_owned(),
            ),
        ])
    }

    fn facts(revision: &str) -> CampaignLeaseFacts {
        CampaignLeaseFacts {
            state_prefix: PREFIX.to_owned(),
            tasks: tasks(),
            campaign_refs: finished_refs(revision),
            published_head: revision.to_owned(),
            live_nodes: 0,
        }
    }

    fn activation(digest: &str) -> CampaignActivation {
        CampaignActivation {
            campaign: "eta".to_owned(),
            repository: "mecattaf/tally.nix".to_owned(),
            worklist: "silent-factory-worklists/eta.json".to_owned(),
            arm_serial: 1,
            graph_digest: digest.to_owned(),
        }
    }

    fn moment(second: u32) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("2026-08-18T09:00:{second:02}Z"))
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn lease_lapses_when_the_last_task_is_terminal_under_a_proven_published_head() {
        let revision = head('7');
        let disposition = lease_disposition(&facts(&revision));
        let CampaignLeaseDisposition::Lapse {
            sha,
            proven_by,
            tasks,
            reason,
        } = disposition
        else {
            panic!("a finished campaign lapses: {disposition:?}");
        };
        assert_eq!(sha, revision);
        assert_eq!(proven_by.task_id, "chapter-gate-c2");
        assert!(
            proof_names_revision(&proven_by.reference, &sha),
            "{proven_by:?}"
        );
        assert_eq!(tasks, ["foundation", "the-lease", "chapter-gate-c2"]);
        assert!(reason.contains("chapter-gate-c2"), "{reason}");
    }

    #[test]
    fn lease_renews_while_any_admitted_task_still_owes_a_completion_fact() {
        let revision = head('7');
        let mut facts = facts(&revision);
        facts
            .campaign_refs
            .remove(&format!("{PREFIX}/merge/the-lease"));
        let disposition = lease_disposition(&facts);
        assert!(!disposition.lapses(), "{disposition:?}");
        assert!(
            disposition.reason().contains("the-lease"),
            "{disposition:?}"
        );

        // A merge receipt for a *different* task whose ID this one prefixes is
        // not this task's completion.
        facts
            .campaign_refs
            .insert(format!("{PREFIX}/merge/the-lease-extended"), head('3'));
        assert!(!lease_disposition(&facts).lapses());
    }

    #[test]
    fn lease_renews_while_work_flows_or_the_published_head_is_unproven() {
        let revision = head('7');
        let mut live = facts(&revision);
        live.live_nodes = 2;
        assert!(live.live_nodes > 0 && !lease_disposition(&live).lapses());
        assert!(lease_disposition(&live).reason().contains('2'));

        // Everything is merged and the gate proved a revision — but the base
        // branch publishes a different one, so nothing has finished yet.
        let mut unpublished = facts(&revision);
        unpublished.published_head = head('9');
        let disposition = lease_disposition(&unpublished);
        assert!(!disposition.lapses(), "{disposition:?}");
        assert!(disposition.reason().contains(&head('9')), "{disposition:?}");

        // And a gate ref that names the published head but points elsewhere is
        // not a proof of it.
        let mut misdirected = facts(&revision);
        misdirected.campaign_refs.insert(
            format!(
                "{PREFIX}/checkpoint/chapter-gate-c2-{}/{revision}",
                worklist_digest()
            ),
            head('4'),
        );
        assert!(!lease_disposition(&misdirected).lapses());
    }

    #[test]
    fn lease_renews_when_the_admitted_graph_declares_no_chapter_gate() {
        let revision = head('7');
        let mut ungated = facts(&revision);
        ungated.tasks.retain(|task| !task.checkpoint);
        ungated
            .campaign_refs
            .retain(|reference, _| !reference.contains("/checkpoint/"));
        let disposition = lease_disposition(&ungated);
        assert!(!disposition.lapses(), "{disposition:?}");
        assert!(
            disposition.reason().contains("chapter gate"),
            "{disposition:?}"
        );
    }

    #[test]
    fn lease_acquisition_excludes_a_second_live_pass_and_reclaims_after_a_crash() {
        let temporary = tempfile::tempdir().unwrap();
        let store = CampaignLeaseStore::new(temporary.path(), "mecattaf/tally.nix", "eta.json");
        assert!(store.read().unwrap().is_none());

        let first = store.acquire(&activation("sha256:aa"), moment(1)).unwrap();
        assert_eq!(first.acquisition(), CampaignLeaseAcquisition::Activated);
        assert_eq!(first.record().state, CampaignLeaseState::Held);

        let refused = store
            .acquire(&activation("sha256:aa"), moment(2))
            .unwrap_err();
        assert!(
            matches!(refused, CampaignLeaseError::Held { .. }),
            "a second live pass must be refused: {refused}"
        );
        assert!(refused.to_string().contains("already leased"), "{refused}");

        // The holder going away — cleanly or not — is the whole reclamation
        // story: the next pass resumes the same activation.
        drop(first);
        let second = store.acquire(&activation("sha256:aa"), moment(3)).unwrap();
        assert_eq!(second.acquisition(), CampaignLeaseAcquisition::Resumed);
        assert_eq!(second.record().acquired_at, rfc3339(moment(1)));
        assert_eq!(second.record().renewed_at, rfc3339(moment(3)));
    }

    #[test]
    fn lease_lapse_is_durable_closes_the_door_and_reopens_only_for_a_new_graph() {
        let temporary = tempfile::tempdir().unwrap();
        let store = CampaignLeaseStore::new(temporary.path(), "mecattaf/tally.nix", "eta.json");
        let revision = head('7');
        let mut guard = store.acquire(&activation("sha256:aa"), moment(1)).unwrap();
        guard.renew(moment(2)).unwrap();

        let CampaignLeaseDisposition::Lapse {
            sha,
            proven_by,
            tasks,
            ..
        } = lease_disposition(&facts(&revision))
        else {
            panic!("the fixture campaign is finished");
        };
        let lapsed = guard.lapse(&sha, proven_by, tasks, moment(3)).unwrap();

        // Durable, not merely in hand: a release reads this with no pass, no
        // registration, and no host in the picture.
        let reread = store.read().unwrap().unwrap();
        assert!(reread.is_lapsed() && reread.holder.is_none());
        let fact = reread.lapse.unwrap();
        assert_eq!(fact, lapsed);
        assert_eq!(fact.sha, revision);
        assert_eq!(fact.lapsed_at, rfc3339(moment(3)));
        assert_eq!(fact.graph_digest, "sha256:aa");
        assert_eq!(fact.tasks, ["foundation", "the-lease", "chapter-gate-c2"]);

        // The door is closed for the graph that finished...
        let refused = store
            .acquire(&activation("sha256:aa"), moment(4))
            .unwrap_err();
        assert!(
            matches!(refused, CampaignLeaseError::Lapsed { .. }),
            "a complete campaign must refuse activation: {refused}"
        );

        // ...and open for the push that re-admits the identity.
        let reopened = store.acquire(&activation("sha256:bb"), moment(5)).unwrap();
        assert_eq!(
            reopened.acquisition(),
            CampaignLeaseAcquisition::Reactivated
        );
        assert_eq!(reopened.record().acquired_at, rfc3339(moment(5)));
        assert!(reopened.record().lapse.is_none());
    }

    #[test]
    fn lease_refuses_to_lapse_on_a_head_its_proof_does_not_name() {
        let temporary = tempfile::tempdir().unwrap();
        let store = CampaignLeaseStore::new(temporary.path(), "mecattaf/tally.nix", "eta.json");
        let guard = store.acquire(&activation("sha256:aa"), moment(1)).unwrap();
        let refused = guard
            .lapse(
                &head('7'),
                PublishProof {
                    task_id: "chapter-gate-c2".to_owned(),
                    reference: format!("{PREFIX}/checkpoint/chapter-gate-c2/{}", head('9')),
                },
                vec!["foundation".to_owned()],
                moment(2),
            )
            .unwrap_err();
        assert!(
            matches!(refused, CampaignLeaseError::UnprovenLapse { .. }),
            "{refused}"
        );
        // And nothing durable moved: the identity is still live.
        assert!(!store.read().unwrap().unwrap().is_lapsed());
    }
}
