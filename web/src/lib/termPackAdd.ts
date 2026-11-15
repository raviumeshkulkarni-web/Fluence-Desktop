import type { TermPack } from './termPacks';

export interface TermPackAddResult {
  added: number;
  skipped: number;
  failed: number;
  firstError: unknown;
}

/**
 * Add selected packs one entry at a time. A failure on one entry does not
 * abort the rest, so already-persisted entries are never hidden by a stale
 * list and the caller can report partial success accurately.
 */
export async function addTermPacks(
  packs: TermPack[],
  existingSpoken: Iterable<string>,
  add: (spoken: string, corrected: string) => Promise<unknown>,
): Promise<TermPackAddResult> {
  const existing = new Set(
    Array.from(existingSpoken, (value) => value.trim().toLowerCase()),
  );
  let added = 0;
  let skipped = 0;
  let failed = 0;
  let firstError: unknown = null;

  for (const pack of packs) {
    for (const term of pack.entries) {
      const key = term.spoken.trim().toLowerCase();
      if (existing.has(key)) {
        skipped += 1;
        continue;
      }
      try {
        await add(term.spoken, term.corrected);
        existing.add(key);
        added += 1;
      } catch (err) {
        if (String(err).toLowerCase().includes('already exists')) {
          existing.add(key);
          skipped += 1;
        } else {
          failed += 1;
          if (firstError === null) firstError = err;
        }
      }
    }
  }

  return { added, skipped, failed, firstError };
}
