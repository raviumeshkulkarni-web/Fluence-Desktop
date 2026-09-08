import * as React from 'react';

/**
 * Escapes regex special characters in a search query.
 */
function escapeRegExp(str: string): string {
  return str.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * Safely highlights occurrences of `query` within `text` by returning an array
 * of React nodes (with `<mark className="search-highlight">`).
 *
 * Guarantees zero HTML injection risk (does not use dangerouslySetInnerHTML).
 */
export function highlightMatch(text: string, query: string): React.ReactNode {
  const q = query.trim();
  if (!q) return text;

  const escaped = escapeRegExp(q);
  const regex = new RegExp(`(${escaped})`, 'gi');
  const parts = text.split(regex);

  if (parts.length <= 1) return text;

  return parts.map((part, index) =>
    part.toLowerCase() === q.toLowerCase() ? (
      <mark key={index} className="search-highlight">
        {part}
      </mark>
    ) : (
      part
    ),
  );
}
