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

// History-proof dashboard source: daily UTC buckets from the synced
// stats ledger (local ∪ remote), never the local history table.
// since_ms bounds the read server-side (bucket only days >= since);
// omit it for the full All-time series. O(days), never O(events).
export interface DailyBucket {
  day_start_ms: number;
  sessions: number;
  words: number;
  duration_ms: number;
}

export const getAccountActivity = (sinceMs?: number) =>
  invokeCmd<DailyBucket[]>('get_account_activity', {
    sinceMs: sinceMs ?? null,
  });
