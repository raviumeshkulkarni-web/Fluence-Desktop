import { invokeCmd } from '@/ipc/tauri';
import type { ClaimOutcome } from '@/lib/claim';

export interface CustomAgent {
  id: string;
  name: string;
  hint: string;
  /**
   * True only for an unclaimed pre-account record while an account is signed in.
   *
   * Computed server-side in `account_scope::VisibleAgents::into_view` from the
   * same admission gate the policy uses, so the affordance cannot disagree with
   * the rule — and cannot be influenced by anything the frontend sends. Absent on
   * older builds, hence optional.
   */
  claimable?: boolean;
}

export interface AgentsView {
  builtin_id: string;
  builtin_name: string;
  builtin_description: string;
  default_id: string;
  custom_agents: CustomAgent[];
}

// Slice 4b/5 command names + argument shapes (backend: agents.rs).
export const getAgents = () => invokeCmd<AgentsView>('get_agents');

export const saveAgent = (name: string, hint: string, id?: string) =>
  invokeCmd<CustomAgent>('save_agent', { name, hint, id: id ?? null });

export const deleteAgent = (id: string) =>
  invokeCmd<void>('delete_agent', { id });

export const setDefaultAgent = (id: string) =>
  invokeCmd<void>('set_default_agent', { id });

// Adopt ONE unclaimed pre-account agent into the account signed in right now.
//
// Only `id` crosses the boundary. The destination account is resolved by the
// backend from the durable session and is deliberately NOT a parameter, so no
// frontend payload can choose which account receives the record. The backend
// scopes the claim to this single id, so a per-row action can never move records
// the user did not choose.
//
// Copy, not move: the pre-account row is left on disk so a sync pass that
// discards this claim degrades to a re-claimable shadow rather than data loss.
export const claimLegacyAgent = (id: string) =>
  invokeCmd<ClaimOutcome>('claim_legacy_agent', { id });
