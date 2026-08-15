//! The canonical-versus-derived durability law.
//!
//! A **canonical** surface records an input, observation, receipt, or operator
//! decision that tally cannot reproduce after losing it. Canonical does not
//! mean hash-chained or globally authoritative: `witness.jsonl` is still the
//! sole proof surface, while (for example) flow membership is the canonical
//! answer to a narrower question the witness chain cannot answer.
//!
//! A **derived** surface carries no independent fact. It may be cached in
//! memory, returned over RPC, or persisted as bounded convenience state, but
//! every write is a replay of canonical records or a rebuild from them. It is
//! therefore safe to discard and regenerate. In particular there is no
//! durable task database: acknowledged enqueue events and the witness ledger
//! are canonical; the daemon's row table and all `query`/`query_v2` values are
//! derived.
//!
//! [`CANONICAL_SURFACES`] and [`DERIVED_SURFACES`] are the single source-level
//! declaration. The marker types make a surface's class explicit, and a
//! [`DerivedSurface`] cannot describe a direct write: its only representable
//! write rules are [`DerivedWrite::Replay`] and [`DerivedWrite::Rebuild`]. The
//! tests below also require every derived declaration to name existing
//! canonical inputs. Locks, sockets, temporary files, and Nix store objects
//! owned outside tally are coordination or transport, not durable tally
//! surfaces, and are intentionally absent.

/// Stable identity of a canonical durability surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CanonicalSurfaceId {
    /// Repository/Nix declarations and campaign worklist bytes.
    DeclaredInputs,
    /// Acknowledged task admission events under `state/events/`.
    AdmissionEvents,
    /// The coordinator's hash-chained verdict ledger.
    WitnessLedger,
    /// Coordinator and executor advisory attestation ledgers.
    AttestationLedgers,
    /// Lifecycle observations plus the receipt for any compacted prefix.
    LifecycleRecords,
    /// Row-less and ordinary flow-run membership facts.
    FlowMembership,
    /// Explicit predecessor-to-successor flow-run transitions.
    FlowLineage,
    /// Explicit operator archive/tag decisions.
    ReaderState,
    /// Lease epochs, grants, releases, debits, and yield receipts.
    LeaseRecords,
    /// Unit launch-generation and exit observations.
    ExecutionRecords,
    /// Raw, retained, and archived execution captures and gate manifests.
    ExecutionCaptures,
    /// Content-addressed brief documents.
    BriefDocuments,
    /// Campaign authority registrations and host-local tuning.
    CampaignRegistrations,
    /// Approved graphs and immutable flow/driver snapshots and manifests.
    CampaignAssets,
    /// Append-only local attempt and steering receipts plus receipt authority.
    CampaignReceipts,
    /// Integration branches, immutable receipt refs, and trailer-marked commits.
    RepositoryReceipts,
    /// Producer ingress files, including accepted and rejected archives.
    ProducerIngress,
    /// Last-trigger/emission/error observations made by a producer.
    ProducerRuntimeRecords,
    /// Storage samples, episode state, and warning receipts.
    StorageRecords,
    /// External or exactly reduced usage-meter observations.
    UsageMeterObservations,
    /// Bounded change notifications with stable cursors inside their window.
    ChangeRecords,
}

/// Stable identity of a derived surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DerivedSurfaceId {
    /// Daemon rows, live jobs, aliases, guardrails, and recovered task state.
    TaskDatabase,
    /// Recovered lease holders, queues, pools, and window debits.
    LeaseState,
    /// Verified witness records and their task/dedup lookup indexes.
    WitnessIndexes,
    /// Parsed flow membership and lineage lookup indexes.
    FlowIndexes,
    /// Parsed lifecycle records and cached snapshots.
    LifecycleIndex,
    /// Row/detail maps used as inputs to query construction.
    QueryIndexes,
    /// Every return value constructed by `query` and `query_v2`.
    QueryProjections,
    /// The offline run view rebuilt without a live daemon.
    DurableRunView,
    /// Bounded in-memory pagination snapshots.
    PaginationSnapshots,
    /// The in-memory change window and `query.watch` response.
    WatchProjection,
    /// Witness-owned Nix GC-root links.
    WitnessGcRoots,
    /// Campaign asset Nix GC-root links.
    CampaignAssetGcRoots,
    /// The recovered failure-stderr copy and its replay cursor.
    FailureStderrProjection,
    /// Effective registration values after authority, tuning, and assets join.
    EffectiveCampaignRegistration,
    /// Queryable storage, producer, and pool headroom snapshots.
    OperationalProjections,
    /// Usage and campaign summary folds.
    AggregateProjections,
}

/// Where a declared surface is materialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceMedium {
    /// Durable files or directory entries owned by tally.
    Filesystem,
    /// Git objects or references.
    Git,
    /// Process-local state that disappears on restart.
    Memory,
    /// A value constructed for a response and not retained as authority.
    Response,
}

/// The only legal way to materialize a derived surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivedWrite {
    /// Fold a newly durable canonical record into an existing materialization.
    Replay,
    /// Recompute the materialization from canonical inputs.
    Rebuild,
}

/// A typed declaration of a canonical surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalSurface {
    id: CanonicalSurfaceId,
    owners: &'static [&'static str],
    medium: SurfaceMedium,
    locations: &'static [&'static str],
}

impl CanonicalSurface {
    const fn new(
        id: CanonicalSurfaceId,
        owners: &'static [&'static str],
        medium: SurfaceMedium,
        locations: &'static [&'static str],
    ) -> Self {
        Self {
            id,
            owners,
            medium,
            locations,
        }
    }

    /// Stable catalog identity.
    #[must_use]
    pub const fn id(self) -> CanonicalSurfaceId {
        self.id
    }

    /// Rust modules that own or consume the surface's format.
    #[must_use]
    pub const fn owners(self) -> &'static [&'static str] {
        self.owners
    }

    /// Persistence medium.
    #[must_use]
    pub const fn medium(self) -> SurfaceMedium {
        self.medium
    }

    /// Human-readable path or namespace patterns.
    #[must_use]
    pub const fn locations(self) -> &'static [&'static str] {
        self.locations
    }
}

/// A typed declaration of a derived surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivedSurface {
    id: DerivedSurfaceId,
    owners: &'static [&'static str],
    medium: SurfaceMedium,
    locations: &'static [&'static str],
    write: DerivedWrite,
    canonical_inputs: &'static [CanonicalSurfaceId],
}

impl DerivedSurface {
    const fn new(
        id: DerivedSurfaceId,
        owners: &'static [&'static str],
        medium: SurfaceMedium,
        locations: &'static [&'static str],
        write: DerivedWrite,
        canonical_inputs: &'static [CanonicalSurfaceId],
    ) -> Self {
        assert!(
            !canonical_inputs.is_empty(),
            "a derived surface must name a canonical replay/rebuild input"
        );
        Self {
            id,
            owners,
            medium,
            locations,
            write,
            canonical_inputs,
        }
    }

    /// Stable catalog identity.
    #[must_use]
    pub const fn id(self) -> DerivedSurfaceId {
        self.id
    }

    /// Rust modules that own the replay/rebuild.
    #[must_use]
    pub const fn owners(self) -> &'static [&'static str] {
        self.owners
    }

    /// Materialization medium.
    #[must_use]
    pub const fn medium(self) -> SurfaceMedium {
        self.medium
    }

    /// Human-readable path, type, or namespace patterns.
    #[must_use]
    pub const fn locations(self) -> &'static [&'static str] {
        self.locations
    }

    /// Whether the surface is incrementally replayed or fully rebuilt.
    #[must_use]
    pub const fn write(self) -> DerivedWrite {
        self.write
    }

    /// Canonical roots from which the surface is materialized.
    #[must_use]
    pub const fn canonical_inputs(self) -> &'static [CanonicalSurfaceId] {
        self.canonical_inputs
    }
}

/// Every canonical surface known to `tally-core`.
///
/// Multiple concrete files are grouped only when they form one atomic
/// semantic surface (for example a lifecycle suffix and its compaction
/// receipt). Adding a durable writer requires adding or extending an entry
/// here and documenting the owning module.
pub const CANONICAL_SURFACES: &[CanonicalSurface] = &[
    CanonicalSurface::new(
        CanonicalSurfaceId::DeclaredInputs,
        &["config", "campaign_contract"],
        SurfaceMedium::Git,
        &["repository/Nix declarations", "campaign worklist"],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::AdmissionEvents,
        &["taskdb", "producers::ingress"],
        SurfaceMedium::Filesystem,
        &["state/events/*.enqueue.json"],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::WitnessLedger,
        &["witness"],
        SurfaceMedium::Filesystem,
        &["data/witness.jsonl"],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::AttestationLedgers,
        &["witness", "exec_attestation"],
        SurfaceMedium::Filesystem,
        &[
            "data/attestations.jsonl",
            "<executor-state>/exec-attestations.jsonl",
        ],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::LifecycleRecords,
        &["history"],
        SurfaceMedium::Filesystem,
        &["data/lifecycle.jsonl", "data/lifecycle-retention.json"],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::FlowMembership,
        &["flow_membership"],
        SurfaceMedium::Filesystem,
        &["data/flow-membership.jsonl"],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::FlowLineage,
        &["flow_lineage"],
        SurfaceMedium::Filesystem,
        &["data/flow-lineage.jsonl"],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::ReaderState,
        &["reader_state"],
        SurfaceMedium::Filesystem,
        &["data/reader-state.jsonl"],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::LeaseRecords,
        &["lease"],
        SurfaceMedium::Filesystem,
        &["state/lease_epoch", "state/lease-events.jsonl"],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::ExecutionRecords,
        &["executor"],
        SurfaceMedium::Filesystem,
        &["state/unit-exit/*.json", "state/unit-exit/*.capture.json"],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::ExecutionCaptures,
        &["executor", "completion"],
        SurfaceMedium::Filesystem,
        &[
            "state/capture/*.out",
            "state/capture/*.adapter.err",
            "state/capture/*.gates.json",
            "state/capture/archive/**",
        ],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::BriefDocuments,
        &["brief"],
        SurfaceMedium::Filesystem,
        &["data/briefs/<sha256>.json"],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::CampaignRegistrations,
        &["campaign_registry"],
        SurfaceMedium::Filesystem,
        &[
            "state/campaigns/armed/*.json",
            "state/campaigns/host-tuning/*.host-v1.json",
        ],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::CampaignAssets,
        &["campaign_registry", "campaign_contract"],
        SurfaceMedium::Filesystem,
        &[
            "state/campaigns/assets/**/assets-v1.json",
            "state/campaigns/assets/**/snapshots/*",
            "state/campaigns/approved-graphs/**/*.json",
        ],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::CampaignReceipts,
        &["attempt_receipts", "campaign_folds"],
        SurfaceMedium::Filesystem,
        &[
            "state/campaigns/attempt-receipts/*/attempt-receipts-v1.jsonl",
            "state/campaigns/attempt-receipts/*/receipt-authority-v1.json",
            "state/campaigns/steering/*/steering-v1.jsonl",
        ],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::RepositoryReceipts,
        &["campaign_folds", "query_v2"],
        SurfaceMedium::Git,
        &[
            "integration branches",
            "refs/tally/spec-build/v1/**",
            "Tally-Task/Tally-Revision trailer-marked commits",
        ],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::ProducerIngress,
        &["producers::ingress"],
        SurfaceMedium::Filesystem,
        &[
            "state/events/*.producer.json",
            "state/events/{processing,done,rejected}/**",
        ],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::ProducerRuntimeRecords,
        &["producers"],
        SurfaceMedium::Filesystem,
        &["state/producers/*.runtime.json"],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::StorageRecords,
        &["storage"],
        SurfaceMedium::Filesystem,
        &["data/storage-metrics.json", "data/storage-warnings.jsonl"],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::UsageMeterObservations,
        &["daemon::rpc::query"],
        SurfaceMedium::Filesystem,
        &["state/meters/*.json"],
    ),
    CanonicalSurface::new(
        CanonicalSurfaceId::ChangeRecords,
        &["watch", "daemon"],
        SurfaceMedium::Filesystem,
        &["data/changes.jsonl"],
    ),
];

/// Canonical inputs read by [`crate::durable_view::durable_run_view`] and the
/// promoted [`crate::durable_view::rebuild_run_view`].
///
/// Execution records supply rebuild's unit-liveness corroboration. Captures
/// are optional enrichment, but remain declared because passing an executor
/// makes them an input to either rebuilt view.
pub const DURABLE_RUN_VIEW_INPUTS: &[CanonicalSurfaceId] = &[
    CanonicalSurfaceId::AdmissionEvents,
    CanonicalSurfaceId::WitnessLedger,
    CanonicalSurfaceId::LifecycleRecords,
    CanonicalSurfaceId::FlowMembership,
    CanonicalSurfaceId::FlowLineage,
    CanonicalSurfaceId::AttestationLedgers,
    CanonicalSurfaceId::ReaderState,
    CanonicalSurfaceId::ExecutionRecords,
    CanonicalSurfaceId::ExecutionCaptures,
];

/// Every derived surface known to `tally-core`.
pub const DERIVED_SURFACES: &[DerivedSurface] = &[
    DerivedSurface::new(
        DerivedSurfaceId::TaskDatabase,
        &["daemon", "recovery"],
        SurfaceMedium::Memory,
        &["Context::{jobs,rows,aliases,guardrails}"],
        DerivedWrite::Replay,
        &[
            CanonicalSurfaceId::DeclaredInputs,
            CanonicalSurfaceId::AdmissionEvents,
            CanonicalSurfaceId::WitnessLedger,
            CanonicalSurfaceId::ExecutionRecords,
        ],
    ),
    DerivedSurface::new(
        DerivedSurfaceId::LeaseState,
        &["lease", "daemon"],
        SurfaceMedium::Memory,
        &["LeaseEngine runtime pools/holders/pending/debits"],
        DerivedWrite::Replay,
        &[
            CanonicalSurfaceId::DeclaredInputs,
            CanonicalSurfaceId::LeaseRecords,
            CanonicalSurfaceId::WitnessLedger,
        ],
    ),
    DerivedSurface::new(
        DerivedSurfaceId::WitnessIndexes,
        &["daemon::witness_view"],
        SurfaceMedium::Memory,
        &["WitnessView::{records,by_task,by_dedup}"],
        DerivedWrite::Replay,
        &[CanonicalSurfaceId::WitnessLedger],
    ),
    DerivedSurface::new(
        DerivedSurfaceId::FlowIndexes,
        &["flow_membership", "flow_lineage", "daemon"],
        SurfaceMedium::Memory,
        &["FlowMembership/FlowLineage maps and daemon caches"],
        DerivedWrite::Replay,
        &[
            CanonicalSurfaceId::FlowMembership,
            CanonicalSurfaceId::FlowLineage,
        ],
    ),
    DerivedSurface::new(
        DerivedSurfaceId::LifecycleIndex,
        &["history"],
        SurfaceMedium::Memory,
        &["LifecycleStore records/shared snapshot"],
        DerivedWrite::Replay,
        &[CanonicalSurfaceId::LifecycleRecords],
    ),
    DerivedSurface::new(
        DerivedSurfaceId::QueryIndexes,
        &["daemon", "query", "query_v2"],
        SurfaceMedium::Memory,
        &["Context::{query_rows,query_details}"],
        DerivedWrite::Replay,
        &[
            CanonicalSurfaceId::AdmissionEvents,
            CanonicalSurfaceId::WitnessLedger,
            CanonicalSurfaceId::LifecycleRecords,
            CanonicalSurfaceId::AttestationLedgers,
        ],
    ),
    DerivedSurface::new(
        DerivedSurfaceId::QueryProjections,
        &["query", "query_v2", "producer_query"],
        SurfaceMedium::Response,
        &["all query/query_v2 return values"],
        DerivedWrite::Rebuild,
        &[
            CanonicalSurfaceId::DeclaredInputs,
            CanonicalSurfaceId::AdmissionEvents,
            CanonicalSurfaceId::WitnessLedger,
            CanonicalSurfaceId::LifecycleRecords,
            CanonicalSurfaceId::FlowMembership,
            CanonicalSurfaceId::FlowLineage,
            CanonicalSurfaceId::ReaderState,
            CanonicalSurfaceId::CampaignRegistrations,
            CanonicalSurfaceId::CampaignReceipts,
            CanonicalSurfaceId::RepositoryReceipts,
            CanonicalSurfaceId::ProducerRuntimeRecords,
            CanonicalSurfaceId::StorageRecords,
            CanonicalSurfaceId::UsageMeterObservations,
        ],
    ),
    DerivedSurface::new(
        DerivedSurfaceId::DurableRunView,
        &["durable_view"],
        SurfaceMedium::Response,
        &["DurableRunView"],
        DerivedWrite::Rebuild,
        DURABLE_RUN_VIEW_INPUTS,
    ),
    DerivedSurface::new(
        DerivedSurfaceId::PaginationSnapshots,
        &["pagination"],
        SurfaceMedium::Memory,
        &["PageCache snapshots"],
        DerivedWrite::Rebuild,
        &[
            CanonicalSurfaceId::AdmissionEvents,
            CanonicalSurfaceId::WitnessLedger,
            CanonicalSurfaceId::LifecycleRecords,
        ],
    ),
    DerivedSurface::new(
        DerivedSurfaceId::WatchProjection,
        &["watch"],
        SurfaceMedium::Response,
        &["ChangeStore in-memory window", "WatchEnvelope"],
        DerivedWrite::Replay,
        &[CanonicalSurfaceId::ChangeRecords],
    ),
    DerivedSurface::new(
        DerivedSurfaceId::WitnessGcRoots,
        &["retention", "nix_store"],
        SurfaceMedium::Filesystem,
        &["data/gcroots/witness-*/**"],
        DerivedWrite::Rebuild,
        &[CanonicalSurfaceId::WitnessLedger],
    ),
    DerivedSurface::new(
        DerivedSurfaceId::CampaignAssetGcRoots,
        &["campaign_registry", "nix_store"],
        SurfaceMedium::Filesystem,
        &["state/campaigns/assets/**/roots/*"],
        DerivedWrite::Rebuild,
        &[
            CanonicalSurfaceId::CampaignRegistrations,
            CanonicalSurfaceId::CampaignAssets,
        ],
    ),
    DerivedSurface::new(
        DerivedSurfaceId::FailureStderrProjection,
        &["daemon::startup", "executor"],
        SurfaceMedium::Filesystem,
        &["state/capture/*.err", "state/failure-stderr-cursor.json"],
        DerivedWrite::Replay,
        &[
            CanonicalSurfaceId::WitnessLedger,
            CanonicalSurfaceId::ExecutionCaptures,
        ],
    ),
    DerivedSurface::new(
        DerivedSurfaceId::EffectiveCampaignRegistration,
        &["campaign_registry"],
        SurfaceMedium::Memory,
        &["CampaignRegistration"],
        DerivedWrite::Rebuild,
        &[
            CanonicalSurfaceId::CampaignRegistrations,
            CanonicalSurfaceId::CampaignAssets,
        ],
    ),
    DerivedSurface::new(
        DerivedSurfaceId::OperationalProjections,
        &["storage", "producer_query", "query"],
        SurfaceMedium::Response,
        &["storage/producer/pool query snapshots"],
        DerivedWrite::Rebuild,
        &[
            CanonicalSurfaceId::DeclaredInputs,
            CanonicalSurfaceId::ProducerRuntimeRecords,
            CanonicalSurfaceId::StorageRecords,
            CanonicalSurfaceId::UsageMeterObservations,
            CanonicalSurfaceId::LeaseRecords,
        ],
    ),
    DerivedSurface::new(
        DerivedSurfaceId::AggregateProjections,
        &["usage_rollup", "campaign_folds"],
        SurfaceMedium::Response,
        &["usage rollups and campaign digest/summary values"],
        DerivedWrite::Rebuild,
        &[
            CanonicalSurfaceId::AdmissionEvents,
            CanonicalSurfaceId::WitnessLedger,
            CanonicalSurfaceId::AttestationLedgers,
            CanonicalSurfaceId::CampaignRegistrations,
            CanonicalSurfaceId::CampaignReceipts,
            CanonicalSurfaceId::RepositoryReceipts,
        ],
    ),
];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn every_derived_surface_is_written_only_by_replay_or_rebuild() {
        let canonical = CANONICAL_SURFACES
            .iter()
            .map(|surface| surface.id())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            canonical.len(),
            CANONICAL_SURFACES.len(),
            "canonical surface identities must be unique"
        );

        let mut derived = BTreeSet::new();
        for surface in DERIVED_SURFACES {
            assert!(
                derived.insert(surface.id()),
                "derived surface {:?} is declared twice",
                surface.id()
            );
            assert!(
                matches!(
                    surface.write(),
                    DerivedWrite::Replay | DerivedWrite::Rebuild
                ),
                "derived surface {:?} has a direct writer",
                surface.id()
            );
            assert!(
                !surface.canonical_inputs().is_empty(),
                "derived surface {:?} names no canonical input",
                surface.id()
            );
            for input in surface.canonical_inputs() {
                assert!(
                    canonical.contains(input),
                    "derived surface {:?} names undeclared canonical input {input:?}",
                    surface.id()
                );
            }
            assert!(!surface.owners().is_empty());
            assert!(!surface.locations().is_empty());
        }
        assert_eq!(derived.len(), DERIVED_SURFACES.len());
    }

    #[test]
    fn every_canonical_surface_names_an_owner_and_location() {
        for surface in CANONICAL_SURFACES {
            assert!(!surface.owners().is_empty(), "{:?}", surface.id());
            assert!(!surface.locations().is_empty(), "{:?}", surface.id());
        }
    }

    #[test]
    fn persisted_derived_writes_are_the_closed_replay_rebuild_set() {
        let persisted = DERIVED_SURFACES
            .iter()
            .filter(|surface| surface.medium() == SurfaceMedium::Filesystem)
            .map(|surface| (surface.id(), surface.write(), surface.canonical_inputs()))
            .collect::<Vec<_>>();
        assert_eq!(
            persisted,
            vec![
                (
                    DerivedSurfaceId::WitnessGcRoots,
                    DerivedWrite::Rebuild,
                    &[CanonicalSurfaceId::WitnessLedger][..],
                ),
                (
                    DerivedSurfaceId::CampaignAssetGcRoots,
                    DerivedWrite::Rebuild,
                    &[
                        CanonicalSurfaceId::CampaignRegistrations,
                        CanonicalSurfaceId::CampaignAssets,
                    ][..],
                ),
                (
                    DerivedSurfaceId::FailureStderrProjection,
                    DerivedWrite::Replay,
                    &[
                        CanonicalSurfaceId::WitnessLedger,
                        CanonicalSurfaceId::ExecutionCaptures,
                    ][..],
                ),
            ],
            "adding persisted derived state requires declaring its canonical replay/rebuild inputs"
        );
    }

    #[test]
    fn durable_run_view_rebuilds_only_from_declared_canonical_surfaces() {
        let declared = CANONICAL_SURFACES
            .iter()
            .map(|surface| surface.id())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            DURABLE_RUN_VIEW_INPUTS,
            &[
                CanonicalSurfaceId::AdmissionEvents,
                CanonicalSurfaceId::WitnessLedger,
                CanonicalSurfaceId::LifecycleRecords,
                CanonicalSurfaceId::FlowMembership,
                CanonicalSurfaceId::FlowLineage,
                CanonicalSurfaceId::AttestationLedgers,
                CanonicalSurfaceId::ReaderState,
                CanonicalSurfaceId::ExecutionRecords,
                CanonicalSurfaceId::ExecutionCaptures,
            ]
        );
        assert!(DURABLE_RUN_VIEW_INPUTS
            .iter()
            .all(|surface| declared.contains(surface)));
        let view = DERIVED_SURFACES
            .iter()
            .find(|surface| surface.id() == DerivedSurfaceId::DurableRunView)
            .expect("durable run view is declared");
        assert_eq!(view.write(), DerivedWrite::Rebuild);
        assert_eq!(view.canonical_inputs(), DURABLE_RUN_VIEW_INPUTS);
    }
}
