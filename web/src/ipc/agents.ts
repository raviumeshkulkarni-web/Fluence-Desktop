import { invokeCmd } from '@/ipc/tauri';

export interface CustomAgent {
  id: string;
  name: string;
  hint: string;
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
