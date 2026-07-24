# Style-transfer brief: attic + Trustix

Source repos (shallow clones, read at commit HEAD of the clone date):
- `~/Downloads/attic` = https://github.com/zhaofengli/attic
- `~/Downloads/trustix` = https://github.com/nix-community/trustix

Scope: attic is evaluated as the candidate cross-host artifact data plane for
tally.nix job-evidence artifacts (Section 1-2). Trustix is evaluated purely as
a design reference for a *future* witness-ledger v2 (Section 3) — nothing in
Section 3 is proposed for the current build.

---

## 1. attic — client/server architecture, push/pull, auth, retention

### 1.1 Push path (client → server)

Entry point: `~/Downloads/attic/client/src/push.rs`.

- `Pusher` (line 70) is the low-level primitive: caller supplies already-computed
  `ValidPathInfo`s (closure + metadata known ahead of time), it fans them out to
  `config.num_workers` async workers over an `async_channel`.
- `PushSession` (line 101) wraps a `Pusher` for the case where store paths arrive
  as a stream (e.g. a build-output watcher) with no precomputed push plan. It
  batches: flush after 2s of inactivity or 10s total (`push.rs:99-100,313,331`),
  memoizing which store-path hashes are already known/pushed
  (`known_paths_mutex`, `push.rs:269,338-361`) so repeated closure computation
  is avoided.
- `PushPlan::plan` (`push.rs:401-493`) is the actual planning logic:
  1. `store.compute_fs_closure_multi(roots, ...)` — full transitive closure
     unless `no_closure` is set.
  2. Query `store.query_path_info` for every path in the closure (parallel via
     `join_all`).
  3. Filter out paths whose signatures already match an `upstream_cache_key_names`
     entry in the cache's config (skip re-pushing things that came from
     cache.nixos.org, etc.) — `push.rs:449-466`.
  4. Call the server's `get_missing_paths` API with the remaining path hashes
     and only push what's actually missing (`push.rs:479-484`).
- `upload_path()` (`push.rs:497-614`) does the actual per-path upload: builds an
  `UploadPathNarInfo` (narinfo-equivalent JSON: store path, references, sigs, CA,
  nar hash/size), streams the NAR via `store.nar_from_path`, and PUTs it through
  `ApiClient::upload_path`, wrapped with an indicatif progress bar.

### 1.2 Upload handling (server side)

Entry point: `~/Downloads/attic/server/src/api/v1/upload_path.rs`.

- The narinfo-equivalent metadata (`UploadPathNarInfo`) is sent either as an
  in-band **preamble** (size given by the `X-Attic-Nar-Info-Preamble-Size`
  header, read off the front of the PUT body, capped by
  `config.max_nar_info_size`) or as the `X-Attic-Nar-Info` header directly
  (`upload_path.rs:96-138`).
- **Global dedup by NAR hash**: before touching storage, the server tries to
  lock an existing `nar` row with the same `nar_hash`
  (`database.find_and_lock_nar`, line 153). If found and all its chunks are
  present, the upload short-circuits to `upload_path_dedup` (line 183) — it
  only needs to (optionally) verify proof-of-possession by re-hashing the
  streamed body, then insert an `object` row (cache → NAR grant). No bytes are
  written to storage again.
- **New NAR, chunked vs unchunked** (`upload_path_new`, line 242): controlled by
  `state.config.chunking.nar_size_threshold`. Below threshold (or threshold =
  0), the whole NAR is one chunk (`upload_path_new_unchunked`, line 449).
  Above threshold, the NAR is split into content-defined chunks
  (`upload_path_new_chunked`, line 260) using
  `attic::chunking::chunk_stream` (FastCDC, see below), each chunk uploaded
  concurrently (`CONCURRENT_CHUNK_UPLOADS = 10`, line 56) and each individually
  deduplicated in `upload_chunk()` (line 545) against existing `chunk` rows by
  chunk hash — the same "try to reuse, else upload+compress+hash-verify+store"
  pattern as the NAR level, just one layer down.
- Per-chunk storage write: compress (brotli/zstd/xz per `CompressionConfig`),
  stream to the configured `StorageBackend` under a random `{uuid}.chunk` key
  (line 590-651), verify the actual streamed hash/size matches the claimed one
  (defense against a malicious/broken claimed hash — `upload_path.rs:653-662`),
  then flip the DB row from `PendingUpload` to `Valid`.
- Everything (NAR row transition + chunk-ref rows + object grant) commits in one
  DB transaction at the end (`upload_path.rs:402-433`), with a `Finally`-based
  cleanup guard (`crate::util::Finally`) that deletes the half-created NAR/chunk
  row and uploaded file if anything fails before commit.

### 1.3 Chunking (content-defined, FastCDC)

`~/Downloads/attic/attic/src/chunking/mod.rs` (`chunk_stream`, line ~17): thin
wrapper over the `fastcdc` crate (ronomon variant) over an `AsyncRead`, yielding
`Bytes` chunks. Parameters (`min_size`, `avg_size`, `max_size`) are configured
server-side under `[chunking]` in `atticd`'s TOML
(`server/src/config.rs:247-269`, documented with defaults in
`~/Downloads/attic/book/src/admin-guide/chunking.md`):

```
nar-size-threshold = 131072   # 128 KiB — below this, NAR is stored as one chunk
min-size           = 65536    # 64 KiB
avg-size           = 131072   # 128 KiB
max-size           = 262144   # 256 KiB
```

The book explicitly warns: changing these values changes cutpoints, so
existing chunks stop deduplicating against newly-uploaded NARs — the
dedup ratio degrades until the store "re-settles" on the new boundaries.
Relevant if tally ever changes its own artifact chunking-adjacent config.

### 1.4 Pull path (server → client / Nix)

Entry point: `~/Downloads/attic/server/src/api/binary_cache.rs`. This implements
the plain Nix HTTP Binary Cache protocol
(https://github.com/fzakaria/nix-http-binary-cache-api-spec), i.e. attic caches
are consumed by ordinary Nix substituters, not by a bespoke protocol:

- `GET /:cache/nix-cache-info` — `StoreDir`, `Priority`, `WantMassQuery`
  (`binary_cache.rs:82-105`).
- `GET /:cache/{storePathHash}.narinfo` — looks up the `object`+`nar` rows,
  signs the narinfo if unsigned (cache-level signing keypair), returns it
  (`binary_cache.rs:115-160`).
- `GET /:cache/nar/{storePathHash}.nar` — single-chunk case: either redirects to
  a pre-signed storage URL (`Download::Url`) or streams the file directly
  (`Download::AsyncRead`). Multi-chunk case: `attic::io::merge_chunks`
  reassembles the NAR from N chunks with a small prefetch window (currently
  hardcoded to 2, `binary_cache.rs:263`, marked TODO to make configurable) —
  `binary_cache.rs:170-278`.

### 1.5 Token / auth model

`~/Downloads/attic/token/src/lib.rs`. Stateless JWT-based access control (no
server-side session/user table):

- Custom claim namespace `https://jwt.attic.rs/v1` (`CLAIM_NAMESPACE`, line
  112) containing a `caches` map of **cache-name-pattern → permission bits**:
  `r` (pull), `w` (push), `d` (delete), `cc` (create-cache), `cr`
  (configure-cache), `cq` (configure-cache-retention), `cd` (destroy-cache) —
  struct `CachePermission`, lines 154-206. Pattern keys can include wildcards
  (`team-*`), matched via `CacheNamePattern` in `attic/src/cache.rs`.
- Signature schemes: `HS256` (shared secret) or `RS256` (keypair, or
  verify-only with just the public key) — `SignatureType` enum, `lib.rs:240-244`.
  Server-side config in `server/src/config.rs:157-201`
  (`[jwt.signing]` block: `token-hs256-secret-base64` /
  `token-rs256-secret-base64` / `token-rs256-pubkey-base64`, all base64-encoded).
- Server can optionally bind/require a specific `iss` and a set of allowed
  `aud` values (`JWTConfig.token_bound_issuer` /
  `token_bound_audiences`, `config.rs:145-155`), checked in
  `Token::from_jwt`'s `VerificationOptions` (`token/src/lib.rs:246-290`).
- Transport: normal `Authorization: Bearer <jwt>` header, or as the **password**
  in HTTP Basic Auth (username ignored) — documented at the top of
  `~/Downloads/attic/server/src/access/http.rs` (module doc, lines 28-39), which
  is exactly how Nix's `~/.config/nix/netrc` mechanism supplies it:
  `machine attic.server.tld password eyJhb...`. Client-side netrc writing is in
  `~/Downloads/attic/client/src/nix_netrc.rs`.
- Discovery semantics are deliberately fuzzy: if a JWT grants *any* permission
  on a cache name, the bearer can "discover" it (get `NotFound`/`Forbidden` as
  appropriate); otherwise every request gets a generic 401 regardless of
  whether the cache exists (`token/src/lib.rs:20-26`, `403-418`) — avoids
  leaking cache existence to unauthenticated/unrelated tokens.
- Object-storage-side auth for uploads/downloads is a separate concern, handled
  per-backend (S3 presigned URLs / credentials in
  `~/Downloads/attic/server/src/storage/s3.rs`); the JWT only gates the attic
  API, not the underlying bucket.

### 1.6 Cache-level retention / GC settings

- Per-cache override: `retention_period: Option<i32>` (seconds) on the `cache`
  DB row (`~/Downloads/attic/server/src/database/entity/cache.rs:50-51`),
  settable via `attic cache configure --retention-period <duration>` /
  `--reset-retention-period` (`client/src/command/cache.rs:128-137,213-228`),
  gated by the `cq` (`configure_cache_retention`) JWT permission
  (`server/src/api/v1/cache_config.rs:109-119`).
- Global default: `[garbage-collection] default-retention-period` in
  `atticd`'s config, default `0` = disabled
  (`server/src/config.rs:314-324,468-473`). `0` on a per-cache override also
  means "use global default", not "retain forever" — semantics decoded via
  `RetentionPeriodConfig::{Period(u32), Global}` in
  `~/Downloads/attic/attic/src/api/v1/cache_config.rs:95-133`.
- Automatic GC cadence: `[garbage-collection] interval`, default `43200s` (12h)
  (`server/src/config.rs:557-559`); `0` disables the background loop but a
  manual one-shot pass remains available via `atticd --mode
  garbage-collector-once`.

### 1.7 Server-side GC model (mechanically, what actually gets deleted)

`~/Downloads/attic/server/src/gc.rs`. Three independent passes per cycle
(`run_garbage_collection_once`, lines 69-78):

1. **Time-based object reap** (`run_time_based_garbage_collection`,
   lines 80-139): for every cache with a non-zero effective retention period,
   delete `object` rows where **both** `created_at` and (`last_accessed_at` is
   null or) `last_accessed_at` are older than `now - retention_period`. This is
   the only place retention period actually has teeth — it deletes the
   cache→NAR *grant* row, not the NAR itself.
2. **Orphan NAR reap** (`run_reap_orphan_nars`, lines 141-170): a `nar` row
   with zero remaining `object` references (left join, `holders_count = 0`) and
   state `Valid` gets hard-deleted. (`holders_count` is the dedup refcount:
   incremented every time a new cache reuses the same NAR hash.)
3. **Orphan chunk reap** (`run_reap_orphan_chunks`, lines 172-275): a `chunk`
   row with no remaining `chunkref` rows and `holders_count = 0` transitions to
   `Deleted` state first (so a NAR reassembly in flight doesn't 404 mid-stream),
   then the underlying storage object is deleted (bounded concurrency,
   semaphore of 20, tolerant of individual delete failures — a chunk stuck in
   `Deleted` state is retried on a later pass), then the DB row is removed.

So attic's retention is purely **time + reachability from a cache's object
table** — there is no separate "GC root" primitive inside attic itself; a
NAR/chunk survives exactly as long as some `cache` still has an `object` row
pointing at it and that row hasn't aged out. This matters for Section 2: attic
does not have (and does not need) a Nix-style GC-roots concept, because its
"root set" is just "rows other tables point at." Any GC-roots-as-retention
design for tally is therefore a **Nix-store-level** concern, orthogonal to
attic's own DB-driven reaping — see below.

### 1.8 Getting a non-Nix-build artifact into the store (so attic can carry it)

attic pushes/pulls **Nix store paths** — `ValidPathInfo` (`push.rs:124-136`)
requires a real store path with a NAR hash, references, and (optionally)
signatures; there is no "upload arbitrary blob" API. To let attic carry a job's
evidence artifact (a plain file tally produced, not a build output), the file
must first become a real store path. The relevant primitive, cited from the
Nix manual (nix 2.24 CLI reference, verified against
`nix.dev/manual/nix/2.24/command-ref/new-cli/`):

- **`nix store add [--mode nar|flat|text] [--hash-algo md5|sha1|sha256|sha512]
  [--name NAME] PATH`** — copies a file or directory into the store and prints
  the resulting store path. Content-addressed: the path is derived from a hash
  of the input, computed either by serializing as a NAR (`nar`, the default —
  used for directories/build-like trees), by hashing the raw file bytes
  directly (`flat` — the right mode for a single evidence file), or `text`
  mode (intended for derivations / `builtins.toFile`-style content). `--name`
  overrides the human-readable name component (default: the input's basename).
  Example from the manual: `nix store add ./dir` →
  `/nix/store/6pmjx56pm94n66n4qw1nff0y1crm8nqg-dir`.
- **`nix store add-file`** and **`nix store add-path`** are deprecated aliases:
  `add-file` ≡ `nix store add --mode flat` (single file, no NAR wrapping);
  `add-path` ≡ `nix store add` (nar mode, the general case). For a single
  evidence-artifact file, `--mode flat` is the semantically-correct choice
  (matches `add-file`'s old default) since NAR-wrapping a single file just
  adds indirection with no benefit.
- **Explicit caveat directly from the manual**: *"the resulting path is not
  registered as a garbage collector root"* — `nix store add` by itself leaves
  the new path collectible on the very next GC sweep. It must be rooted
  (Section 2) before anything else can safely reference it, including before
  handing it to attic's push path (attic's `PushPlan` calls
  `store.query_path_info` / closure computation against the real store; if the
  path is GC'd between `add` and `push`, the push fails).

Once the artifact is a rooted store path, it is a completely ordinary
`ValidPathInfo` from attic's point of view: `store_path_hash`, `nar_hash`,
`nar_size`, `references` (likely empty for a standalone evidence file), no
`ca`/`sigs` unless tally signs it — same code path as any build output. No
attic-side changes are needed to carry it.

---

## 2. GC-roots-as-retention — mechanics (Nix-core, not attic)

Cited from the Nix manual (`nix.dev/manual/nix/2.24/command-ref/nix-store/gc`
and the `--add-root` description under the `nix-store` realise/query
operations):

- **What a root is**: `nix-store --gc` deletes every store path not reachable
  via filesystem references from a configured "root set." Roots live under
  `/nix/var/nix/gcroots/` (or the equivalent under a non-default Nix store
  prefix).
- **`--add-root PATH`** (paired with `-r`/`--realise` or a build): *"causes the
  result of a realisation to be registered as a root of the garbage
  collector. PATH will be created as a symlink to the resulting store path."*
  Crucially this is **not** a single symlink — it's a two-level **indirect
  root**:
  1. `PATH` (caller-chosen, e.g. `/var/lib/tally/gcroots/job-<id>/result`) is
     created as a symlink → the real store path.
  2. A second, auto-named symlink is created in `/nix/var/nix/gcroots/auto/`
     that points back at `PATH` (not at the store path directly).
- **Self-cleaning property**: if the caller later deletes `PATH` (e.g. tally
  removes a job's evidence root when its retention window ends), the
  `gcroots/auto/` entry becomes a dangling symlink. `nix-store --gc` treats
  dangling auto-roots as absent and simply skips them — **no separate
  "unregister the root" call is needed**; deleting the indirect-root symlink
  *is* the deregistration.
  This is exactly the mechanism to build per-job retention on: "job evidence
  artifact retained" == "the job's gcroot symlink still exists on disk";
  expiring a job's retention is just `rm` on that one symlink, and the next GC
  pass reclaims the NAR/chunks (on attic's side, the corresponding cache
  `object` row would need to be deleted too, or covered by attic's own
  time-based reap in parallel — the two retention mechanisms, Nix-side GC roots
  and attic-side `retention_period`, are independent and would need to be kept
  in sync deliberately, not assumed to coincide).
- **Hard limitation, quoted directly**: *"it is not possible to move or rename
  GC roots, since the symlink in the auto directory will still point to the old
  location."* Implication for a per-job design: the indirect-root path chosen
  for a job (e.g. keyed by job UUID/attempt) must be treated as immutable for
  the life of that root; renaming/reorganizing the gcroots directory tree after
  the fact silently breaks rooting for anything created before the move.
- **Direct vs indirect**: a plain symlink placed straight in
  `/nix/var/nix/gcroots/` (a "direct root") is also honored, but has no
  self-cleaning behavior — removing it requires deleting the actual gcroots
  entry, not some external path. The indirect form (`--add-root` from outside
  the gcroots directory) is what you want for per-job roots that live inside
  tally's own state directory rather than inside Nix's.
- Global knobs that interact with this: `keep-outputs` and `keep-derivations`
  in `nix.conf` also expand the effective root set (keeping a build's
  dependencies/derivation alongside its output); not evaluated further here —
  flagged as **unknown/out of scope** whether tally would want either enabled,
  since it changes blast radius beyond just evidence artifacts.

**What's unknown / not verified**: the exact symlink-naming scheme used inside
`gcroots/auto/` (hash-of-target vs sequential) was described only by example in
the fetched manual page and not independently confirmed against Nix source;
treat as "some collision-resistant derived name," not a byte-exact format, until
checked against `nix-store` source directly if it matters for tally's design.

---

## 3. Trustix — FOR A LATER WITNESS-V2 DESIGN SESSION. DO NOT IMPLEMENT NOW.

One page, reference-only. Trustix (https://github.com/nix-community/trustix) is
a build-transparency system: independent builders publish (input-hash →
output-hash) facts into per-builder append-only Merkle logs, and clients
compare across logs to detect non-reproducibility or compromise
(`~/Downloads/trustix/packages/trustix-doc/src/about.md`).

**Merkle log format** — `~/Downloads/trustix/packages/trustix/internal/log/log.go`
(`VerifiableLog`, RFC-6962-style append-only "history tree," not a Bitcoin-style
Merkle tree — no rebalancing, provably append-only):
- `Append`/`AppendKV` (lines 68-102) add a leaf, hashing with a
  domain-prefixed `leafDigest`/`leafDigestKV` (`internal/log/leaf.go`).
- `Root()` (lines 34-66) and `AuditProof`/`ConsistencyProof` (lines 138-233)
  implement standard transparency-log math: inclusion proof for one leaf
  against a given tree size, and consistency proof between two tree sizes
  (proves the log was only ever appended to, never rewritten).
- A second structure, a **sparse Merkle tree** (`smt` package, imported in
  `internal/sth/sth.go`), maps arbitrary keys → values for O(log n) membership
  proofs (`SparseCompactMerkleProof` in the proto, used for "does this input
  hash have a recorded output" lookups) — this is the "map" alongside the
  append-only "log."
- **Signed head** (`SignHead`/`VerifyLogHeadSig`,
  `internal/sth/sth.go:26-82`): a `LogHead` commits to *both* structures at
  once — the log root + size, the sparse-map root, and a second log (`vMapLog`)
  that itself logs successive map-root snapshots (so map-root history is also
  tamper-evident) — all hashed together and signed once. This four-way
  bundling (log root, log size, map root, map-log root+size) into one
  signature is the main structural idea worth stealing if tally ever needs a
  "verifiable snapshot of both an append-only history and a queryable index
  over it," which is close to tally's existing witness-ledger-plus-query-cache
  shape.

**Submission / verification flow** — protocol defined in
`~/Downloads/trustix/packages/trustix-proto/doc.md` (rendered from
`.proto` sources under `packages/trustix-proto/`), served by
`~/Downloads/trustix/packages/trustix/internal/server/logrpc.go`:
- `Submit(logID, [(key,value)...])` appends entries to a named log (requires an
  internal `X-TRUSTIX-AUTH` header — trusted-writer only, `logrpc.go:86-104`).
- `GetHead(logID)` returns the current signed `LogHead`.
- `GetLogEntries`, `GetLogAuditProof`, `GetLogConsistencyProof`,
  `GetMapValue` (with `SparseCompactMerkleProof`) are the public read/verify
  surface — a third party can fetch a head, fetch entries/proofs, and verify
  both leaf inclusion and root consistency without trusting the server.

**Comparing independent logs** —
`~/Downloads/trustix/packages/trustix/internal/decider/` implements pluggable
"deciders" that take `[]*DeciderInput` (one entry per log that reported a value
for the same key) and produce a `DeciderOutput{Value, Confidence}`:
`logid.go` (trust one named log outright), `percentage.go`
(`minimumPercent` — require N% of logs to agree), `agg.go` (chain deciders),
`js.go` (arbitrary JS-scripted decision function via goja). `RPCApi.Decide`
(`logrpc.go` companion, proto: `DecideRequest`/`DecisionResponse`) is the entry
point a client calls with a content digest to get a cross-log trust
verdict — this pluggable-quorum idea (not just "hash-chain verifies," but "N
independently-witnessing parties agree") is the part with no analogue in
tally's current single-writer witness ledger, and is the main reason Trustix is
flagged for a v2 design session rather than mined for direct code reuse.

No RFC document was found in the repo (only the doc-comment-driven proto docs
and the mdbook under `trustix-doc`); there is no separate formal spec file.

---

## 4. Lift list / do-NOT-copy

### Lift (patterns worth transferring into tally's design)

| From | What | Why |
|---|---|---|
| attic `push.rs` `PushPlan::plan` | closure → filter-already-cached → filter-upstream-signed → query-missing, in that order | Directly reusable shape for "which evidence artifacts still need pushing to a remote attic cache" — tally would substitute its own dedup-key check for the upstream-signature filter. |
| attic `upload_path.rs` two-level dedup (NAR-hash reuse, then chunk-hash reuse) | Content-addressed dedup at two granularities with a `Finally`-style rollback guard on partial failure | tally's own artifact dedup (SPEC.md's "existence-based… rehash to the witnessed value") is already conceptually identical; the *rollback-on-partial-write* pattern (`Finally::new`, delete-on-drop-unless-cancelled) is the concrete piece worth lifting verbatim as a Rust idiom. |
| attic JWT cache-permission model (`token/src/lib.rs`) | Stateless, wildcard-pattern, per-verb (r/w/d/cc/cr/cq/cd) capability tokens | If tally ever needs multi-host push credentials, this is a smaller surface than standing up its own auth service — but note this is *already solved by deploying attic itself*, not something to reimplement. |
| Nix `nix store add --mode flat` + indirect GC roots | The only sanctioned path from "arbitrary file" to "collectible, attic-pushable store object" | This is not optional — it is the *only* mechanism; see Section 1.8/2. |
| Trustix `LogHead` (log-root + map-root + map-log-root, bundled + signed once) | Single-signature commitment over both an append-only history and an index over it | Directly analogous to tally's witness-ledger + query-projection split; worth a full design pass at witness-v2 time, not before. |
| Trustix decider quorum model | Pluggable N-of-M agreement over independently-submitted facts | Only relevant if tally ever has >1 independent witness process; today tally has one. |

### Do NOT copy

| From | What | Why not |
|---|---|---|
| attic S3/local storage backend abstraction | `server/src/storage/{s3,local}.rs` | tally has no need for a pluggable object-storage backend of its own — if remote artifact storage is needed, that's what deploying attic *is for*; reimplementing a second storage abstraction inside tally would just duplicate attic. |
| attic's own DB-driven GC (`gc.rs`) as a model for tally's retention | Time-based reap + orphan reap, keyed by SQL joins over `object`/`nar`/`chunk` tables | tally's retention is meant to be GC-roots-as-policy (Nix-native, filesystem-symlink-based per Section 2), not a second SQL-backed reaper. Copying attic's DB-GC pattern would mean building a parallel, redundant retention system instead of using the one attic already runs for its own tables. |
| attic's chunking (FastCDC) | `attic/src/chunking/mod.rs` | Only meaningful for large, compressible NAR-like blobs pushed at volume; a single job's evidence artifact is very unlikely to benefit, and reimplementing CDC chunking inside tally would duplicate what attic's server already does automatically on ingest. |
| Trustix's transport/RPC stack (connect-rpc/gRPC, `packages/trustix-proto/`, `packages/unixtransport/`) | Custom protobuf services + Unix-socket transport | Explicitly out of scope for now (Section 3 is design-reference only); pulling in a second RPC framework/protocol stack alongside whatever tally already uses would be premature and is not what "design reference" means here. |
| Trustix's sparse-Merkle-tree map implementation (`celestiaorg/smt` dependency) | Full external Go dependency for keyed membership proofs | Language mismatch (tally is Rust) and premature — nothing in tally's current SPEC.md calls for cross-witness membership proofs; would need its own v2 ruling first. |
| attic's soft-delete-caches option, per-cache signing keypairs, Fly.io-oriented cold-start design notes | `server/src/config.rs` (`soft_delete_caches`), `access/http.rs` module doc | These solve attic's own multi-tenant SaaS-cache problem (many caches, many owners, serverless cold starts). tally.nix is not building a multi-tenant cache product; these are attic-operational concerns, not tally-design concerns. |

---

## Sources

- attic: `~/Downloads/attic` (shallow clone of `zhaofengli/attic`, default branch).
- Trustix: `~/Downloads/trustix` (shallow clone of `nix-community/trustix`, default branch).
- Nix manual (2.24), fetched pages:
  `command-ref/new-cli/nix3-store-add`,
  `command-ref/new-cli/nix3-store-add-file`,
  `command-ref/new-cli/nix3-store-add-path`,
  `command-ref/nix-store/gc`,
  `command-ref/nix-store/realise` (for `--add-root`).
- tally.nix's own `docs/SPEC.md` (witness ledger, job artifact/dedup semantics)
  read for vocabulary alignment only — not modified.
