import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import ts from 'typescript';
import {
  decodeEntities,
  isTranslatableText,
  normalizeText,
  TRANSLATABLE_ATTRIBUTES,
} from '../../i18n/normalize.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const webRoot = path.resolve(here, '../..');
const srcRoot = path.join(webRoot, 'src');
const outFile = path.join(webRoot, 'i18n', 'catalog.en.json');
const keysFile = path.join(srcRoot, 'i18n', 'keys.ts');
const deFile = path.join(webRoot, 'public', 'i18n', 'de.json');

const UI_PROPERTY_NAMES = new Set([
  'label',
  'title',
  'description',
  'subtitle',
  'tooltip',
  'heading',
  'message',
  'emptytext',
  'emptylabel',
  'desc',
  'badge',
  'text',
  'detail',
  'caption',
  'hint',
]);

function walkFiles(dir, out = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === 'node_modules' || entry.name === 'dist') continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      if (full === path.join(srcRoot, 'i18n')) continue;
      walkFiles(full, out);
    } else if (
      entry.isFile() &&
      /\.tsx?$/.test(full) &&
      !/\.(test|spec)\.tsx?$/.test(full) &&
      !full.endsWith('.d.ts')
    ) {
      out.push(full);
    }
  }
  return out;
}

function collect(sourceFile, keys) {
  const add = (value) => {
    const text = normalizeText(decodeEntities(value));
    if (isTranslatableText(text)) keys.add(text);
  };
  const addExplicit = (value) => {
    const text = normalizeText(decodeEntities(value));
    if (text) keys.add(text);
  };

  const visit = (node) => {
    if (ts.isJsxText(node)) {
      add(node.text);
    } else if (ts.isJsxAttribute(node) && ts.isIdentifier(node.name)) {
      const name = node.name.text.toLowerCase();
      if (
        TRANSLATABLE_ATTRIBUTES.has(name) &&
        node.initializer &&
        ts.isStringLiteral(node.initializer)
      ) {
        add(node.initializer.text);
      }
    } else if (ts.isCallExpression(node)) {
      const callee = node.expression.getText(sourceFile);
      const short = callee.split('.').pop() ?? callee;
      if ((short === 't' || short === 'tr') && node.arguments.length > 0) {
        const first = node.arguments[0];
        if (ts.isStringLiteral(first) || ts.isNoSubstitutionTemplateLiteral(first)) {
          addExplicit(first.text);
        }
      }
    } else if (ts.isPropertyAssignment(node)) {
      const name = node.name.getText(sourceFile).replace(/['"]/g, '').toLowerCase();
      if (UI_PROPERTY_NAMES.has(name)) {
        const init = node.initializer;
        if (ts.isStringLiteral(init) || ts.isNoSubstitutionTemplateLiteral(init)) {
          add(init.text);
        }
      }
    }
    ts.forEachChild(node, visit);
  };

  visit(sourceFile);
}

const keys = new Set();
for (const file of walkFiles(srcRoot)) {
  const source = fs.readFileSync(file, 'utf8');
  const kind = file.endsWith('.tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS;
  const sourceFile = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, kind);
  collect(sourceFile, keys);
}

for (const key of [
  'Chrome icon',
  'Collapse sidebar',
  'Connection successful',
  'Custom',
  'Expand sidebar',
  'Fluence Agent',
  'LLM connection successful',
  'Local Offline',
  'Natural',
  'Professional',
  'Proofread',
  'Sign-out',
  'Switch to dark mode',
  'Switch to light mode',
  'The all-rounder. Edits, rewrites, and answers questions.',
  'Welcome to Fluence Transcribe',
]) {
  keys.add(key);
}

const sorted = [...keys].sort((a, b) => a.localeCompare(b, 'en'));
const catalog = Object.fromEntries(sorted.map((key) => [key, key]));
fs.mkdirSync(path.dirname(outFile), { recursive: true });
fs.writeFileSync(outFile, JSON.stringify(catalog, null, 2) + '\n', 'utf8');

const union = sorted.map((key) => `  | ${JSON.stringify(key)}`).join('\n');
fs.writeFileSync(keysFile, `export type TranslationKey =\n${union};\n`, 'utf8');

const german = JSON.parse(fs.readFileSync(deFile, 'utf8'));
const missing = sorted.filter((key) => !(key in german));
const extra = Object.keys(german).filter((key) => !(key in catalog));
const placeholders = (value) =>
  [...String(value).matchAll(/\{(\w+)\}/g)].map((match) => match[1]).sort().join(',');
const mismatched = sorted.filter((key) => placeholders(key) !== placeholders(german[key]));

if (missing.length > 0 || extra.length > 0 || mismatched.length > 0) {
  for (const key of missing) console.error(`Missing German translation: ${JSON.stringify(key)}`);
  for (const key of extra) console.error(`Unused German translation: ${JSON.stringify(key)}`);
  for (const key of mismatched) console.error(`Placeholder mismatch: ${JSON.stringify(key)}`);
  process.exitCode = 1;
}

console.log(`Extracted ${sorted.length} UI strings -> ${path.relative(webRoot, outFile)}`);
