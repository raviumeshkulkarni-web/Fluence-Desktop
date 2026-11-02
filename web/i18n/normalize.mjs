export const TRANSLATABLE_ATTRIBUTES = new Set([
  'title',
  'placeholder',
  'alt',
  'aria-label',
  'aria-description',
  'label',
  'description',
  'tooltip',
  'subtitle',
  'heading',
  'emptytext',
  'emptylabel',
  'body',
  'confirmlabel',
  'cancellabel',
]);

const LETTER = /\p{L}/u;

export const EXCLUDED_KEYS = new Set(['Welcome to flu', 'flu', 'ence']);

const NAMED_ENTITIES = {
  amp: '&',
  lt: '<',
  gt: '>',
  quot: '"',
  apos: "'",
  nbsp: '\u00a0',
};

export function decodeEntities(value) {
  return String(value).replace(/&(#x?[0-9a-fA-F]+|[a-zA-Z]+);/g, (match, entity) => {
    if (entity[0] === '#') {
      const hex = entity[1].toLowerCase() === 'x';
      const code = parseInt(entity.slice(hex ? 2 : 1), hex ? 16 : 10);
      return Number.isFinite(code) ? String.fromCodePoint(code) : match;
    }
    return NAMED_ENTITIES[entity] ?? match;
  });
}

export function normalizeText(raw) {
  return String(raw).replace(/\s+/g, ' ').trim();
}

export function isTranslatableText(text) {
  if (!text || text.length < 2 || text.length > 600) return false;
  if (EXCLUDED_KEYS.has(text)) return false;
  if (!LETTER.test(text)) return false;
  if (/^(https?:|data:|mailto:|file:)/i.test(text)) return false;
  if (/^[.#/\\]/.test(text)) return false;
  if (text.includes('\u0000')) return false;
  if (!/[\p{L}\p{N}]/u.test(text)) return false;
  if (!/\s/.test(text) && !/^\p{Lu}/u.test(text) && !/^[\p{Lu}\p{N}]+$/u.test(text)) {
    return false;
  }
  if (!/\s/.test(text) && /^[\w.\-/\\:_]+$/.test(text) && !/^[A-Z][a-z]+$/.test(text)) {
    if (!/^[A-Z][A-Za-z0-9]*(?:-[A-Za-z0-9]+)+$/.test(text)) return false;
  }
  return true;
}
