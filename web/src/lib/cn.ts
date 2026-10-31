import { clsx, type ClassValue } from 'clsx';
import { twMerge } from 'tailwind-merge';

// Canonical shadcn `cn` helper. tailwind-merge is inert here until Tailwind
// utilities are adopted (deferred slice) — today it only dedupes class lists.
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
