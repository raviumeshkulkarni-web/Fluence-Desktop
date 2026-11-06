export interface ReleaseNotesSection {
  title: string;
  items: string[];
}

function plainText(markdown: string): string {
  return markdown
    .replace(/!?\[([^\]]*)\]\([^)]+\)/g, '$1')
    .replace(/(\*\*|__)(.*?)\1/g, '$2')
    .replace(/([*_])([^*_]+)\1/g, '$2')
    .replace(/`([^`]+)`/g, '$1')
    .replace(/<[^>]*>/g, '')
    .trim();
}

export function parseReleaseNotes(body: string | null | undefined): ReleaseNotesSection[] {
  if (!body?.trim()) return [];

  const sections: ReleaseNotesSection[] = [{ title: 'Release highlights', items: [] }];
  for (const line of body.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed) continue;

    const heading = trimmed.match(/^#{1,6}\s+(.+?)\s*#*$/);
    if (heading) {
      const title = plainText(heading[1]);
      const current = sections[sections.length - 1];
      if (current.items.length === 0 && current.title === 'Release highlights') {
        current.title = title;
      } else {
        sections.push({ title, items: [] });
      }
      continue;
    }

    const bullet = trimmed.match(/^(?:[-*+]\s+|\d+[.)]\s+)(.*)$/);
    const item = plainText(bullet ? bullet[1] : trimmed);
    if (item) sections[sections.length - 1].items.push(item);
  }

  return sections.filter((section) => section.items.length > 0);
}
