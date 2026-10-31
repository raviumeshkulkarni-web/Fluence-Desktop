// Provider brand/glyph icons. Brand marks are copied verbatim from the
// vanilla #page-providers markup (src/index.html); UI glyphs come from
// lucide-react so every refresh/download affordance matches the rest of the
// app (wizard included). Fixed sizes as in vanilla.
import { Download, RefreshCw } from 'lucide-react';
export function GroqIcon() {
  return (
    <svg aria-hidden="true" focusable="false" width="28" height="28" viewBox="0 0 100 100" fill="none" xmlns="http://www.w3.org/2000/svg">
      <rect width="100" height="100" rx="16" fill="#F55036" />
      <path d="M53 20 C 37 20 28 30 28 45 C 28 60 37 70 53 70 C 58 70 63 68 67 65 L 67 71 C 67 82 59 90 47 90 C 40 90 35 87 31 81 L 22 88 C 28 97 37 100 47 100 C 65 100 78 88 78 71 L 78 22 L 67 22 L 67 27 C 63 23 58 20 53 20 Z M 53 60 C 44 60 39 53 39 45 C 39 37 44 30 53 30 C 62 30 67 37 67 45 C 67 53 62 60 53 60 Z" fill="#fff" />
    </svg>
  );
}

export function OpenAiIcon() {
  return (
    <svg aria-hidden="true" focusable="false" width="28" height="28" viewBox="0 0 24 24" fill="currentColor" xmlns="http://www.w3.org/2000/svg">
      <path d="M22.282 9.821a5.985 5.985 0 0 0-.516-4.91 6.046 6.046 0 0 0-6.51-2.9A6.065 6.065 0 0 0 4.981 4.18a5.985 5.985 0 0 0-3.998 2.9 6.046 6.046 0 0 0 .743 7.097 5.98 5.98 0 0 0 .51 4.911 6.051 6.051 0 0 0 6.515 2.9A5.985 5.985 0 0 0 13.26 24a6.056 6.056 0 0 0 5.772-4.206 5.99 5.99 0 0 0 3.997-2.9 6.056 6.056 0 0 0-.747-7.073zM13.26 22.43a4.476 4.476 0 0 1-2.876-1.04l.141-.081 4.779-2.758a.795.795 0 0 0 .392-.681v-6.737l2.02 1.168a.071.071 0 0 1 .038.052v5.583a4.504 4.504 0 0 1-4.494 4.494zM3.6 18.304a4.47 4.47 0 0 1-.535-3.014l.142.085 4.783 2.759a.771.771 0 0 0 .78 0l5.843-3.369v2.332a.08.08 0 0 1-.033.062L9.74 19.95a4.5 4.5 0 0 1-6.14-1.646zM2.34 7.896a4.485 4.485 0 0 1 2.366-1.973V11.6a.766.766 0 0 0 .388.676l5.815 3.355-2.02 1.168a.076.076 0 0 1-.071 0l-4.83-2.786A4.504 4.504 0 0 1 2.34 7.896zm16.597 3.855l-5.843-3.372 2.02-1.163a.076.076 0 0 1 .071 0l4.83 2.786a4.494 4.494 0 0 1-.676 8.105v-5.678a.79.79 0 0 0-.402-.678zm2.01-3.023l-.141-.085-4.774-2.782a.776.776 0 0 0-.785 0L9.409 9.23V6.897a.066.066 0 0 1 .028-.061l4.83-2.787a4.5 4.5 0 0 1 6.68 4.66zm-12.64 4.135l-2.02-1.164a.08.08 0 0 1-.038-.057V6.075a4.5 4.5 0 0 1 7.375-3.453l-.142.08L8.704 5.46a.795.795 0 0 0-.393.681zm1.097-2.365l2.602-1.5 2.607 1.5v2.999l-2.597 1.5-2.607-1.5z" />
    </svg>
  );
}

export function MistralIcon() {
  return (
    <svg aria-hidden="true" focusable="false" width="28" height="28" viewBox="0 0 50 50" xmlns="http://www.w3.org/2000/svg">
      <rect x="0" y="0" width="10" height="10" fill="#FCD34D" />
      <rect x="40" y="0" width="10" height="10" fill="#FCD34D" />
      <rect x="0" y="10" width="10" height="10" fill="#F59E0B" />
      <rect x="40" y="10" width="10" height="10" fill="#F59E0B" />
      <rect x="0" y="20" width="50" height="10" fill="#F97316" />
      <rect x="0" y="30" width="10" height="10" fill="#EA580C" />
      <rect x="20" y="30" width="10" height="10" fill="#EA580C" />
      <rect x="40" y="30" width="10" height="10" fill="#EA580C" />
      <rect x="0" y="40" width="20" height="10" fill="#DC2626" />
      <rect x="30" y="40" width="20" height="10" fill="#DC2626" />
    </svg>
  );
}

export function CustomProviderIcon() {
  return (
    <svg aria-hidden="true" focusable="false" width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><circle cx="12" cy="12" r="3" /><path d="M12 1v4M12 19v4M4.22 4.22l2.83 2.83M16.95 16.95l2.83 2.83M1 12h4M19 12h4M4.22 19.78l2.83-2.83M16.95 7.05l2.83-2.83" /></svg>
  );
}

export function OfflineProviderIcon() {
  return (
    <svg aria-hidden="true" focusable="false" width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <path d="M12 2L2 7l10 5 10-5-10-5zM2 17l10 5 10-5M2 12l10 5 10-5" />
    </svg>
  );
}

export function RefreshIcon() {
  return <RefreshCw size={16} strokeWidth={2} aria-hidden="true" />;
}

export function DownloadIcon() {
  return (
    <Download
      size={18}
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    />
  );
}
