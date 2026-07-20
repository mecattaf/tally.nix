// tally — models.dev model-id normalization (SPEC "Record schema" `model`; IMPLEMENTATION-PLAN M1.2).
//
// The witness `model` field carries a models.dev `provider/model-name` id (jul9). Ids that already
// contain a `/` are treated as fully-qualified and pass through untouched; bare harness-reported
// names are prefix-normalized onto the models.dev provider convention. `model` is absent (null) on
// shell runs — the normalizer therefore accepts and returns `null` unchanged.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

/**
 * Prefix rules mapping a bare harness model family to its models.dev provider. Ordered longest /
 * most-specific first is unnecessary here because the families are disjoint; matching is by
 * case-insensitive family prefix. This is the one table the SPEC enumerates
 * (`claude-* → anthropic/…`, `gpt-* → openai/…`, `gemini-* → google/…`).
 */
interface PrefixRule {
  /** Case-insensitive prefix a bare name must start with. */
  readonly prefix: string;
  /** The models.dev provider to prepend. */
  readonly provider: string;
}

const PREFIX_RULES: readonly PrefixRule[] = [
  { prefix: "claude-", provider: "anthropic" },
  { prefix: "claude.", provider: "anthropic" },
  { prefix: "gpt-", provider: "openai" },
  { prefix: "o1-", provider: "openai" },
  { prefix: "o3-", provider: "openai" },
  { prefix: "o4-", provider: "openai" },
  { prefix: "gemini-", provider: "google" },
  { prefix: "gemini.", provider: "google" },
];

/**
 * Normalize a raw model identifier to a models.dev `provider/model-name` id.
 *
 * Rules (SPEC "Record schema"):
 *  - `null` / `undefined` / empty ⇒ `null` (shell runs carry no model).
 *  - an id already containing `/` is fully-qualified ⇒ passed through verbatim (trimmed).
 *  - a bare harness family name is prefix-normalized: `claude-` → `anthropic/<name>`,
 *    `gpt-`/`o1-`/`o3-`/`o4-` → `openai/<name>`, `gemini-` → `google/<name>`.
 *  - any other bare name is returned trimmed but unqualified (we never invent a provider we
 *    don't know — an unknown bare id is preserved so no proof is fabricated).
 */
export function normalizeModelId(raw: string | null | undefined): string | null {
  if (raw === null || raw === undefined) return null;
  const trimmed = raw.trim();
  if (trimmed.length === 0) return null;
  // Already provider-qualified (or otherwise contains a path separator): pass through.
  if (trimmed.includes("/")) return trimmed;
  const lower = trimmed.toLowerCase();
  for (const rule of PREFIX_RULES) {
    if (lower.startsWith(rule.prefix)) {
      return `${rule.provider}/${trimmed}`;
    }
  }
  // Unknown bare family: preserve verbatim rather than guess a provider.
  return trimmed;
}
