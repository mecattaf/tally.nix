// tally — gh intake signals: the normalized signal shape, the reason/qualifier → priority-class
// classifier, and the mute-respect predicate (IMPLEMENTATION-PLAN M2.4; octo.nvim surface scan
// §5–6). A "signal" is one qualifying attention-demand distilled from a notification thread or a
// search node into a single provenance-stable record the mapper turns into a TaskChampion row.
//
// Priority ordering (scan §6, default, config-tunable): review_requested > mention > assign. This is
// the cross-source urgency the intake proves over the one store — a gh signal out-ranks the OCR
// firehose (BUILD-SEQUENCE step 8). Mute respect (scan §5.8): a signal from an UNSUBSCRIBED/IGNORED
// subject is dropped so muted threads never resurface.
//
// Authored fresh for tally; no vendor/ code (clean-room, CLI-SURFACE §4).

import type { Priority } from "../contracts/job";
import type {
  Notification,
  NotificationReason,
  SearchNode,
  SubjectType,
  SubscriptionState,
} from "./gh";

/**
 * The signal class — the attention-demand category, mapped from a notification `reason` or the
 * search qualifier that surfaced the item. Ordered by default urgency (scan §6).
 */
export type SignalClass = "review_requested" | "mention" | "assign" | "author" | "other";

/** The default priority-class ordering (scan §6): review_requested > mention > assign > rest. */
export const SIGNAL_CLASS_ORDER: readonly SignalClass[] = [
  "review_requested",
  "mention",
  "assign",
  "author",
  "other",
];

/**
 * The default class → tally `Priority` map (scan §6, config-tunable via {@link SignalPolicy}).
 * review_requested is the highest-urgency PR signal (scan §5.3); mention is medium; assign/author/
 * other are low. `map.ts` reads this to set the row's priority (cross-source urgency ranking).
 */
export const DEFAULT_CLASS_PRIORITY: Record<SignalClass, Priority> = {
  review_requested: "high",
  mention: "medium",
  assign: "medium",
  author: "low",
  other: "low",
};

/** A normalized gh signal — the provenance-stable record the mapper turns into a TW row. */
export interface Signal {
  /** The GraphQL node id of the subject — the STABLE dedup key (scan §6 "dedup on node id"). */
  node_id: string;
  /** The classified attention-demand category. */
  class: SignalClass;
  /** The subject type (Issue / PullRequest / Discussion / …). */
  subject_type: SubjectType;
  /** The subject title (row description material). */
  title: string;
  /** The `owner/repo` this subject lives in. */
  repo: string;
  /** The item number within the repo (0 when unknown, e.g. a notification without a URL). */
  number: number;
  /** The web/api url of the subject. */
  url: string;
  /** The subject's `updatedAt` (the two-phase probe key), when known. */
  updated_at: string;
  /** Where this signal was surfaced from — for provenance + the two-phase decision. */
  origin: "notification" | "search";
}

/** Per-poll tunables (rendered from config in a later pass; defaults here are the scan's defaults). */
export interface SignalPolicy {
  /**
   * The notification reasons treated as first-class signals (scan §5.1). A reason not in this set is
   * dropped at classification. Default: the three first-class reasons plus `author`.
   */
  reasons: readonly NotificationReason[];
  /** The class → priority map (config-tunable; defaults to {@link DEFAULT_CLASS_PRIORITY}). */
  classPriority: Record<SignalClass, Priority>;
}

/** The default signal policy (scan §6 defaults). */
export function defaultSignalPolicy(): SignalPolicy {
  return {
    reasons: ["review_requested", "mention", "assign", "author"],
    classPriority: { ...DEFAULT_CLASS_PRIORITY },
  };
}

/**
 * Map a notification `reason` to a {@link SignalClass}. Unknown reasons collapse to `other` (the
 * union stays open, so the classifier never throws on a reason GitHub adds).
 */
export function classifyReason(reason: NotificationReason): SignalClass {
  switch (reason) {
    case "review_requested":
      return "review_requested";
    case "mention":
    case "team_mention":
      return "mention";
    case "assign":
      return "assign";
    case "author":
      return "author";
    default:
      return "other";
  }
}

/**
 * The tally priority for a signal class under a policy. Falls back to the default map for a class the
 * policy omits (forward-compat).
 */
export function priorityFor(cls: SignalClass, policy: SignalPolicy): Priority {
  return policy.classPriority[cls] ?? DEFAULT_CLASS_PRIORITY[cls];
}

/**
 * Whether a subscription tier means the subject is MUTED (scan §5.8): an explicitly `ignored` thread,
 * an unsubscribed (`subscribed:false` with a mute `reason`), or a thread whose `reason` is
 * `unsubscribed`. A subject with no inlined subscription is treated as NOT muted (the inbox already
 * filters to participating/@mentioned) — the poller only re-checks via the subscription endpoint
 * when a reason string demands it.
 */
export function isMuted(sub: SubscriptionState | undefined): boolean {
  if (sub === undefined) return false;
  if (sub.ignored) return true;
  if (sub.reason === "unsubscribed") return true;
  // `subscribed:false` without an explicit unsubscribe reason still means the user opted out.
  if (sub.subscribed === false && sub.ignored === false && sub.reason === null) return true;
  return false;
}

/**
 * Derive the GraphQL-node dedup key for a notification. The notification thread `id` is NOT the
 * subject node id (scan §5.1), so we key on the subject's stable `owner/repo#number` derived from the
 * subject url, falling back to the thread id when the url is absent. This keeps re-polls idempotent
 * on the SUBJECT rather than the (mutable) thread.
 */
export function notificationNodeKey(n: Notification): string {
  const derived = subjectKeyFromUrl(n.subject.url);
  return derived ?? `gh-thread:${n.id}`;
}

/** Parse an `owner/repo#number` subject key from a REST subject url; null when un-parseable. */
export function subjectKeyFromUrl(url: string | null): string | null {
  if (!url) return null;
  // e.g. https://api.github.com/repos/mecattaf/tally/pulls/128
  const m = url.match(/repos\/([^/]+)\/([^/]+)\/(?:pulls|issues|discussions)\/(\d+)/);
  if (!m) return null;
  return `${m[1]}/${m[2]}#${m[3]}`;
}

/** Parse `{repo, number}` from a REST/web subject url; both fields best-effort. */
export function subjectRefFromUrl(url: string | null): { repo: string; number: number } {
  if (!url) return { repo: "", number: 0 };
  const m = url.match(/(?:repos\/)?([^/]+)\/([^/]+)\/(?:pull|pulls|issues|discussions)\/(\d+)/);
  if (!m) return { repo: "", number: 0 };
  return { repo: `${m[1]}/${m[2]}`, number: Number(m[3]) };
}

/**
 * Distill a notification into a {@link Signal}, honoring the reason filter and mute respect. Returns
 * `null` when the reason is not a configured signal reason OR the subject is muted — so a dropped
 * notification never becomes a row.
 */
export function signalFromNotification(n: Notification, policy: SignalPolicy): Signal | null {
  if (!policy.reasons.includes(n.reason)) return null;
  if (isMuted(n.subscription)) return null;
  const cls = classifyReason(n.reason);
  const ref = subjectRefFromUrl(n.subject.url);
  return {
    node_id: notificationNodeKey(n),
    class: cls,
    subject_type: n.subject.type,
    title: n.subject.title,
    repo: n.repository.full_name || ref.repo,
    number: ref.number,
    url: n.subject.url ?? "",
    updated_at: n.updated_at,
    origin: "notification",
  };
}

/**
 * Distill a search node into a {@link Signal} for a given qualifier class. Search nodes carry their
 * own stable GraphQL `id` — the canonical dedup key. Mute respect is not expressible on a bare search
 * node (subscription tier is not in the search projection), so the poller de-conflicts search
 * signals against the notification set (which DOES carry mute) before mapping.
 */
export function signalFromSearchNode(node: SearchNode, cls: SignalClass): Signal {
  return {
    node_id: node.id,
    class: cls,
    subject_type: node.typename as SubjectType,
    title: node.title,
    repo: node.nameWithOwner,
    number: node.number,
    url: node.url,
    updated_at: node.updatedAt,
    origin: "search",
  };
}

/**
 * The search qualifier → default {@link SignalClass} map (scan §6 follow-up qualifiers). Used when
 * the poller runs the search primitives.
 */
export const SEARCH_QUALIFIERS: ReadonlyArray<{ qualifier: string; class: SignalClass }> = [
  { qualifier: "review-requested:@me is:open", class: "review_requested" },
  { qualifier: "assignee:@me is:open", class: "assign" },
  { qualifier: "mentions:@me is:open", class: "mention" },
  { qualifier: "author:@me is:open", class: "author" },
];
