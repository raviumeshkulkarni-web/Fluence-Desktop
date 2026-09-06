import { invokeCmd } from '@/ipc/tauri';

// get_account_stats returns either the merged account-ledger view
// (source "account", with weekly_timestamps) or the platform-local
// history-derived fallback (source "local", weekly_timestamps []).
export interface AccountStats {
  source: string;
  total_words: number;
  weekly_words: number;
  weekly_duration_ms: number;
  monthly_words: number;
  total_duration_ms: number;
  week_start_ms: number;
  weekly_timestamps?: string[] | null;
}

// Command names + argument shapes reproduced exactly from vanilla
// (loadDashboardStats).
export const getAccountStats = () =>
  invokeCmd<AccountStats>('get_account_stats');

export const getWeeklyActivity = (startOfWeekUtc: string) =>
  invokeCmd<string[]>('get_weekly_activity', { startOfWeekUtc });
