# Eval findings — no coverage manifest at all

An eval that never adopted the manifest section writes a findings file like
this one: normal prose, no `<!-- eval-coverage-manifest:v1 -->` marker
anywhere. The checker must report this as a failure (findings with no
manifest are not silently equivalent to "nothing to check"), not skip it.
