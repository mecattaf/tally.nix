//! Baseline parity: the bare-vs-laned capability probe.
//!
//! The law this witnesses was recorded on 2026-08-15
//! (`AUG15-SESSION-FINDINGS.md` §2.6) and restated as this chapter's standing
//! authority in `specs/eta/evidence/sitting-c2.md`: whatever an agent can do
//! bare in a terminal — write temp files, use `/dev/shm`, compile at full
//! parallelism, see its own error output — it must be able to do inside a
//! lane; every deliberate gap is a documented containment ruling with a named
//! justification, or it is a defect.
//!
//! Chapter 1 deleted the worst two violations (the unauthored 8 GiB job cap,
//! `specs/substrate/evidence/vestige-sweep.md` V-1; the read-only diagnosis
//! jailer, V-2) but nothing *witnessed* the property, so the next unauthored
//! constraint would arrive silently and present as an agent fault again — the
//! shape that burned five hours of misattribution on 2026-08-14. This module
//! is the witness. It runs one typed capability probe twice — BARE as a direct
//! child of the CLI process, LANED as a real job unit through the daemon — and
//! diffs the two typed reports field by field. Equal reports pass. A
//! divergence passes only when the committed containment-rulings table beside
//! this file carries an entry naming the capability, the ruling, and its
//! source; any other divergence fails naming the capability and both observed
//! values.
//!
//! The reports are typed and serialized rather than scraped from prose,
//! because the whole point is that the comparison is mechanical: prose is what
//! let the 8 GiB cap live for a month without anyone being able to point at
//! the line that imposed it.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::text::sanitize_line;
use super::*;

/// The report shape both sides emit. Bumped only when a capability is added or
/// its rendering changes; a mismatched pair is refused rather than compared,
/// because a field-by-field diff across two shapes is not a diff.
const REPORT_SCHEMA_VERSION: u32 = 1;
/// The committed containment-rulings table, compiled in. It travels with the
/// binary on purpose: a table read from the filesystem at run time is a table
/// an operator can edit to make a red probe green without a commit, which is
/// exactly the silence the law forbids.
const COMMITTED_CONTAINMENT_RULINGS: &str = include_str!("containment-rulings.json");
/// Hidden verbs. The probe runs the *same binary* on both sides so that a
/// divergence is a fact about the lane and never about two different programs.
pub(super) const PARITY_PROBE_VERB: &str = "__parity-probe";
pub(super) const PARITY_FAIL_VERB: &str = "__parity-fail";
/// What `__parity-fail` writes to stderr and what the probe looks for. A
/// deliberately failing command whose stderr the process cannot read back is
/// the 2026-08-14 signature: the agent sees an empty fault and gets blamed for
/// it.
pub(super) const PARITY_FAIL_MARKER: &str = "tally baseline-parity: deliberate failure";
/// Distinct from 1 and 2 so a probe that read its own failing child correctly
/// is never confused with a probe that failed to run.
const PARITY_FAIL_EXIT: i32 = 97;
/// The file the laned side writes its report to inside the declared workspace.
const PARITY_REPORT_FILE: &str = "parity-report.json";
const PARITY_PROBE_REPO: &str = "tally/baseline-parity";
const PARITY_PROBE_BRANCH: &str = "baseline-parity-probe";
/// The laned probe does five filesystem-and-`/proc` reads and one child spawn.
/// Five minutes is the smoke genre's budget and is orders of magnitude more
/// than the work; a probe that needs longer is itself the finding.
const PARITY_RUNTIME_MAX_SEC: u64 = 5 * 60;

/// The capabilities the law names, in the order the diff walks them.
///
/// A typed enum rather than free strings: a containment-rulings row naming a
/// capability that does not exist would otherwise sit in the table looking
/// like coverage while covering nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum Capability {
    TempdirWritable,
    DevShmWritable,
    CpuParallelism,
    FailingCommandStderr,
    MemoryCeiling,
}

impl Capability {
    const fn as_str(self) -> &'static str {
        match self {
            Self::TempdirWritable => "tempdirWritable",
            Self::DevShmWritable => "devShmWritable",
            Self::CpuParallelism => "cpuParallelism",
            Self::FailingCommandStderr => "failingCommandStderr",
            Self::MemoryCeiling => "memoryCeiling",
        }
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Which side of the comparison a report came from.
///
/// Carried *in* the report rather than inferred from where it was read, so a
/// laned report compared against itself — the failure mode where the probe
/// silently proves nothing — is a refusal instead of a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub(super) enum ProbeSide {
    Bare,
    Laned,
}

impl ProbeSide {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Bare => "bare",
            Self::Laned => "laned",
        }
    }
}

/// The memory ceiling the process observes on itself, if any.
///
/// Externally tagged so "no ceiling" renders as the bare word `absent` and a
/// ceiling renders with both numbers an operator needs: the bytes and the
/// systemd property that imposed them. V-1's whole cost was that the 8 GiB
/// SIGKILL named neither.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) enum MemoryCeiling {
    Absent,
    Imposed { bytes: u64, property: String },
}

impl MemoryCeiling {
    fn rendered(&self) -> String {
        match self {
            Self::Absent => "absent".to_owned(),
            Self::Imposed { bytes, property } => format!("{bytes} bytes ({property})"),
        }
    }
}

/// One side's typed answer to "what can a process do here?".
///
/// Every field is a capability fact and nothing else. Deliberately absent:
/// the cgroup path, the temporary directory's location, the process id — facts
/// that differ between a shell and a transient unit *by construction*, and
/// whose presence would make every honest run diverge and train the operator
/// to ignore the probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CapabilityReport {
    pub(super) schema_version: u32,
    pub(super) side: ProbeSide,
    /// A file created, written, read back, and removed under the process's own
    /// temporary directory.
    pub(super) tempdir_writable: bool,
    /// The same write under `/dev/shm` — V-2's exact death (`mkdir -p
    /// /dev/shm/...` inside a landlocked diagnosis agent).
    pub(super) dev_shm_writable: bool,
    /// Parallelism as the process observes it, which is what a compiler asks
    /// for. Honours cgroup CPU quota and affinity, so a lane narrowed by
    /// either answers differently from the terminal beside it.
    pub(super) cpu_parallelism: usize,
    /// Whether a deliberately failing child's stderr came back present and
    /// readable. False is the 2026-08-14 signature: the fault is real, the
    /// error text is gone, and the agent wears it.
    pub(super) failing_command_stderr: bool,
    pub(super) memory_ceiling: MemoryCeiling,
}

/// One capability's value on one side, rendered once so the diff and the
/// failure message cannot disagree about what was observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Observation {
    pub(super) capability: Capability,
    pub(super) rendered: String,
}

impl CapabilityReport {
    /// Run the probe here, now, in this process.
    pub(super) fn observe(side: ProbeSide) -> Self {
        Self {
            schema_version: REPORT_SCHEMA_VERSION,
            side,
            tempdir_writable: directory_is_writable(&std::env::temp_dir()),
            dev_shm_writable: directory_is_writable(Path::new("/dev/shm")),
            cpu_parallelism: std::thread::available_parallelism()
                .map(std::num::NonZeroUsize::get)
                .unwrap_or(0),
            failing_command_stderr: failing_command_stderr_is_readable(),
            memory_ceiling: observe_memory_ceiling(),
        }
    }

    /// The report as the ordered capability list the diff walks. Both sides
    /// produce the same capabilities in the same order by construction, which
    /// is what makes the comparison mechanical rather than a search.
    pub(super) fn observations(&self) -> Vec<Observation> {
        let rendered = |capability: Capability, rendered: String| Observation {
            capability,
            rendered,
        };
        vec![
            rendered(
                Capability::TempdirWritable,
                self.tempdir_writable.to_string(),
            ),
            rendered(
                Capability::DevShmWritable,
                self.dev_shm_writable.to_string(),
            ),
            rendered(Capability::CpuParallelism, self.cpu_parallelism.to_string()),
            rendered(
                Capability::FailingCommandStderr,
                self.failing_command_stderr.to_string(),
            ),
            rendered(Capability::MemoryCeiling, self.memory_ceiling.rendered()),
        ]
    }

    fn parse(source: &str, origin: &str) -> Result<Self> {
        let report: Self = serde_json::from_str(source)
            .map_err(|error| invalid(format!("{origin} is not a capability report: {error}")))?;
        if report.schema_version != REPORT_SCHEMA_VERSION {
            return Err(invalid(format!(
                "{origin} declares report schema {} but this build compares schema {REPORT_SCHEMA_VERSION}",
                report.schema_version
            )));
        }
        Ok(report)
    }
}

/// One capability whose two sides disagree, carrying both values so the
/// failure message never sends anyone back to the machine to find out what was
/// actually observed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Divergence {
    pub(super) capability: Capability,
    pub(super) bare: String,
    pub(super) laned: String,
}

/// One committed row: a capability the lane deliberately narrows, the ruling
/// that narrowed it, and where the ruling is written down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ContainmentRuling {
    pub(super) capability: Capability,
    pub(super) ruling: String,
    pub(super) citation: String,
}

/// The committed table. `note` is part of the schema rather than a comment
/// because JSON has no comments and a table whose emptiness is unexplained
/// reads as an oversight instead of a position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ContainmentRulings {
    pub(super) schema_version: u32,
    pub(super) note: String,
    pub(super) rulings: Vec<ContainmentRuling>,
}

impl ContainmentRulings {
    /// The table compiled into this binary.
    pub(super) fn committed() -> Result<Self> {
        Self::parse(COMMITTED_CONTAINMENT_RULINGS, "the committed rulings table")
    }

    pub(super) fn parse(source: &str, origin: &str) -> Result<Self> {
        let table: Self = serde_json::from_str(source).map_err(|error| {
            invalid(format!(
                "{origin} is not a containment-rulings table: {error}"
            ))
        })?;
        if table.schema_version != REPORT_SCHEMA_VERSION {
            return Err(invalid(format!(
                "{origin} declares table schema {} but this build reads schema {REPORT_SCHEMA_VERSION}",
                table.schema_version
            )));
        }
        let mut seen = BTreeMap::new();
        for entry in &table.rulings {
            // A ruling with no justification and no citation is prose wearing
            // a schema: it would green a divergence while naming nothing.
            if entry.ruling.trim().is_empty() || entry.citation.trim().is_empty() {
                return Err(invalid(format!(
                    "{origin} row for {} must name both a ruling and a citation",
                    entry.capability
                )));
            }
            if seen.insert(entry.capability, ()).is_some() {
                return Err(invalid(format!(
                    "{origin} carries two rulings for {}; one capability has one ruling",
                    entry.capability
                )));
            }
        }
        Ok(table)
    }

    fn covering(&self, capability: Capability) -> Option<&ContainmentRuling> {
        self.rulings
            .iter()
            .find(|entry| entry.capability == capability)
    }
}

/// A divergence the table covers, kept beside the row that covers it so the
/// receipt carries the citation rather than only the verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ContainedDivergence {
    #[serde(flatten)]
    pub(super) divergence: Divergence,
    pub(super) ruling: String,
    pub(super) citation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ParityVerdict {
    /// Every capability agreed.
    Parity,
    /// Every divergence matched a committed ruling.
    Contained,
    /// At least one divergence is undocumented. This is the defect the law
    /// exists to make loud.
    Defect,
}

impl ParityVerdict {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Parity => "PARITY",
            Self::Contained => "CONTAINED",
            Self::Defect => "DEFECT",
        }
    }

    pub(super) const fn passed(self) -> bool {
        matches!(self, Self::Parity | Self::Contained)
    }
}

/// The whole judgement, kept as data so the receipt and the exit code read the
/// same adjudication rather than each deciding for itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Adjudication {
    pub(super) contained: Vec<ContainedDivergence>,
    pub(super) undocumented: Vec<Divergence>,
}

impl Adjudication {
    pub(super) fn verdict(&self) -> ParityVerdict {
        if !self.undocumented.is_empty() {
            ParityVerdict::Defect
        } else if self.contained.is_empty() {
            ParityVerdict::Parity
        } else {
            ParityVerdict::Contained
        }
    }
}

/// Diff two typed reports field by field and put every divergence to the
/// table.
///
/// Refuses a pair that is not one bare and one laned report: comparing a side
/// against itself is the failure mode where the probe passes while proving
/// nothing at all.
pub(super) fn adjudicate(
    bare: &CapabilityReport,
    laned: &CapabilityReport,
    rulings: &ContainmentRulings,
) -> Result<Adjudication> {
    if bare.side != ProbeSide::Bare || laned.side != ProbeSide::Laned {
        return Err(invalid(format!(
            "baseline parity compares one bare and one laned report; got {} and {}",
            bare.side.as_str(),
            laned.side.as_str()
        )));
    }
    let mut contained = Vec::new();
    let mut undocumented = Vec::new();
    for (bare_side, laned_side) in bare.observations().into_iter().zip(laned.observations()) {
        debug_assert_eq!(bare_side.capability, laned_side.capability);
        if bare_side.rendered == laned_side.rendered {
            continue;
        }
        let divergence = Divergence {
            capability: bare_side.capability,
            bare: bare_side.rendered,
            laned: laned_side.rendered,
        };
        match rulings.covering(divergence.capability) {
            Some(entry) => contained.push(ContainedDivergence {
                divergence,
                ruling: entry.ruling.clone(),
                citation: entry.citation.clone(),
            }),
            None => undocumented.push(divergence),
        }
    }
    Ok(Adjudication {
        contained,
        undocumented,
    })
}

/// The failure line. It names the capability and both observed values because
/// the incident this probe descends from cost five hours to a message that
/// named neither.
pub(super) fn undocumented_divergence_message(undocumented: &[Divergence]) -> String {
    let named = undocumented
        .iter()
        .map(|divergence| {
            format!(
                "{} (bare: {}, laned: {})",
                divergence.capability, divergence.bare, divergence.laned
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "baseline parity defect: {named} — no containment ruling covers {}. Either the lane \
         regained the capability or the gap is deliberate and belongs in \
         crates/tally/src/cli/containment-rulings.json with its justification and citation",
        if undocumented.len() == 1 {
            "it"
        } else {
            "them"
        }
    )
}

/// Create, write, read back, and remove one file. Anything less than the full
/// round trip would call a directory writable that accepts `create` and then
/// denies the write, which is a real shape under a landlocked sandbox.
fn directory_is_writable(directory: &Path) -> bool {
    let probe = directory.join(format!(
        "tally-parity-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default()
    ));
    let written = std::fs::write(&probe, b"tally baseline parity\n").is_ok();
    let read_back = written
        && std::fs::read(&probe).is_ok_and(|contents| contents == b"tally baseline parity\n");
    let _ = std::fs::remove_file(&probe);
    read_back
}

/// Run a child that fails on purpose and report whether its stderr came back.
///
/// The child is this same binary's `__parity-fail` verb rather than a shell
/// one-liner so the observation depends on nothing the lane might not have.
fn failing_command_stderr_is_readable() -> bool {
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let Ok(output) = std::process::Command::new(executable)
        .arg(PARITY_FAIL_VERB)
        .output()
    else {
        return false;
    };
    !output.status.success() && String::from_utf8_lossy(&output.stderr).contains(PARITY_FAIL_MARKER)
}

/// The effective memory ceiling on this process, read from its own cgroup
/// chain.
///
/// Walks leaf to root and takes the smallest bound, because a ceiling on an
/// ancestor slice kills exactly as dead as one on the unit — and V-1's cap was
/// stamped by the executor onto every transient unit, which is a leaf. Both
/// `memory.max` and `memory.high` are read: one kills, one throttles, and a
/// compiler that thrashes against `MemoryHigh` presents as a hung lane.
fn observe_memory_ceiling() -> MemoryCeiling {
    let Some(relative) = own_cgroup_path() else {
        return MemoryCeiling::Absent;
    };
    let mut ceiling: Option<(u64, &'static str)> = None;
    let mut current = Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
    loop {
        for (file, property) in [("memory.max", "MemoryMax"), ("memory.high", "MemoryHigh")] {
            let Ok(contents) = std::fs::read_to_string(current.join(file)) else {
                continue;
            };
            let Ok(bytes) = contents.trim().parse::<u64>() else {
                // The literal `max` — cgroup v2's spelling of "no bound".
                continue;
            };
            // `MemoryMax` wins ties: a kill and a throttle at the same number
            // are not the same fact to whoever reads the receipt.
            if ceiling.is_none_or(|(observed, _)| bytes < observed) {
                ceiling = Some((bytes, property));
            }
        }
        if current == Path::new("/sys/fs/cgroup") {
            break;
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }
    match ceiling {
        Some((bytes, property)) => MemoryCeiling::Imposed {
            bytes,
            property: property.to_owned(),
        },
        None => MemoryCeiling::Absent,
    }
}

/// This process's cgroup v2 path, the `0::` line of `/proc/self/cgroup`.
fn own_cgroup_path() -> Option<String> {
    std::fs::read_to_string("/proc/self/cgroup")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("0::").map(str::to_owned))
}

/// The hidden verb both sides run. Writes the typed report to stdout always,
/// and to `--out` as well when the caller named a path: the laned side reads
/// its report back from a file inside the declared workspace, because a
/// capture projected through the adapter's scrape configuration would make the
/// probe's reach depend on the very configuration it is measuring.
pub(super) fn run_parity_probe(args: ParityProbeArgs) -> Result<()> {
    let report = CapabilityReport::observe(args.side);
    let rendered = serde_json::to_string(&report)?;
    if let Some(path) = &args.out {
        std::fs::write(path, format!("{rendered}\n"))
            .with_context(|| format!("cannot write parity report to {}", path.display()))?;
    }
    outln!("{rendered}");
    Ok(())
}

/// The hidden verb the stderr capability probes with. It exists so the
/// observation is "a failing child's stderr came back", not "a shell existed".
pub(super) fn run_parity_fail() -> Result<()> {
    errln!("{PARITY_FAIL_MARKER}");
    // Empty message: the marker above is the whole output, and the top-level
    // printer would otherwise append a `tally: ...` line the probe would then
    // have to reason about.
    Err(exit_failure(PARITY_FAIL_EXIT, ""))
}

/// The live probe: run the capability report bare, run it again as a real job
/// unit, and put the difference to the committed table.
pub(super) async fn run_adapter_parity(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    args: AdapterParityArgs,
) -> Result<()> {
    let rulings = ContainmentRulings::committed()?;
    let config = load_client_config(config_path)?;
    if !config.adapters.contains_key(&args.adapter) {
        return Err(invalid(format!(
            "unknown adapter {:?}; configured adapters: {}",
            args.adapter,
            configured_names(config.adapters.keys())
        )));
    }
    let pool = resolve_smoke_pool(&args.adapter, args.pool.as_deref(), &config.pools)?;
    let executable = std::env::current_exe()
        .context("cannot resolve the tally executable for the baseline parity probe")?;

    // Bare first, and as a direct child of this process: that is the whole
    // meaning of "bare" — no unit, no slice, no adapter, exactly what an
    // operator's terminal would give the agent.
    let bare = probe_bare(&executable)?;

    // Connect before seeding, for the reason the smoke states beside its own
    // probe: a site created before the daemon turns out to be unreachable is a
    // directory nobody asked for and nobody reaps.
    let client = connect_rpc(socket, config_path).await?;
    let parent = match args.probe_root {
        Some(root) => root,
        None => ParityProbeSite::root_under(&match args.state_dir {
            Some(state_dir) => state_dir,
            None => default_state_dir()?,
        }),
    };
    let site = ParityProbeSite::seed(&parent)?;
    // Every failure from here has left a directory on disk; each one names it.
    let retained = |error: anyhow::Error| {
        error.context(format!(
            "baseline parity probe site retained at {}",
            site.root.display()
        ))
    };

    let report_path = site.root.join(PARITY_REPORT_FILE);
    let payload = EnqueuePayload {
        invocation: None,
        argv: Some(vec![
            executable.display().to_string(),
            PARITY_PROBE_VERB.to_owned(),
            "--side".to_owned(),
            ProbeSide::Laned.as_str().to_owned(),
            "--out".to_owned(),
            report_path.display().to_string(),
        ]),
        pools: Some(vec![pool.clone()]),
        executor: None,
        priority: Some(Priority::Medium),
        adapter: Some(args.adapter.clone()),
        cwd: Some(site.root.clone()),
        // The same declaration the commit probe makes, for the same reason:
        // workspace metadata is the only per-job mechanism that puts a
        // directory in a hardened transient unit's `ReadWritePaths=`, so the
        // laned side can write its report without any hardening tier being
        // weakened for it.
        workspace: Some(site.workspace()),
        adapter_options: None,
        gate_manifest: None,
        brief: None,
        brief_path: None,
        resume_from: None,
        source: Some(EnqueueSource::Manual),
        dedup_key: None,
        submission: None,
        orchestration: None,
        parent: None,
        evidence: vec!["exit:0".to_owned()],
        drv: None,
        evidence_class: Some(json!({
            "kind": "baseline-parity",
            "label": format!("baseline-parity:{}", args.adapter),
            "adapter": args.adapter.clone(),
        })),
        manifest_hash: None,
        consumption_estimate: None,
        runtime_max_sec: Some(PARITY_RUNTIME_MAX_SEC),
        no_enqueue: true,
        credentials: Default::default(),
        origin: None,
        caller_job_id: inherited_caller_job_id(),
        caller_job_token: inherited_caller_job_token(),
        task_uuid: None,
        related_trigger: None,
        wait: true,
    };

    let admitted = client
        .call(
            "queue.enqueue",
            Some(serde_json::to_value(payload).map_err(|error| retained(error.into()))?),
        )
        .await
        .map_err(|error| retained(error.into()))?;
    report_degraded_membership(&admitted).map_err(retained)?;
    let terminal = if admitted.get("verdict").and_then(Value::as_str).is_some() {
        admitted
    } else {
        let task_uuid = admitted
            .get("task_uuid")
            .and_then(Value::as_str)
            .filter(|task_uuid| !task_uuid.is_empty())
            .ok_or_else(|| {
                retained(invalid(
                    "queue.enqueue returned no task_uuid for the baseline parity probe",
                ))
            })?
            .to_owned();
        match await_job_with_rearm(client, socket, &task_uuid, rpc_timeout).await {
            Ok(terminal) => terminal,
            Err(error) if is_rpc_timeout(&error) => {
                // The daemon did not answer. That is a statement about the
                // daemon and never about parity, so it is a distinct exit code
                // and the site is kept rather than judged.
                return Err(retained(verdict_unavailable(format!(
                    "baseline parity could not read its laned verdict: queue.await_job did not \
                     return within {} s; the daemon may be stalled (see #431). Task {task_uuid} \
                     was admitted",
                    rpc_timeout.as_secs(),
                ))));
            }
            Err(error) => return Err(retained(error.into())),
        }
    };

    let exit_code = waited_exit_code(&terminal);
    if exit_code != 0 {
        return Err(retained(exit_failure(
            exit_code,
            format!(
                "the laned half of the baseline parity probe finished with verdict {}",
                terminal
                    .get("verdict")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            ),
        )));
    }
    let laned = std::fs::read_to_string(&report_path)
        .with_context(|| {
            format!(
                "the laned probe exited 0 but wrote no report at {}",
                report_path.display()
            )
        })
        .map_err(&retained)?;
    let laned = CapabilityReport::parse(&laned, "the laned report").map_err(&retained)?;

    let adjudication = adjudicate(&bare, &laned, &rulings).map_err(&retained)?;
    let verdict = adjudication.verdict();
    print_parity_result(
        &args.adapter,
        &pool,
        &terminal,
        &bare,
        &laned,
        &adjudication,
    )
    .map_err(&retained)?;
    if !verdict.passed() {
        // The site is the evidence for a defect, exactly as a failed commit
        // probe's repository is.
        return Err(retained(exit_failure(
            1,
            undocumented_divergence_message(&adjudication.undocumented),
        )));
    }
    site.discard();
    Ok(())
}

fn probe_bare(executable: &Path) -> Result<CapabilityReport> {
    let output = std::process::Command::new(executable)
        .arg(PARITY_PROBE_VERB)
        .args(["--side", ProbeSide::Bare.as_str()])
        .output()
        .context("cannot run the bare half of the baseline parity probe")?;
    if !output.status.success() {
        return Err(invalid(format!(
            "the bare half of the baseline parity probe failed: {}",
            sanitize_line(String::from_utf8_lossy(&output.stderr).trim())
        )));
    }
    CapabilityReport::parse(&String::from_utf8_lossy(&output.stdout), "the bare report")
}

fn print_parity_result(
    adapter: &str,
    pool: &str,
    terminal: &Value,
    bare: &CapabilityReport,
    laned: &CapabilityReport,
    adjudication: &Adjudication,
) -> Result<()> {
    outln!(
        "{}",
        serde_json::to_string(&json!({
            "schemaVersion": 1,
            "diagnostic": "baseline-parity",
            "adapter": adapter,
            "pool": pool,
            "taskUuid": terminal
                .get("task_uuid")
                .or_else(|| terminal.get("taskUuid"))
                .cloned()
                .unwrap_or(Value::Null),
            "bare": bare,
            "laned": laned,
            "contained": adjudication.contained,
            "undocumented": adjudication.undocumented,
            "verdict": adjudication.verdict().label(),
        }))?
    );
    Ok(())
}

/// The directory the laned half runs in and writes its report to.
///
/// A seeded git repository rather than a bare directory because the workspace
/// metadata that carries it into a hardened unit's `ReadWritePaths=` declares a
/// base revision, and a synthetic one would be a receipt that reads like
/// provenance without being any. It is minted under the same state-directory
/// root and with the same `probe-` prefix the adapter smoke uses, so `tally gc`
/// already reaps it: a second retention path for the same genre of throwaway is
/// how directories become permanent.
struct ParityProbeSite {
    root: PathBuf,
    base_rev: String,
}

impl ParityProbeSite {
    fn root_under(state_dir: &Path) -> PathBuf {
        state_dir.join(tally_core::retention::ADAPTER_SMOKE_DIRECTORY)
    }

    fn seed(parent: &Path) -> Result<Self> {
        if !parent.is_absolute() {
            return Err(invalid(format!(
                "baseline parity probe root must be an absolute path: {}",
                parent.display()
            )));
        }
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "cannot create baseline parity probe root {}",
                parent.display()
            )
        })?;
        let root = parent.join(format!(
            "{}parity-{}-{}",
            tally_core::retention::ADAPTER_SMOKE_PROBE_PREFIX,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|since| since.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir(&root).with_context(|| {
            format!(
                "cannot create baseline parity probe site {}",
                root.display()
            )
        })?;
        std::fs::write(root.join("README.md"), "tally baseline parity probe site\n")
            .context("cannot seed the baseline parity probe site")?;
        for argv in [
            vec!["init", "--quiet", "--initial-branch", PARITY_PROBE_BRANCH],
            vec!["config", "user.email", "baseline-parity@localhost"],
            vec!["config", "user.name", "tally baseline parity"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["add", "--all"],
            vec![
                "commit",
                "--quiet",
                "--message",
                "baseline parity probe site",
            ],
        ] {
            git(&root, &argv)?;
        }
        let base_rev = git(&root, &["rev-parse", "HEAD"])?;
        Ok(Self { root, base_rev })
    }

    fn workspace(&self) -> WorkspaceMetadata {
        WorkspaceMetadata {
            repo: PARITY_PROBE_REPO.to_owned(),
            base_rev: self.base_rev.clone(),
            branch: PARITY_PROBE_BRANCH.to_owned(),
            worktree_path: self.root.clone(),
        }
    }

    /// Only a passing probe's site is removed; a defect's site is the evidence.
    fn discard(&self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Crate-local, resolved through `CARGO_MANIFEST_DIR`: the nix build runs
    /// from a filtered source tree, so a fixture reached by a relative path
    /// from the process's working directory is a fixture the packaged build
    /// cannot find.
    fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/parity")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
    }

    fn report(name: &str) -> CapabilityReport {
        CapabilityReport::parse(&fixture(name), name).unwrap()
    }

    fn rulings(name: &str) -> ContainmentRulings {
        ContainmentRulings::parse(&fixture(name), name).unwrap()
    }

    #[test]
    fn equal_reports_are_at_baseline_parity() {
        let bare = report("bare-report.json");
        let laned = report("laned-report-at-parity.json");
        let adjudication = adjudicate(&bare, &laned, &ContainmentRulings::committed().unwrap())
            .expect("a bare and a laned report are a comparable pair");

        assert_eq!(adjudication.verdict(), ParityVerdict::Parity);
        assert!(adjudication.verdict().passed());
        assert!(adjudication.undocumented.is_empty());
        assert!(adjudication.contained.is_empty());
    }

    #[test]
    fn an_untabled_divergence_fails_parity_naming_the_capability_and_both_values() {
        let bare = report("bare-report.json");
        let laned = report("laned-report-untabled.json");
        let adjudication =
            adjudicate(&bare, &laned, &ContainmentRulings::committed().unwrap()).unwrap();

        assert_eq!(adjudication.verdict(), ParityVerdict::Defect);
        assert!(!adjudication.verdict().passed());
        assert_eq!(
            adjudication.undocumented,
            vec![Divergence {
                capability: Capability::DevShmWritable,
                bare: "true".to_owned(),
                laned: "false".to_owned(),
            }]
        );

        // The message is the whole point: the 2026-08-14 misattribution cost
        // five hours to a failure that named neither the capability nor the
        // values, so a defect that only says "parity failed" is the same
        // defect.
        let message = undocumented_divergence_message(&adjudication.undocumented);
        assert!(message.contains("devShmWritable"), "{message}");
        assert!(message.contains("bare: true"), "{message}");
        assert!(message.contains("laned: false"), "{message}");
        assert!(
            message.contains("containment-rulings.json"),
            "the defect must name where a deliberate gap would be recorded: {message}"
        );
    }

    #[test]
    fn a_tabled_divergence_passes_parity_only_by_citing_its_ruling() {
        let bare = report("bare-report.json");
        let laned = report("laned-report-tabled.json");
        let table = rulings("containment-rulings-tabled.json");
        let adjudication = adjudicate(&bare, &laned, &table).unwrap();

        assert_eq!(adjudication.verdict(), ParityVerdict::Contained);
        assert!(adjudication.verdict().passed());
        assert!(adjudication.undocumented.is_empty());
        let contained = adjudication
            .contained
            .first()
            .expect("the tabled divergence is carried, not swallowed");
        assert_eq!(contained.divergence.capability, Capability::MemoryCeiling);
        assert_eq!(contained.divergence.bare, "absent");
        assert_eq!(contained.divergence.laned, "8589934592 bytes (MemoryMax)");
        // Passing without carrying the citation forward would make the receipt
        // say "contained" and leave the reader no way to find out by what.
        assert_eq!(contained.ruling, table.rulings[0].ruling);
        assert_eq!(contained.citation, table.rulings[0].citation);
        assert!(contained.citation.contains("vestige-sweep.md"));
    }

    #[test]
    fn the_same_divergence_is_a_parity_defect_against_the_committed_table() {
        // The gate is the table, not the capability: the identical report pair
        // that passes above fails here because the committed table carries no
        // such ruling. This is what keeps the empty table meaningful.
        let bare = report("bare-report.json");
        let laned = report("laned-report-tabled.json");
        let adjudication =
            adjudicate(&bare, &laned, &ContainmentRulings::committed().unwrap()).unwrap();

        assert_eq!(adjudication.verdict(), ParityVerdict::Defect);
        let message = undocumented_divergence_message(&adjudication.undocumented);
        assert!(message.contains("memoryCeiling"), "{message}");
        assert!(message.contains("bare: absent"), "{message}");
        assert!(
            message.contains("8589934592 bytes (MemoryMax)"),
            "{message}"
        );
    }

    #[test]
    fn the_committed_containment_rulings_table_binds_named_parity_capabilities() {
        let table = ContainmentRulings::committed().unwrap();
        assert_eq!(table.schema_version, REPORT_SCHEMA_VERSION);
        assert!(
            !table.note.trim().is_empty(),
            "an empty table whose emptiness is unexplained reads as an oversight"
        );
        // Every row, now and later, must justify and cite. `parse` enforces it;
        // this states it as the table's own contract.
        for entry in &table.rulings {
            assert!(!entry.ruling.trim().is_empty());
            assert!(!entry.citation.trim().is_empty());
        }
    }

    #[test]
    fn a_parity_rulings_row_must_justify_cite_and_name_a_real_capability() {
        let unknown = r#"{"schemaVersion":1,"note":"x","rulings":[
            {"capability":"networkReachable","ruling":"r","citation":"c"}]}"#;
        assert!(ContainmentRulings::parse(unknown, "fixture").is_err());

        let uncited = r#"{"schemaVersion":1,"note":"x","rulings":[
            {"capability":"devShmWritable","ruling":"r","citation":"  "}]}"#;
        assert!(ContainmentRulings::parse(uncited, "fixture").is_err());

        let doubled = r#"{"schemaVersion":1,"note":"x","rulings":[
            {"capability":"devShmWritable","ruling":"r","citation":"c"},
            {"capability":"devShmWritable","ruling":"other","citation":"c"}]}"#;
        assert!(ContainmentRulings::parse(doubled, "fixture").is_err());
    }

    #[test]
    fn parity_refuses_a_pair_that_is_not_one_bare_and_one_laned_report() {
        // A laned report compared against itself diverges nowhere and would
        // pass while proving nothing at all.
        let laned = report("laned-report-untabled.json");
        let table = ContainmentRulings::committed().unwrap();
        assert!(adjudicate(&laned, &laned, &table).is_err());

        let bare = report("bare-report.json");
        assert!(adjudicate(&bare, &bare, &table).is_err());
        assert!(adjudicate(&laned, &bare, &table).is_err());
    }

    #[test]
    fn a_parity_report_covers_every_capability_the_law_names() {
        let observed = report("bare-report.json")
            .observations()
            .into_iter()
            .map(|observation| observation.capability)
            .collect::<Vec<_>>();
        assert_eq!(
            observed,
            vec![
                Capability::TempdirWritable,
                Capability::DevShmWritable,
                Capability::CpuParallelism,
                Capability::FailingCommandStderr,
                Capability::MemoryCeiling,
            ],
            "the law names temp files, /dev/shm, parallelism, the process's own \
             error output, and the memory ceiling"
        );
    }

    #[test]
    fn a_parity_report_round_trips_through_its_serialized_form() {
        // The comparison is mechanical only if the wire shape is the type: a
        // field that serializes and does not deserialize would silently drop
        // out of the diff.
        for name in [
            "bare-report.json",
            "laned-report-at-parity.json",
            "laned-report-untabled.json",
            "laned-report-tabled.json",
        ] {
            let parsed = report(name);
            let rendered = serde_json::to_string(&parsed).unwrap();
            assert_eq!(CapabilityReport::parse(&rendered, name).unwrap(), parsed);
        }
    }

    #[test]
    fn a_parity_report_from_another_schema_is_refused_not_compared() {
        let future = r#"{"schemaVersion":2,"side":"bare","tempdirWritable":true,
            "devShmWritable":true,"cpuParallelism":1,"failingCommandStderr":true,
            "memoryCeiling":"absent"}"#;
        assert!(CapabilityReport::parse(future, "fixture").is_err());

        // An unknown field is a capability this build does not compare, which
        // is a silent hole in a mechanical diff.
        let extra = r#"{"schemaVersion":1,"side":"bare","tempdirWritable":true,
            "devShmWritable":true,"cpuParallelism":1,"failingCommandStderr":true,
            "memoryCeiling":"absent","networkReachable":true}"#;
        assert!(CapabilityReport::parse(extra, "fixture").is_err());
    }

    #[test]
    fn the_parity_writability_probe_is_a_write_then_read_back() {
        let directory = tempfile::tempdir().unwrap();
        assert!(directory_is_writable(directory.path()));
        // And it leaves nothing behind: a probe that littered would eventually
        // be the thing filling the disk it measures.
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);

        assert!(!directory_is_writable(
            &directory.path().join("no-such-directory")
        ));
    }
}
