export interface TermPackEntry {
  spoken: string;
  corrected: string;
}

export interface TermPack {
  id: string;
  name: string;
  description: string;
  entries: TermPackEntry[];
}

// Curated correction sets. Entries are only applied when the whole spoken
// phrase appears, and existing entries are skipped when a pack is added.
export const TERM_PACKS: TermPack[] = [
  {
    id: 'developer',
    name: 'Developer & Web',
    description: 'Tools and frameworks commonly misheard by speech models.',
    entries: [
      { spoken: 'git hub', corrected: 'GitHub' },
      { spoken: 'git lab', corrected: 'GitLab' },
      { spoken: 'java script', corrected: 'JavaScript' },
      { spoken: 'type script', corrected: 'TypeScript' },
      { spoken: 'node js', corrected: 'Node.js' },
      { spoken: 'next js', corrected: 'Next.js' },
      { spoken: 'my sql', corrected: 'MySQL' },
      { spoken: 'postgres', corrected: 'PostgreSQL' },
      { spoken: 'graphql', corrected: 'GraphQL' },
      { spoken: 'graph q l', corrected: 'GraphQL' },
      { spoken: 'kubernetes', corrected: 'Kubernetes' },
      { spoken: 'kube netes', corrected: 'Kubernetes' },
      { spoken: 'vs code', corrected: 'VS Code' },
    ],
  },
  {
    id: 'ai-data',
    name: 'AI & Data',
    description: 'AI products, hardware, and data formats.',
    entries: [
      { spoken: 'chat gpt', corrected: 'ChatGPT' },
      { spoken: 'open ai', corrected: 'OpenAI' },
      { spoken: 'hugging face', corrected: 'Hugging Face' },
      { spoken: 'g p u', corrected: 'GPU' },
      { spoken: 'c p u', corrected: 'CPU' },
      { spoken: 'l l m', corrected: 'LLM' },
      { spoken: 's q l', corrected: 'SQL' },
      { spoken: 'n l p', corrected: 'NLP' },
      { spoken: 'j s o n', corrected: 'JSON' },
      { spoken: 'y a m l', corrected: 'YAML' },
    ],
  },
  {
    id: 'business',
    name: 'Business & Finance',
    description: 'Common business abbreviations and quarter names.',
    entries: [
      { spoken: 'r o i', corrected: 'ROI' },
      { spoken: 'k p i', corrected: 'KPI' },
      { spoken: 'c r m', corrected: 'CRM' },
      { spoken: 'saas', corrected: 'SaaS' },
      { spoken: 'b two b', corrected: 'B2B' },
      { spoken: 'b two c', corrected: 'B2C' },
      { spoken: 'q one', corrected: 'Q1' },
      { spoken: 'q two', corrected: 'Q2' },
      { spoken: 'q three', corrected: 'Q3' },
      { spoken: 'q four', corrected: 'Q4' },
      { spoken: 'e o d', corrected: 'EOD' },
    ],
  },
  {
    id: 'healthcare',
    name: 'Healthcare',
    description: 'Clinical abbreviations and imaging terms.',
    entries: [
      { spoken: 'm r i', corrected: 'MRI' },
      { spoken: 'c t scan', corrected: 'CT scan' },
      { spoken: 'e k g', corrected: 'EKG' },
      { spoken: 'e c g', corrected: 'ECG' },
      { spoken: 'c b c', corrected: 'CBC' },
      { spoken: 'i v', corrected: 'IV' },
      { spoken: 'n p o', corrected: 'NPO' },
      { spoken: 'o t c', corrected: 'OTC' },
      { spoken: 'x ray', corrected: 'X-ray' },
      { spoken: 'd o b', corrected: 'DOB' },
    ],
  },
  {
    id: 'legal',
    name: 'Legal & Compliance',
    description: 'Contract and privacy abbreviations.',
    entries: [
      { spoken: 'n d a', corrected: 'NDA' },
      { spoken: 'l l c', corrected: 'LLC' },
      { spoken: 'i p', corrected: 'IP' },
      { spoken: 'e u l a', corrected: 'EULA' },
      { spoken: 'g d p r', corrected: 'GDPR' },
      { spoken: 'h i p a a', corrected: 'HIPAA' },
      { spoken: 's o w', corrected: 'SOW' },
      { spoken: 'r f p', corrected: 'RFP' },
    ],
  },
];

export function termPackCount(packs: TermPack[]): number {
  return packs.reduce((total, pack) => total + pack.entries.length, 0);
}
