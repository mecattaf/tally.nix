#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path
from typing import Any


DRIVER = Path(
    os.environ.get(
        "SPEC_BUILD_DRIVER",
        Path(__file__).parents[1] / "drivers/spec_build_driver.py",
    )
)
SPEC = importlib.util.spec_from_file_location("spec_build_driver", DRIVER)
assert SPEC is not None and SPEC.loader is not None
driver = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(driver)

CHECKPOINT_REF_VECTORS = Path(
    os.environ.get(
        "SPEC_BUILD_CHECKPOINT_REF_VECTORS",
        Path(__file__).parents[1] / "test/fixtures/spec-build/checkpoint-refs.json",
    )
)


def git(*argv: str, cwd: Path | None = None, check: bool = True) -> str:
    return subprocess.run(
        ["git", *argv],
        cwd=cwd,
        check=check,
        capture_output=True,
        text=True,
    ).stdout.strip()


def attempt_receipts(root: Path, campaign: str = "fixture") -> dict[str, Any]:
    return {
        "schemaVersion": 1,
        "kind": "local-jsonl",
        "path": str(
            root
            / "campaigns"
            / "attempt-receipts"
            / campaign
            / driver.ATTEMPT_RECEIPTS_FILE
        ),
    }


def implementation_task() -> dict[str, Any]:
    return {
        "id": "task-1",
        "kind": "implementation",
        "title": "Build phase one",
        "goal": "Materialize phase one.",
        "deliveredBehaviors": ["phase one exists"],
        "readFirst": {"specSections": ["spec.md#phase-one"], "styleReferences": []},
        "acceptanceCriteria": [
            {
                "id": "phase-one",
                "description": "Phase one passes.",
                "argv": ["true"],
            }
        ],
        "dependencies": [],
        "conflictDomains": ["phase-one"],
    }


def checkpoint_task() -> dict[str, Any]:
    return {
        "id": "phase-checkpoint",
        "kind": "checkpoint",
        "title": "Validate phase one",
        "argv": ["true"],
        "runtimeMaxSec": 60,
        "dependencies": ["task-1"],
    }


class CheckpointReceiptTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.remote = self.root / "remote.git"
        self.checkout = self.root / "checkout"
        git("init", "--bare", "--initial-branch=main", str(self.remote))
        git("init", "--initial-branch=main", str(self.checkout))
        self.configure_identity(self.checkout)
        worklist = self.checkout / "specs/001-fixture/tasks.json"
        worklist.parent.mkdir(parents=True)
        worklist.write_text(
            json.dumps(
                {
                    "schemaVersion": 1,
                    "tasks": [implementation_task(), checkpoint_task()],
                }
            )
            + "\n",
            encoding="utf-8",
        )
        (self.checkout / "README.md").write_text("base\n", encoding="utf-8")
        git("add", "--all", cwd=self.checkout)
        git("commit", "-m", "fixture: base", cwd=self.checkout)
        git("remote", "add", "origin", str(self.remote), cwd=self.checkout)
        git("push", "--set-upstream", "origin", "main", cwd=self.checkout)
        self.base_rev = git("rev-parse", "HEAD", cwd=self.checkout)
        self.config = {
            "checkout": str(self.checkout),
            "baseBranch": "main",
            "remote": "origin",
            "forge": "local",
        }
        self.worklist_brief = {
            "repository": "acme/spec",
            "repositoryConfig": self.config,
            "worklist": "specs/*/tasks.json",
            "maxTasks": 2,
            "maxParallel": 1,
        }
        self.worklist = driver.action_worklist(self.worklist_brief)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    @staticmethod
    def configure_identity(checkout: Path) -> None:
        git("config", "user.name", "Checkpoint Receipt Test", cwd=checkout)
        git("config", "user.email", "checkpoint@example.invalid", cwd=checkout)

    def merged_fact(self, revision: str | None = None) -> dict[str, str]:
        return {
            "taskId": "task-1",
            "pullRequest": "local://acme/spec/task-1",
            "mergeCommit": revision or self.base_rev,
        }

    def reference(self, source: dict[str, str] | None = None) -> str:
        selected = source or self.worklist["source"]
        return driver.checkpoint_ref(
            "fixture",
            "7",
            "phase-checkpoint",
            selected["sha256"],
            selected["revision"],
        )

    def completed(
        self,
        *,
        source: dict[str, str] | None = None,
        merged_revision: str | None = None,
    ) -> list[dict[str, str]]:
        return driver.completed_checkpoint_tasks(
            driver.repo_config(self.config),
            "fixture",
            "7",
            self.worklist["tasks"],
            source or self.worklist["source"],
            [self.merged_fact(merged_revision)],
        )

    def push_receipt(self, revision: str, reference: str | None = None) -> None:
        git(
            "push",
            "origin",
            f"{revision}:{reference or self.reference()}",
            cwd=self.checkout,
        )

    def orphan_commit(self) -> str:
        git("switch", "--orphan", "forged", cwd=self.checkout)
        git("rm", "-r", "--quiet", "--ignore-unmatch", ".", cwd=self.checkout)
        (self.checkout / "forged.txt").write_text("forged\n", encoding="utf-8")
        git("add", "forged.txt", cwd=self.checkout)
        git("commit", "-m", "fixture: forged lineage", cwd=self.checkout)
        revision = git("rev-parse", "HEAD", cwd=self.checkout)
        git("switch", "main", cwd=self.checkout)
        return revision

    def clone_advancer(self) -> Path:
        advancer = self.root / "advancer"
        git("clone", str(self.remote), str(advancer))
        self.configure_identity(advancer)
        return advancer

    def checkpoint_brief(self) -> dict[str, Any]:
        return {
            "campaign": "fixture",
            "repository": "acme/spec",
            "repositoryConfig": self.config,
            "issue": {
                "number": "7",
                "url": "https://example.invalid/acme/spec/issues/7",
            },
            "task": self.worklist["tasks"][1],
            "source": self.worklist["source"],
            "workspace": {
                "taskId": "phase-checkpoint",
                "baseRev": self.base_rev,
                "branch": "main",
                "publishBranch": "tally/fixture-issue-7/phase-checkpoint",
                "worktreePath": str(self.checkout),
            },
        }

    def captured_checkpoint_brief(
        self,
        task_uuid: str,
        *,
        stdout: bytes,
        stderr: bytes,
        verdict: str = "failed",
        exit_code: int | None = 1,
    ) -> dict[str, Any]:
        capture_root = self.root / "state/capture/archive"
        current = capture_root.parent
        current.mkdir(parents=True, mode=0o700, exist_ok=True)
        stem = f"{task_uuid}.phase-checkpoint"
        (current / f"{stem}.out").write_bytes(stdout)
        (current / f"{stem}.adapter.err").write_bytes(stderr)
        brief = self.checkpoint_brief()
        brief.update(
            {
                "captureRoot": str(capture_root),
                "execution": {
                    "taskUuid": task_uuid,
                    "verdict": verdict,
                    "exitCode": exit_code,
                },
            }
        )
        return brief

    def retry_brief(
        self,
        path: Path,
        *,
        post_failure_evidence: bool,
        post_failure_stderr: bool,
    ) -> dict[str, Any]:
        return {
            "campaign": "fixture",
            "repository": "acme/spec",
            "repositoryConfig": self.config,
            "issue": {
                "number": "7",
                "url": "https://example.invalid/acme/spec/issues/7",
            },
            "taskId": "phase-checkpoint",
            "stage": "checkpoint",
            "detail": "The checkpoint command returned a failure verdict.",
            "attemptReceipts": attempt_receipts(self.root),
            "checkpointCapture": {
                "path": str(path),
                "postFailureEvidence": post_failure_evidence,
                "postFailureStderr": post_failure_stderr,
            },
        }

    def retry_reason(self, attempt: int) -> str:
        records = driver.read_attempt_receipts(
            attempt_receipts(self.root), "fixture", "7"
        )
        return next(
            record["reason"]
            for record in records
            if record["kind"] == "retry"
            and record["taskId"] == "phase-checkpoint"
            and record["attempt"] == attempt
        )

    def test_worklist_reads_remote_base_and_pushed_edit_invalidates_receipt(self) -> None:
        self.push_receipt(self.base_rev)
        original_source = self.worklist["source"]
        path = self.checkout / original_source["path"]
        local_document = json.loads(path.read_text(encoding="utf-8"))
        local_document["tasks"][1]["title"] = "uncommitted local title"
        path.write_text(json.dumps(local_document) + "\n", encoding="utf-8")

        still_remote = driver.action_worklist(self.worklist_brief)
        self.assertEqual(still_remote["source"], original_source)
        self.assertEqual(still_remote["tasks"][1]["title"], "Validate phase one")

        git("restore", str(path.relative_to(self.checkout)), cwd=self.checkout)
        advancer = self.clone_advancer()
        pushed_path = advancer / original_source["path"]
        pushed_document = json.loads(pushed_path.read_text(encoding="utf-8"))
        pushed_document["tasks"][1]["title"] = "pushed remote title"
        pushed_path.write_text(json.dumps(pushed_document) + "\n", encoding="utf-8")
        git("add", "--all", cwd=advancer)
        git("commit", "-m", "fixture: edit checkpoint", cwd=advancer)
        git("push", "origin", "main", cwd=advancer)
        pushed_revision = git("rev-parse", "HEAD", cwd=advancer)
        self.assertEqual(git("rev-parse", "HEAD", cwd=self.checkout), self.base_rev)

        updated = driver.action_worklist(self.worklist_brief)
        self.assertNotEqual(updated["source"], original_source)
        self.assertEqual(updated["source"]["revision"], pushed_revision)
        self.assertEqual(updated["tasks"][1]["title"], "pushed remote title")
        self.assertEqual(self.completed(source=updated["source"]), [])

    def test_receipt_is_invalid_after_unrelated_base_advance(self) -> None:
        self.push_receipt(self.base_rev)
        self.assertEqual([fact["taskId"] for fact in self.completed()], ["phase-checkpoint"])
        advancer = self.clone_advancer()
        (advancer / "unrelated.txt").write_text("later\n", encoding="utf-8")
        git("add", "unrelated.txt", cwd=advancer)
        git("commit", "-m", "fixture: unrelated base advance", cwd=advancer)
        git("push", "origin", "main", cwd=advancer)

        advanced = driver.action_worklist(self.worklist_brief)
        self.assertEqual(advanced["source"]["sha256"], self.worklist["source"]["sha256"])
        self.assertNotEqual(advanced["source"]["revision"], self.base_rev)
        self.assertEqual(self.completed(source=advanced["source"]), [])

    def test_forged_ref_pointing_outside_named_base_is_rejected(self) -> None:
        forged = self.orphan_commit()
        self.push_receipt(forged)
        with self.assertRaisesRegex(driver.DriverError, "does not point to its named base revision"):
            self.completed()

    def test_receipt_missing_dependency_ancestry_is_rejected(self) -> None:
        forged_dependency = self.orphan_commit()
        self.push_receipt(self.base_rev)
        with self.assertRaisesRegex(driver.DriverError, "does not contain dependency"):
            self.completed(merged_revision=forged_dependency)

    def test_annotated_receipt_tag_is_rejected(self) -> None:
        git("tag", "-a", "annotated-receipt", "-m", "not a direct commit", cwd=self.checkout)
        git(
            "push",
            "origin",
            f"annotated-receipt:{self.reference()}",
            cwd=self.checkout,
        )
        with self.assertRaisesRegex(driver.DriverError, "must point directly to a commit"):
            self.completed()

    def test_checkpoint_rejects_dirty_worktree(self) -> None:
        (self.checkout / "README.md").write_text("dirty\n", encoding="utf-8")
        with self.assertRaisesRegex(driver.DriverError, "changed tracked files"):
            driver.action_checkpoint(self.checkpoint_brief())

    def test_checkpoint_rejects_moved_head(self) -> None:
        (self.checkout / "local.txt").write_text("commit\n", encoding="utf-8")
        git("add", "local.txt", cwd=self.checkout)
        git("commit", "-m", "fixture: move local head", cwd=self.checkout)
        with self.assertRaisesRegex(driver.DriverError, "changed HEAD"):
            driver.action_checkpoint(self.checkpoint_brief())

    def test_checkpoint_rejects_changed_branch(self) -> None:
        git("switch", "-c", "other", cwd=self.checkout)
        with self.assertRaisesRegex(driver.DriverError, "changed branches"):
            driver.action_checkpoint(self.checkpoint_brief())

    def test_failed_checkpoint_attempts_persist_bounded_private_captures(self) -> None:
        stdout = b"discarded stdout\n" * 700 + b"actionable stdout tail\n"
        stderr = b"discarded stderr\n" * 700 + b"actionable stderr tail\n"
        paths: list[Path] = []
        for suffix in (1, 2):
            task_uuid = f"00000000-0000-4000-8000-{suffix:012d}"
            recorded = driver.action_checkpoint(
                self.captured_checkpoint_brief(
                    task_uuid,
                    stdout=stdout,
                    stderr=stderr,
                )
            )
            path = Path(recorded["capturePath"])
            paths.append(path)
            self.assertEqual(path.parent.parent, self.root / "state/capture/archive")
            self.assertFalse(recorded["passed"])
            self.assertIsNone(recorded["ref"])
            self.assertTrue(recorded["stdoutTruncated"])
            self.assertTrue(recorded["stderrTruncated"])
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(path.parent.stat().st_mode & 0o777, 0o700)
            capture = json.loads(path.read_text(encoding="utf-8"))
            self.assertEqual(capture["taskUuid"], task_uuid)
            self.assertEqual(capture["verdict"], "failed")
            self.assertLessEqual(
                len(capture["stdout"].encode("utf-8")),
                driver.CHECKPOINT_CAPTURE_MAX_BYTES,
            )
            self.assertLessEqual(
                len(capture["stderr"].encode("utf-8")),
                driver.CHECKPOINT_CAPTURE_MAX_BYTES,
            )
            self.assertTrue(capture["stdout"].endswith("actionable stdout tail\n"))
            self.assertTrue(capture["stderr"].endswith("actionable stderr tail\n"))

        self.assertNotEqual(paths[0], paths[1])
        self.assertTrue(paths[0].is_file(), "a later attempt must retain the first capture")

    def test_passing_checkpoint_persists_capture_and_completion_ref(self) -> None:
        recorded = driver.action_checkpoint(
            self.captured_checkpoint_brief(
                "00000000-0000-4000-8000-000000000004",
                stdout=b"checkpoint passed\n",
                stderr=b"",
                verdict="pass",
                exit_code=0,
            )
        )

        self.assertTrue(recorded["passed"])
        self.assertIsNotNone(recorded["ref"])
        capture = json.loads(
            Path(recorded["capturePath"]).read_text(encoding="utf-8")
        )
        self.assertEqual(capture["stdout"], "checkpoint passed\n")
        self.assertEqual(capture["stderr"], "")
        self.assertEqual(
            git("ls-remote", "origin", recorded["ref"], cwd=self.checkout).split()[0],
            self.base_rev,
        )

    def test_retry_withholds_stderr_by_default_and_redacts_last_ten_lines_when_enabled(
        self,
    ) -> None:
        secret = "ghp_0123456789abcdefghijklmnopqrstuvwxyz"
        lines = [f"checkpoint-line-{index:02d}" for index in range(1, 13)]
        lines[7] = f"GITHUB_TOKEN={secret}"
        stderr = ("discarded\n" * 1400 + "\n".join(lines) + "\n").encode()
        recorded = driver.action_checkpoint(
            self.captured_checkpoint_brief(
                "00000000-0000-4000-8000-000000000003",
                stdout=b"checkpoint stdout\n",
                stderr=stderr,
            )
        )
        path = Path(recorded["capturePath"])

        withheld = driver.action_retry(
            self.retry_brief(
                path,
                post_failure_evidence=False,
                post_failure_stderr=False,
            )
        )
        self.assertTrue(withheld["posted"])
        withheld_reason = self.retry_reason(1)
        self.assertIn(f"Checkpoint capture: {path}", withheld_reason)
        self.assertNotIn("Checkpoint stderr", withheld_reason)
        self.assertNotIn("checkpoint-line-12", withheld_reason)
        self.assertNotIn(secret, withheld_reason)

        published = driver.action_retry(
            self.retry_brief(
                path,
                post_failure_evidence=True,
                post_failure_stderr=True,
            )
        )
        self.assertTrue(published["posted"])
        self.assertTrue(published["redacted"])
        published_reason = self.retry_reason(2)
        self.assertIn(f"Checkpoint capture: {path}", published_reason)
        self.assertIn("Checkpoint stderr (last 10 line(s)", published_reason)
        self.assertNotIn("checkpoint-line-01", published_reason)
        self.assertNotIn("checkpoint-line-02", published_reason)
        self.assertIn("checkpoint-line-03", published_reason)
        self.assertIn("checkpoint-line-12", published_reason)
        self.assertIn("[redacted sensitive diagnosis line]", published_reason)
        self.assertNotIn(secret, published_reason)

    def test_escalation_comment_names_checkpoint_capture(self) -> None:
        path = self.root / "state/capture/archive/attempt/checkpoint.json"
        diagnosis = f"Observed the checkpoint fail.\n\nCheckpoint capture: {path}"
        reconciliation = {
            "complete": False,
            "quiescent": True,
            "escalation": None,
            "campaign": "fixture",
            "repository": "acme/spec",
            "blocked": [
                {
                    "taskId": "phase-checkpoint",
                    "blockedBy": ["phase-checkpoint"],
                }
            ],
            "diagnoses": [
                {
                    "taskId": "phase-checkpoint",
                    "attempt": 2,
                    "diagnosis": diagnosis,
                }
            ],
            "retries": [],
            "warnings": [],
        }
        original_reconcile = driver.action_reconcile
        original_digest = driver.campaign_digest
        original_summary = driver.publish_closing_summary
        driver.action_reconcile = lambda _brief: reconciliation
        driver.campaign_digest = lambda _state, _outcome: {}
        driver.publish_closing_summary = lambda *_args, **_kwargs: "local://summary"
        try:
            escalated = driver.action_escalate(
                {
                    **self.worklist_brief,
                    "campaign": "fixture",
                    "issue": {
                        "number": "7",
                        "url": "https://example.invalid/acme/spec/issues/7",
                    },
                    "attemptReceipts": attempt_receipts(self.root),
                }
            )
        finally:
            driver.action_reconcile = original_reconcile
            driver.campaign_digest = original_digest
            driver.publish_closing_summary = original_summary

        self.assertTrue(escalated["posted"])
        records = driver.read_attempt_receipts(
            attempt_receipts(self.root), "fixture", "7"
        )
        body = next(
            record["body"] for record in records if record["kind"] == "escalation"
        )
        self.assertIn("Checkpoint captures:", body)
        self.assertIn(f"- {path}", body)

    def advance_remote_base(self, name: str) -> str:
        """Land one mainline commit from outside this checkout."""
        advancer = self.clone_advancer_once()
        (advancer / f"{name}.txt").write_text(f"{name}\n", encoding="utf-8")
        git("add", "--all", cwd=advancer)
        git("commit", "-m", f"fixture: {name}", cwd=advancer)
        git("push", "origin", "main", cwd=advancer)
        return git("rev-parse", "HEAD", cwd=advancer)

    def clone_advancer_once(self) -> Path:
        advancer = self.root / "advancer"
        if not advancer.exists():
            return self.clone_advancer()
        git("pull", "--ff-only", cwd=advancer)
        return advancer

    def prepare_on(self, revision: str) -> dict[str, Any]:
        """Prepare the checkpoint lane on a revision, as a fresh pass would."""
        git("fetch", "origin", cwd=self.checkout)
        git("reset", "--hard", revision, cwd=self.checkout)
        brief = self.checkpoint_brief()
        brief["workspace"]["baseRev"] = revision
        return brief

    def test_checkpoint_records_tested_point_when_remote_base_moves_forward(self) -> None:
        """The tested point is recorded, and recording it is not progress.

        A receipt is read back under the revision the campaign reconciled, so a
        base that already moved past the tested one leaves this receipt unread
        and the checkpoint incomplete. Publishing it keeps the record truthful;
        reporting it as an advance is what let a base branch moving faster than
        the checkpoint runs keep a campaign looping for ever.
        """
        advancer = self.clone_advancer()
        (advancer / "later.txt").write_text("later\n", encoding="utf-8")
        git("add", "later.txt", cwd=advancer)
        git("commit", "-m", "fixture: advance while checkpoint runs", cwd=advancer)
        git("push", "origin", "main", cwd=advancer)

        with self.assertRaisesRegex(
            driver.DriverError, "moving faster than this checkpoint runs"
        ):
            driver.action_checkpoint(self.checkpoint_brief())

        self.assertEqual(
            git("ls-remote", "origin", self.reference(), cwd=self.checkout).split()[0],
            self.base_rev,
        )
        current = driver.action_worklist(self.worklist_brief)
        self.assertEqual(self.completed(source=current["source"]), [])

    def test_a_base_that_outruns_the_checkpoint_never_reports_progress(self) -> None:
        """#297 f1: the re-validation loop is bounded rather than endless.

        Every pass prepares on the current base and the base advances again
        before the receipt lands. No pass may return a completion the campaign
        can count, or the run reports `advanced`, posts a continuation, and
        never escalates.
        """
        tested: list[str] = []
        for index in range(3):
            prepared = self.advance_remote_base(f"pass-{index}")
            brief = self.prepare_on(prepared)
            self.advance_remote_base(f"traffic-{index}")
            with self.assertRaisesRegex(
                driver.DriverError, "moving faster than this checkpoint runs"
            ):
                driver.action_checkpoint(brief)
            tested.append(prepared)

        for revision in tested:
            reference = driver.checkpoint_ref(
                "fixture",
                "7",
                "phase-checkpoint",
                self.worklist["source"]["sha256"],
                revision,
            )
            self.assertEqual(
                git("ls-remote", "origin", reference, cwd=self.checkout).split()[0],
                revision,
                "the tested point stays recorded so nothing re-tests it",
            )
        current = driver.action_worklist(self.worklist_brief)
        self.assertEqual(
            self.completed(source=current["source"]),
            [],
            "not one of those recordings completes the checkpoint",
        )

    def test_a_checkpoint_prepared_after_the_passes_merges_is_honored(self) -> None:
        """#297 f2: a checkpoint scheduled after this pass's merges counts.

        Sharing a frontier with a mergeable implementation task used to
        guarantee waste: the checkpoint recorded against the pre-merge base and
        the pass then moved that base, so the next reconcile found nothing and
        re-ran the whole checkpoint. The flow now prepares checkpoint lanes
        after the pass's merges, and the tested revision is the one the next
        pass reconciles.
        """
        merge_revision = self.advance_remote_base("task-1-merge")

        recorded = driver.action_checkpoint(self.prepare_on(merge_revision))

        self.assertEqual(recorded["revision"], merge_revision)
        following = driver.action_worklist(self.worklist_brief)
        self.assertEqual(following["source"]["revision"], merge_revision)
        self.assertEqual(
            self.completed(source=following["source"], merged_revision=merge_revision),
            [
                {
                    "taskId": "phase-checkpoint",
                    "ref": recorded["ref"],
                    "revision": merge_revision,
                }
            ],
        )

    def test_checkpoint_rejects_diverged_remote_base(self) -> None:
        advancer = self.clone_advancer()
        git("switch", "--orphan", "replacement", cwd=advancer)
        git("rm", "-r", "--quiet", "--ignore-unmatch", ".", cwd=advancer)
        (advancer / "replacement.txt").write_text("replacement\n", encoding="utf-8")
        git("add", "replacement.txt", cwd=advancer)
        git("commit", "-m", "fixture: replace base lineage", cwd=advancer)
        git("push", "--force", "origin", "HEAD:main", cwd=advancer)

        with self.assertRaisesRegex(driver.DriverError, "remote base diverged"):
            driver.action_checkpoint(self.checkpoint_brief())

    def test_checkpoint_will_not_move_an_existing_receipt_ref(self) -> None:
        forged = self.orphan_commit()
        self.push_receipt(forged)
        with self.assertRaisesRegex(driver.DriverError, "immutable checkpoint ref"):
            driver.action_checkpoint(self.checkpoint_brief())

    def test_new_receipts_are_hidden_refs_and_never_tags(self) -> None:
        """A public target repository must not auto-fetch a campaign's ledger.

        Tags are cloned by everyone; the hidden state namespace is served only
        on request. A new receipt therefore lands beside the campaign's other
        durable state and leaves the target's tag namespace untouched.
        """
        recorded = driver.action_checkpoint(self.checkpoint_brief())
        self.assertTrue(recorded["ref"].startswith("refs/tally/spec-build/v1/"))
        self.assertNotIn("refs/tags/", recorded["ref"])
        self.assertEqual(
            git("ls-remote", "--tags", "origin", cwd=self.checkout).strip(), ""
        )
        self.assertEqual(
            git("ls-remote", "origin", recorded["ref"], cwd=self.checkout).split()[0],
            self.base_rev,
        )
        self.assertEqual(
            [fact["ref"] for fact in self.completed()], [recorded["ref"]]
        )

class CheckpointRefVectorTests(unittest.TestCase):
    """The driver half of the shared cross-language checkpoint-ref vectors.

    `crates/tally/src/cli/campaign.rs` computes the same ref layout to project
    checkpoint completion onto the master issue, and for two waves it computed
    a name the driver has never written because nothing pinned the two
    implementations to one another. Both sides now assert against this file;
    neither owns it.
    """

    def vectors(self) -> list[dict[str, Any]]:
        document = json.loads(CHECKPOINT_REF_VECTORS.read_text(encoding="utf-8"))
        self.assertEqual(document["schemaVersion"], 1)
        self.assertTrue(document["vectors"])
        return document["vectors"]

    def test_driver_ref_layout_matches_the_shared_vectors(self) -> None:
        for vector in self.vectors():
            with self.subTest(campaign=vector["campaign"], task=vector["taskId"]):
                arguments = (
                    vector["campaign"],
                    str(vector["issueNumber"]),
                    vector["taskId"],
                    vector["source"],
                    vector["baseRevision"],
                )
                self.assertEqual(driver.checkpoint_ref(*arguments), vector["ref"])

    def test_every_vector_ref_is_its_prefix_plus_the_tested_revision(self) -> None:
        # The Rust projection knows the family but not the revision, so it
        # queries `<prefix>/*`. A ref that is not exactly prefix + revision
        # would make that query match nothing.
        for vector in self.vectors():
            with self.subTest(campaign=vector["campaign"], task=vector["taskId"]):
                self.assertEqual(
                    vector["ref"],
                    f"{vector['refPrefix']}/{vector['baseRevision']}",
                )


if __name__ == "__main__":
    unittest.main()
