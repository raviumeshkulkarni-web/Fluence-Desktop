// Shared user-facing error text. Backend errors are diagnostic strings, not
// copy: map the common classes to a sentence, and only pass through short,
// path-free messages that are already meaningful to a user.

const FALLBACK = 'Something went wrong. Please try again.';

// Paths and diagnostics are never user copy. Cover drive/UNC paths, registry
// keys, POSIX-style absolute or multi-segment paths, and common file
// extensions (the backend emits model names such as ggml-base.en.bin).
const PATH_OR_DIAGNOSTIC =
  /https?:\/\/|[A-Za-z]:[\\/]|\\\\[^\\\s]+\\|(?:^|[\s("'`])(?:\.{1,2}[\\/]|\/)|(?:^|[\s("'`])[\w.-]+[\\/][\w.-]+[\\/]|\b(?:HKEY_[A-Z_]+|HKLM|HKCU)\b|\b[\w-]+(?:\.[\w-]+)*\.(?:exe|dll|sys|msi|zip|7z|rar|gz|tgz|tar|flac|wav|mp3|m4a|ogg|opus|aac|onnx|gguf|bin|pt|ckpt|safetensors|db|sqlite3?|log|tmp|jsonl?|toml|ya?ml|cfg|conf|ini|rs|tsx?|jsx?|css|html?|md|txt|png|jpe?g|gif|webp|svg|csv|tsv|parquet)\b|panicked at|thread '|stack backtrace|os error \d+|<unknown>|unexpected (?:end of )?json|expected value at line|missing field|invalid type:|failed to (?:parse|deserialize)/i;

function looksSafe(text: string): boolean {
  if (text.length > 160) return false;
  if (PATH_OR_DIAGNOSTIC.test(text)) return false;
  // Unbalanced/structured output is a diagnostic dump, not user copy.
  if (/[{}\[\]"<>]/.test(text)) return false;
  return true;
}

export function describeError(err: unknown, fallback = FALLBACK): string {
  const raw = err instanceof Error ? err.message : String(err ?? '');
  const text = raw.replace(/^error:\s*/i, '').trim();
  if (!text) return fallback;
  const lower = text.toLowerCase();

  if (/\b(?:401|403)\b|unauthori[sz]ed|forbidden|invalid api key|incorrect api key|api key.*(invalid|rejected)/.test(lower)) {
    return 'The API key was rejected. Check the key and try again.';
  }
  if (/\b429\b|rate.?limit|quota/.test(lower)) {
    return 'The provider is limiting requests right now. Try again in a moment.';
  }
  if (/(dns|econn|network|timed out|timeout|failed to connect|connection (refused|reset|closed)|could not resolve)/.test(lower)) {
    return 'Could not reach the server. Check your connection and the endpoint URL, then try again.';
  }
  if (/\b404\b|not found/.test(lower)) {
    return 'The provider could not find that endpoint or model. Check the URL and model name.';
  }
  if (/\b5\d{2}\b|server error|internal server/.test(lower)) {
    return 'The provider returned a server error. Try again shortly.';
  }
  if (/(credential|keyring|windows credential|credential manager)/.test(lower)) {
    return 'Could not access Windows Credential Manager. Try again.';
  }
  if (/(database|sqlite|db is locked)/.test(lower)) {
    return 'Could not open the local database. Try again.';
  }
  if (/(permission|denied|not allowed|blocked)/.test(lower)) {
    return 'Windows blocked that action. Check permissions and try again.';
  }
  if (/(no such file|not exist|missing file|file not found|cannot find the file)/.test(lower)) {
    return 'A required file is missing. Repair or reinstall Fluence, then try again.';
  }
  return looksSafe(text) ? text : fallback;
}
