// Claim-outcome policy for the Agents / AI-cleanup Styles boards.
//
// Deliberately PURE: no IPC, no React, no DOM. The backend owns the decision
// (this module never re-derives whether a claim succeeded — it only renders what
// the backend reported), and keeping the mapping here means the messaging rules
// are unit-testable and cannot drift between the two boards.
//
// CONTRACT (audit finding S6): an empty outcome — all three lists absent or
// empty, rendering as "Nothing to add" — means genuinely nothing happened.
// It NEVER masks a failure: `claim_legacy_account_agents` and
// `claim_legacy_account_styles` return `Err` when the durable save fails, so a
// failed save surfaces through the caller's catch as an error toast, never
// through this function. If this function ever renders "Nothing to add" for a
// claim the user just tapped, that is a backend bug, not an expected state.
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
import { t } from '@/i18n';

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
      text: t('Added to your account — it will sync to your devices'),
    };
  }

  const copy =
    subject === 'agent'
      ? {
          deleted: t('That agent was deleted in your account, so it was not added.'),
          duplicate: t(
            'That agent appears more than once on this device, so it was not added.',
          ),
          builtIn: t('That agent is built in and cannot be added to an account.'),
          generic: t('That agent could not be added.'),
        }
      : {
          deleted: t('That style was deleted in your account, so it was not added.'),
          duplicate: t(
            'That style appears more than once on this device, so it was not added.',
          ),
          builtIn: t('That style is built in and cannot be added to an account.'),
          generic: t('That style could not be added.'),
        };

  const reasons = new Set(refusals(outcome.refusedIds).map(([, reason]) => reason));
  if (reasons.size > 0) {
    if (reasons.has('tombstoned')) {
      return { kind: 'info', text: copy.deleted };
    }
    if (reasons.has('duplicate-legacy-id')) {
      return { kind: 'info', text: copy.duplicate };
    }
    if (reasons.has('builtin-id')) {
      return { kind: 'info', text: copy.builtIn };
    }
    return { kind: 'info', text: copy.generic };
  }

  if (ids(outcome.skippedIds).length > 0) {
    return {
      kind: 'info',
      text: t('Already in your account — nothing changed'),
    };
  }

  return { kind: 'info', text: t('Nothing to add') };
}