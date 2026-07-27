#!/usr/bin/env bash

set -euo pipefail

scenario_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$scenario_dir/lib.sh"

SCENARIO_NAME="fleet-conformance"
require_command cargo

required_tests=(
  "daemon::tests::fleet_conformance_cooperative_yield_obeys_grace_then_witnesses_preemption"
  "daemon::tests::fleet_conformance_coordinator_switch_bumps_epoch_and_re_adopts_remote_work"
  "daemon::tests::fleet_conformance_submission_artifact_drift_creates_with_disclosure"
  "daemon::tests::fleet_conformance_submission_conflicts_fail_closed_for_every_live_shape"
  "daemon::tests::fleet_conformance_submission_created_and_attached_materialize_once"
  "daemon::tests::fleet_conformance_submission_legacy_behavior_remains_byte_and_behavior_compatible"
  "daemon::tests::fleet_conformance_submission_terminal_failure_is_memoized_and_conflicts"
  "daemon::tests::fleet_conformance_submission_terminal_pass_is_reused_without_side_effects"
  "lease::tests::fleet_conformance_fairness_ages_once_strictly_after_the_threshold"
  "lease::tests::fleet_conformance_fairness_braids_400_node_flow_siblings_and_standalone_work"
  "producers::tests::fleet_conformance_network_blip_and_true_vanish_are_distinguished_by_hysteresis"
  "wire::tests::fleet_conformance_concurrent_serving_correlates_six_awaits_and_queries"
  "wire::tests::fleet_conformance_configured_frame_limits_are_symmetric_without_negotiation"
  "wire::tests::fleet_conformance_default_frame_boundary_is_symmetric"
  "wire::tests::fleet_conformance_in_flight_window_is_64_with_fifo_overflow"
  "wire::tests::fleet_conformance_watch_and_pagination_are_exact_under_concurrency"
)

listing="$(
  cargo test --quiet \
    --manifest-path "$repo_root/Cargo.toml" \
    -p tally-core \
    --lib \
    fleet_conformance \
    -- \
    --list
)"

listed_count="$(grep -c ': test$' <<<"$listing")"
[[ "$listed_count" -eq "${#required_tests[@]}" ]] \
  || scenario_fail "expected ${#required_tests[@]} assertions, cargo listed $listed_count"

for test_name in "${required_tests[@]}"; do
  grep -Fqx -- "$test_name: test" <<<"$listing" \
    || scenario_fail "required assertion is missing: $test_name"
done

cargo test \
  --manifest-path "$repo_root/Cargo.toml" \
  -p tally-core \
  --lib \
  fleet_conformance \
  -- \
  --test-threads=1

printf 'PASS fleet-conformance: assertions=%d fault-injection=3 harness-obligations=5\n' \
  "${#required_tests[@]}"
