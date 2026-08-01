# Toy build specification

## Task one

Create `build/one.txt` containing exactly `one` followed by a newline and the
temporary `build/checkpoint-red` marker used by the integration fixture.

## Phase one checkpoint

After task one merges, validate the accumulated base directly: `build/one.txt`
must have its exact content and `build/checkpoint-red` must be absent. This
checkpoint intentionally fails while task four, which is not its descendant,
continues and removes the marker.

## Task two

Task two starts only after task one's pull request has merged and the automated
phase-one checkpoint has passed. It must observe `build/one.txt`, then create
`build/two.txt` containing exactly `two` followed by a newline.

## Task three

Create `build/three.txt` containing exactly `three` followed by a newline. This
task owns a disjoint path and may run alongside task one.

## Task four

Create `build/four.txt` containing exactly `four` followed by a newline. This
task conservatively shares task one's `build/one.txt` conflict domain, so it
must wait while task one is in a frontier. It does not depend on the phase-one
checkpoint, so it may remove `build/checkpoint-red` and merge while that
checkpoint is failing.

## Task five

Create `build/five.txt` only after task two merges. The failure fixture never
implements this task: it exists to prove that a blocked task blocks descendants.

## Task six

Create `build/six.txt` containing exactly `six` followed by a newline. This task
depends only on task one and must continue while task two exhausts its steered
attempts. It runs beside the failing checkpoint and task four; because task four
integrates first, this task's rebased head must be gated again.
