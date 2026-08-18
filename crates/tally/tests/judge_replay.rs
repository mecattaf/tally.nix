//! The judge-tier corpus replay harness, end to end and model-free.
//!
//! ETA.md §8.5 waits on a number: whether a smaller model may hold the
//! judge/diagnosis seat has been decided on instinct every time, and the Aug-1
//! design named the empirical alternative — replay the journaled diagnosis
//! corpus against a candidate, measure disagreement, downgrade only on the
//! numbers. These tests prove the deterministic core of that procedure against
//! a synthetic durable record and a fixture candidate that answers canned
//! verdicts. Nothing here calls a model, and nothing here runs on a timer or in
//! a gate: the live replay against a real candidate is a seam act on the
//! operator side.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[path = "support/isolated_host.rs"]
mod isolated_host;

use isolated_host::{Isolated, IsolatedHost};

const CANDIDATE: &str = "fixture-judge";
const FIXTURE_CANDIDATE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/judge-replay/fixture-candidate.sh"
);

/// Assembly is a claim about the durable record, so the record under test
/// carries one of every shape the real one does: a diagnosis whose brief and
/// typed verdict both survive, a verdict whose brief the archive no longer
/// holds, a brief whose dispatch never recorded a verdict, a legacy receipt
/// written before the verdict field existed, and a task+attempt two retained
/// briefs both claim. Only the first is replayable; the assembler must say so
/// case by case rather than reporting a corpus that quietly dropped four
/// fifths of what it walked.
#[test]
fn replay_corpus_assembly_reports_what_it_found_and_what_was_unrecoverable() {
    let temporary = tempfile::tempdir().unwrap();
    let record = DurableRecord::new(temporary.path());

    record.brief("eta", "1", "agrees-case", 1);
    record.receipt("eta", "1", "agrees-case", 1, Some("retry"));

    record.receipt("eta", "1", "hand-diagnosed", 1, Some("blocked"));

    record.brief("eta", "1", "never-recorded", 1);

    record.brief("eta", "1", "legacy-shape", 1);
    record.receipt("eta", "1", "legacy-shape", 1, None);

    record.brief("eta", "1", "twice-dispatched", 1);
    record.brief_with_salt("eta", "1", "twice-dispatched", 1, "a second arm");
    record.receipt("eta", "1", "twice-dispatched", 1, Some("retry"));

    // A second campaign in the same archive must not leak into a corpus that
    // did not ask for it.
    record.brief("epsilon", "1", "other-campaign", 1);
    record.receipt("epsilon", "1", "other-campaign", 1, Some("retry"));

    let corpus = temporary.path().join("corpus");
    let output = record.assemble(&["eta"], &corpus);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));

    let manifest = read_manifest(&corpus);
    assert_eq!(manifest["schemaVersion"], 1);
    assert_eq!(manifest["found"], 1);
    assert_eq!(manifest["unrecoverable"], 4);
    assert_eq!(manifest["cases"][0]["id"], "eta+1+agrees-case+attempt-1");
    assert_eq!(manifest["cases"][0]["recordedVerdict"], "retry");

    let reasons: Vec<(String, String)> = manifest["unrecoverableCases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| {
            (
                case["taskId"].as_str().unwrap().to_owned(),
                case["reason"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        reasons,
        vec![
            ("hand-diagnosed".to_owned(), "brief-not-retained".to_owned()),
            (
                "legacy-shape".to_owned(),
                "recorded-verdict-absent".to_owned()
            ),
            (
                "never-recorded".to_owned(),
                "verdict-not-recorded".to_owned()
            ),
            ("twice-dispatched".to_owned(), "ambiguous-brief".to_owned()),
        ]
    );

    // The corpus carries the brief bytes verbatim under the hash the manifest
    // records: a re-serialized copy would be a document no receipt was ever
    // written against.
    let brief = corpus
        .join("cases")
        .join("eta+1+agrees-case+attempt-1")
        .join("brief.json");
    let bytes = std::fs::read(&brief).unwrap();
    assert_eq!(
        manifest["cases"][0]["briefSha256"],
        format!("sha256:{:x}", Sha256::digest(&bytes))
    );
    let document: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(document["role"], "diagnosis");
    assert_eq!(document["task"]["id"], "agrees-case");

    let recorded: Value = serde_json::from_slice(
        &std::fs::read(
            corpus
                .join("cases")
                .join("eta+1+agrees-case+attempt-1")
                .join("recorded.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(recorded["verdict"], "retry");
    assert_eq!(recorded["taskId"], "agrees-case");

    // Nothing that was not replayable got a case directory.
    let cases = std::fs::read_dir(corpus.join("cases")).unwrap().count();
    assert_eq!(cases, 1);
}

/// The whole point of the file is that a seam sitting can commit it, so the
/// same corpus replayed twice against the same candidate must produce the same
/// bytes — and the numbers in it must be the ones a §8.5 ruling would be taken
/// on: agreement, verdict-class disagreement, and a candidate that cannot
/// answer the diagnosis result schema at all.
#[test]
fn replay_against_a_fixture_candidate_renders_a_byte_stable_disagreement_table() {
    let temporary = tempfile::tempdir().unwrap();
    let record = DurableRecord::new(temporary.path());

    record.brief("eta", "1", "the-candidate-agrees", 1);
    record.receipt("eta", "1", "the-candidate-agrees", 1, Some("retry"));
    record.brief("eta", "1", "the-candidate-dissents", 1);
    record.receipt("eta", "1", "the-candidate-dissents", 1, Some("retry"));
    record.brief("eta", "1", "the-candidate-is-off-schema", 1);
    record.receipt(
        "eta",
        "1",
        "the-candidate-is-off-schema",
        1,
        Some("blocked"),
    );

    let corpus = temporary.path().join("corpus");
    let assembled = record.assemble(&["eta"], &corpus);
    assert_eq!(assembled.status.code(), Some(0), "{}", stderr(&assembled));
    assert_eq!(read_manifest(&corpus)["found"], 3);

    let first = temporary.path().join("disagreement-1.txt");
    let run = record.replay(&corpus, &first);
    assert_eq!(run.status.code(), Some(0), "{}", stderr(&run));
    let table = std::fs::read_to_string(&first).unwrap();

    // The whole table, byte for byte. The three outcome classes, the totals,
    // the rate the ruling is taken on, and the failure detail are all fixed
    // bytes: a seam sitting commits this file and a later reader must be able
    // to diff it against a re-run rather than re-read a model's prose.
    assert_eq!(
        table,
        concat!(
            "# judge-tier corpus replay \u{2014} disagreement table\n",
            "schemaVersion: 1\n",
            "candidate: fixture-judge\n",
            "campaigns: eta\n",
            "corpus: 3 replayable case(s), 0 unrecoverable\n",
            "\n",
            "case                                         recorded  candidate  outcome\n",
            "-------------------------------------------  --------  ---------  ----------------------\n",
            "eta+1+the-candidate-agrees+attempt-1         retry     retry      match\n",
            "eta+1+the-candidate-dissents+attempt-1       retry     blocked    verdict-class-mismatch\n",
            "eta+1+the-candidate-is-off-schema+attempt-1  blocked   -          schema-failure\n",
            "\n",
            "totals: cases 3  match 1  verdict-class-mismatch 1  schema-failure 1\n",
            "disagreement: 2/3 = 66.67%\n",
            "\n",
            "schema failures\n",
            "- eta+1+the-candidate-is-off-schema+attempt-1: \"perhaps\" is not a declared verdict\n",
        )
    );
    // The detail says why the candidate failed the schema, and never quotes
    // the candidate's own stream back into a file that gets committed and read
    // in a terminal.
    assert!(
        !table.contains("TALLY_FINAL_MESSAGE"),
        "the table must not echo the candidate's raw stream:\n{table}"
    );

    // The table is also printed, so an operator running this at a seam reads
    // the same bytes they are about to commit.
    assert_eq!(String::from_utf8(run.stdout).unwrap(), table);

    let second = temporary.path().join("disagreement-2.txt");
    let again = record.replay(&corpus, &second);
    assert_eq!(again.status.code(), Some(0), "{}", stderr(&again));
    assert_eq!(
        std::fs::read(&first).unwrap(),
        std::fs::read(&second).unwrap(),
        "the disagreement table is not byte-stable across runs"
    );
}

/// A candidate is a name in the host catalog, not a command line. Both ways of
/// naming one that cannot hold the seat are refused before any case is
/// dispatched, so a replay never half-measures a corpus.
#[test]
fn replay_refuses_a_candidate_the_host_catalog_cannot_seat() {
    let temporary = tempfile::tempdir().unwrap();
    let record = DurableRecord::new(temporary.path());
    record.brief("eta", "1", "the-candidate-agrees", 1);
    record.receipt("eta", "1", "the-candidate-agrees", 1, Some("retry"));
    let corpus = temporary.path().join("corpus");
    record.assemble(&["eta"], &corpus);

    let out = temporary.path().join("never-written.txt");
    let unknown = record.replay_as("no-such-model", &corpus, &out);
    assert_eq!(unknown.status.code(), Some(2), "{}", stderr(&unknown));
    assert!(
        stderr(&unknown).contains("unknown candidate adapter \"no-such-model\""),
        "{}",
        stderr(&unknown)
    );

    // `shell` is the shipped adapter with no argv of its own: seating it would
    // exec the brief sentinel as a program.
    let argvless = record.replay_as("shell", &corpus, &out);
    assert_eq!(argvless.status.code(), Some(2), "{}", stderr(&argvless));
    assert!(
        stderr(&argvless).contains("declares no argv"),
        "{}",
        stderr(&argvless)
    );
    assert!(!out.exists(), "a refused replay wrote a table anyway");
}

/// Assembly must not silently merge into a directory that already holds a
/// corpus: the manifest and the case directories are one consistent artifact,
/// and a half-overwritten corpus would replay a mixture nobody assembled.
#[test]
fn replay_corpus_assembly_refuses_a_directory_that_already_holds_entries() {
    let temporary = tempfile::tempdir().unwrap();
    let record = DurableRecord::new(temporary.path());
    record.brief("eta", "1", "the-candidate-agrees", 1);
    record.receipt("eta", "1", "the-candidate-agrees", 1, Some("retry"));
    let corpus = temporary.path().join("corpus");
    assert_eq!(record.assemble(&["eta"], &corpus).status.code(), Some(0));

    let again = record.assemble(&["eta"], &corpus);
    assert_eq!(again.status.code(), Some(2), "{}", stderr(&again));
    assert!(
        stderr(&again).contains("already holds entries"),
        "{}",
        stderr(&again)
    );
}

/// A synthetic durable record with the shape the real one has: a
/// content-addressed brief archive holding every role's briefs, and one
/// append-only attempt-receipt log per campaign.
struct DurableRecord {
    root: PathBuf,
    briefs: PathBuf,
    receipts: PathBuf,
    config: PathBuf,
}

impl DurableRecord {
    fn new(root: &Path) -> Self {
        let briefs = root.join("data/briefs");
        let receipts = root.join("state/campaigns/attempt-receipts");
        std::fs::create_dir_all(&briefs).unwrap();
        std::fs::create_dir_all(&receipts).unwrap();
        let config = root.join("catalog.json");
        // The candidate is resolved in a host catalog, so the fixture candidate
        // is declared as an adapter rather than passed as a command. Its
        // finalMessage scrape is the narrator preset's, which is the shape a
        // steward-seat adapter has.
        std::fs::write(
            &config,
            serde_json::to_string_pretty(&json!({
                "adapters": {
                    CANDIDATE: {
                        "argv": [FIXTURE_CANDIDATE],
                        "scrape": {
                            "finalMessage": {
                                "mode": "regex",
                                "pattern": "^TALLY_FINAL_MESSAGE=(.*)$",
                                "stream": "stdout"
                            }
                        }
                    },
                    "shell": {}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        Self {
            root: root.to_owned(),
            briefs,
            receipts,
            config,
        }
    }

    fn brief(&self, campaign: &str, issue: &str, task: &str, attempt: usize) {
        self.brief_with_salt(campaign, issue, task, attempt, "");
    }

    /// Write one diagnosis brief in the archive's content-addressed shape. The
    /// salt makes a second, distinct document for the same task and attempt —
    /// the re-dispatch across arms that the real archive holds and that no
    /// receipt can be attributed to.
    fn brief_with_salt(&self, campaign: &str, issue: &str, task: &str, attempt: usize, salt: &str) {
        let document = json!({
            "schemaVersion": 1,
            "role": "diagnosis",
            "mission": format!("Judge failed spec-build task {task} independently.{salt}"),
            "campaign": {
                "name": campaign,
                "repository": "mecattaf/tally.nix",
                "issue": {"number": issue}
            },
            "task": {"id": task},
            // The flow renders attempt `previousDiagnoses.length + 1`, so the
            // list length is the attempt index the record can be keyed on.
            "previousDiagnoses": vec![json!({"attempt": 1}); attempt - 1]
        });
        let bytes = serde_json::to_vec(&document).unwrap();
        let digest = format!("{:x}", Sha256::digest(&bytes));
        std::fs::write(self.briefs.join(format!("{digest}.json")), &bytes).unwrap();
        // A non-diagnosis document in the same flat archive must be walked past.
        let narration =
            serde_json::to_vec(&json!({"role": "narration", "task": {"id": task}})).unwrap();
        std::fs::write(
            self.briefs
                .join(format!("{:x}.json", Sha256::digest(&narration))),
            &narration,
        )
        .unwrap();
    }

    fn receipt(
        &self,
        campaign: &str,
        issue: &str,
        task: &str,
        attempt: usize,
        verdict: Option<&str>,
    ) {
        let directory = self.receipts.join(campaign);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("attempt-receipts-v1.jsonl");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let sequence = existing.lines().filter(|line| !line.is_empty()).count() + 1;
        let mut record = json!({
            "schemaVersion": if verdict.is_some() { 2 } else { 1 },
            "sequence": sequence,
            "kind": "diagnosis",
            "campaign": campaign,
            "issueNumber": issue,
            "taskId": task,
            "attempt": attempt,
            "diagnosis": format!("recorded diagnosis for {task}"),
            "redaction": "conservative-v2"
        });
        if let Some(verdict) = verdict {
            record["verdict"] = json!(verdict);
        }
        std::fs::write(
            &path,
            format!("{existing}{}\n", serde_json::to_string(&record).unwrap()),
        )
        .unwrap();
    }

    fn assemble(&self, campaigns: &[&str], out: &Path) -> Output {
        let host = IsolatedHost::new();
        let mut command = Command::new(env!("CARGO_BIN_EXE_tally"));
        command
            .isolated(&host)
            .args(["--config", self.config.to_str().unwrap()])
            .args(["judge-replay", "assemble"]);
        for campaign in campaigns {
            command.args(["--campaign", campaign]);
        }
        command
            .args(["--briefs", self.briefs.to_str().unwrap()])
            .args(["--receipts-root", self.receipts.to_str().unwrap()])
            .args(["--out", out.to_str().unwrap()])
            .current_dir(&self.root)
            .output()
            .unwrap()
    }

    fn replay(&self, corpus: &Path, out: &Path) -> Output {
        self.replay_as(CANDIDATE, corpus, out)
    }

    fn replay_as(&self, candidate: &str, corpus: &Path, out: &Path) -> Output {
        let host = IsolatedHost::new();
        Command::new(env!("CARGO_BIN_EXE_tally"))
            .isolated(&host)
            .args(["--config", self.config.to_str().unwrap()])
            .args(["judge-replay", "run"])
            .args(["--corpus", corpus.to_str().unwrap()])
            .args(["--candidate", candidate])
            .args(["--out", out.to_str().unwrap()])
            .args(["--timeout-sec", "60"])
            .current_dir(&self.root)
            .output()
            .unwrap()
    }
}

fn read_manifest(corpus: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(corpus.join("corpus.json")).unwrap()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
