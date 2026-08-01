# Toy build specification

## Task one

Create `build/one.txt` containing exactly `one` followed by a newline.

## Task two

Task two starts only after task one's pull request has merged. It must observe
`build/one.txt`, then create `build/two.txt` containing exactly `two` followed by
a newline.

## Task three

Create `build/three.txt` containing exactly `three` followed by a newline. This
task owns a disjoint path and may run alongside task one.

## Task four

Create `build/four.txt` containing exactly `four` followed by a newline. This
task conservatively shares task one's `build/one.txt` conflict domain, so it
must wait while task one is in a frontier. It may run beside task two after task
one merges; its rebased head must then be gated again after task two merges.
