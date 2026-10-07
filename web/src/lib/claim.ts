// Claim-outcome policy for the Agents / AI-cleanup Styles boards.
//
// Deliberately PURE: no IPC, no React, no DOM. The backend owns the decision
// (this module never re-derives whether a claim succeeded — it only renders what
// the backend reported), and keeping the mapping here means the messaging rules
// are unit-testable and cannot drift between the two boards.
//
// `refusedIds` arrives as `[id, reason]` pairs because the backend serialises
// `Vec<(String, &'static str)>`. Reasons are fixed strings defined by
// `account_scope::ClaimOutcome`; an unknown reason degrades to a generic message
// rather than being shown raw.

// The fields are typed as required, but the UI and the Rust binary version
// INDEPENDENTLY: a user can run a new frontend against an older binary that
// predates `refusedIds`/`skippedIds`. Reading those as definite would throw a
// TypeError and replace truthful messaging with a raw crash toast, so every field
// is treated as possibly-absent at runtime.
export type ClaimRefusal = readonly [id: string, reason: string];

export interface ClaimOutcome {
  claimedIds?: string[];
  skippedIds?: string[];
  refusedIds?: ClaimRefusal[];
}

const ids = (value: string[] | undefined): string[] =>
  Array.isArray(value) ? value : [];

// Elements are guarded individually too: destructuring `[, reason]` on a null, a
// number, or an object throws, which would replace truthful messaging with a crash
// toast on malformed data.
const refusals = (value: ClaimRefusal[] | undefined): ClaimRefusal[] => {
  if (!Array.isArray(value)) return [];
  return value.filter(
    (entry): entry is ClaimRefusal =>
      Array.isArray(entry) && entry.length >= 2 && typeof entry[1] === 'string',
  );
};

export type ClaimMessageKind = 'success' | 'info';

export interface ClaimMessage {
  kind: ClaimMessageKind;
  text: string;
}

/**
 * A refused record is a deliberate policy decision, not an error: the user asked
 * to adopt something and the app is explaining why it will not. Reporting it as
 * an error would be misleading, and reporting it as "nothing to add" would hide
 * the reason — so it gets its own wording.
 *
 * Reachability, stated honestly:
 *  - `claimed` — the normal outcome.
 *  - `duplicate-legacy-id` / `builtin-id` — reachable (a hand-edited or migrated
 *    legacy store can present these).
 *  - `skipped` — NOT reachable from the boards. A row is only claimable when the
 *    union shows it as legacy-and-unassigned, which by the account-first dedupe
 *    means the account does not hold that id, so the backend cannot report it as
 *    already owned. Handled anyway, defensively.
 *  - `tombstoned` — NOT reachable from the boards either: a tombstoned account id
 *    wins the dedupe and the tombstone is then filtered from display, so there is
 *    no row to tap. Handled anyway, defensively.
 */
export function describeClaimOutcome(
  outcome: ClaimOutcome,
  subject: 'agent' | 'style',
): ClaimMessage {
  if (ids(outcome.claimedIds).length > 0) {
    return {
      kind: 'success',
      text: 'Added to your account — it will sync to your devices',
    };
  }

  const reasons = new Set(refusals(outcome.refusedIds).map(([, reason]) => reason));
  if (reasons.size > 0) {
    if (reasons.has('tombstoned')) {
      return {
        kind: 'info',
        text: `That ${subject} was deleted in your account, so it was not added.`,
      };
    }
    if (reasons.has('duplicate-legacy-id')) {
      return {
        kind: 'info',
        text: `That ${subject} appears more than once on this device, so it was not added.`,
      };
    }
    if (reasons.has('builtin-id')) {
      return {
        kind: 'info',
        text: `That ${subject} is built in and cannot be added to an account.`,
      };
    }
    return { kind: 'info', text: `That ${subject} could not be added.` };
  }

  if (ids(outcome.skippedIds).length > 0) {
    return {
      kind: 'info',
      text: 'Already in your account — nothing changed',
    };
  }

  return { kind: 'info', text: 'Nothing to add' };
}