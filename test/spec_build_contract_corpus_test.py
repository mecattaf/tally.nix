#!/usr/bin/env python3
"""Consume the Rust-generated canonical campaign contract corpus."""

from __future__ import annotations

import copy
import importlib.util
import json
import os
import re
import unittest
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
DRIVER = Path(
    os.environ.get(
        "SPEC_BUILD_DRIVER",
        os.environ.get("SPEC_BUILD_DRIVER_SOURCE", ROOT / "drivers/spec_build_driver.py"),
    )
)
CORPUS = Path(
    os.environ.get(
        "SPEC_BUILD_CONTRACT_CORPUS",
        ROOT / "test/fixtures/spec-build/contract-corpus.json",
    )
)
RUST_CONTRACT = Path(
    os.environ.get(
        "SPEC_BUILD_CAMPAIGN_CONTRACT_SOURCE",
        ROOT / "crates/tally-core/src/campaign_contract.rs",
    )
)

SPEC = importlib.util.spec_from_file_location("spec_build_driver", DRIVER)
assert SPEC is not None and SPEC.loader is not None
driver = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(driver)


def pointer_target(document: Any, pointer: str) -> Any:
    target = document
    if not pointer:
        return target
    if not pointer.startswith("/"):
        raise AssertionError(f"invalid corpus JSON pointer {pointer!r}")
    for encoded in pointer[1:].split("/"):
        token = encoded.replace("~1", "/").replace("~0", "~")
        target = target[int(token)] if isinstance(target, list) else target[token]
    return target


def mutated(base: Any, mutation: dict[str, Any]) -> Any:
    document = copy.deepcopy(base)
    kind = mutation["kind"]
    pointer = mutation["pointer"]
    if kind in {"insert", "remove"}:
        target = pointer_target(document, pointer)
        key = mutation["key"]
        if kind == "insert":
            target[key] = copy.deepcopy(mutation["value"])
        else:
            del target[key]
        return document
    if kind != "replace":
        raise AssertionError(f"unknown corpus mutation kind {kind!r}")
    if not pointer:
        return copy.deepcopy(mutation["value"])
    parent_pointer, _, encoded_key = pointer.rpartition("/")
    parent = pointer_target(document, parent_pointer)
    key = encoded_key.replace("~1", "/").replace("~0", "~")
    if isinstance(parent, list):
        parent[int(key)] = copy.deepcopy(mutation["value"])
    else:
        parent[key] = copy.deepcopy(mutation["value"])
    return document


def rust_manifest_fields(source: str) -> set[str]:
    declaration = re.search(r"pub struct CampaignManifest\s*\{", source)
    if declaration is None:
        raise AssertionError("Rust CampaignManifest declaration is missing")
    depth = 1
    index = declaration.end()
    while index < len(source) and depth:
        if source[index] == "{":
            depth += 1
        elif source[index] == "}":
            depth -= 1
        index += 1
    if depth:
        raise AssertionError("Rust CampaignManifest declaration is unterminated")
    body = source[declaration.end() : index - 1]
    snake_fields = re.findall(r"^\s*pub\s+([a-z][a-z0-9_]*):", body, re.MULTILINE)
    if not snake_fields:
        raise AssertionError("Rust CampaignManifest has no visible fields")
    return {
        parts[0] + "".join(part[:1].upper() + part[1:] for part in parts[1:])
        for field in snake_fields
        if (parts := field.split("_"))
    }


class CampaignContractCorpusTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.corpus = json.loads(CORPUS.read_text(encoding="utf-8"))
        cls.accepted = {
            vector["name"]: vector for vector in cls.corpus.get("accepted", [])
        }

    def test_corpus_shape_and_rust_manifest_members_are_current(self) -> None:
        self.assertEqual(self.corpus["schemaVersion"], 1)
        self.assertTrue(self.accepted)
        manifest_fields = set(self.corpus["requiredKeySets"]["campaignManifest"])
        graph_fields = set(self.corpus["requiredKeySets"]["campaignGraph"])
        self.assertEqual(graph_fields, {"manifest", "tasks", "executableDigest"})
        for name, vector in self.accepted.items():
            with self.subTest(name=name):
                self.assertEqual(set(vector["manifest"]), manifest_fields)
                self.assertEqual(set(vector["graph"]), graph_fields)
        self.assertEqual(
            rust_manifest_fields(RUST_CONTRACT.read_text(encoding="utf-8")),
            manifest_fields,
            "CampaignManifest changed without regenerating contract-corpus.json",
        )

    def test_every_rust_emitted_vector_is_byte_identical_in_python(self) -> None:
        for name, vector in self.accepted.items():
            with self.subTest(name=name):
                manifest = driver.canonical_manifest(copy.deepcopy(vector["manifest"]))
                self.assertEqual(
                    driver.canonical_json(manifest), vector["manifestCanonicalJson"]
                )
                graph = driver.canonical_campaign_graph(copy.deepcopy(vector["graph"]))
                self.assertEqual(driver.canonical_json(graph), vector["graphCanonicalJson"])
                self.assertEqual(graph["executableDigest"], vector["digest"])
                self.assertEqual(
                    driver.canonical_sha256(
                        {"manifest": graph["manifest"], "tasks": graph["tasks"]}
                    ),
                    vector["digest"],
                )

    def test_every_named_rejection_is_refused_by_the_real_decoder(self) -> None:
        names: set[str] = set()
        for vector in self.corpus["rejected"]:
            name = vector["name"]
            self.assertNotIn(name, names)
            names.add(name)
            decoder_name = vector["decoder"]
            member = {"manifest": "manifest", "graph": "graph"}[decoder_name]
            decoder = {
                "manifest": driver.canonical_manifest,
                "graph": driver.canonical_campaign_graph,
            }[decoder_name]
            value = mutated(
                self.accepted[vector["base"]][member], vector["mutation"]
            )
            with self.subTest(name=name):
                with self.assertRaises(driver.DriverError):
                    decoder(value)


if __name__ == "__main__":
    unittest.main()
