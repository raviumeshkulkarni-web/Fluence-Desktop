import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));

const waveformPath = path.join(ROOT, 'web/src/components/fluence/waveform.ts');
const waveformStagePath = path.join(ROOT, 'web/src/components/fluence/WaveformStage.tsx');
const overlayPreviewPath = path.join(ROOT, 'web/src/components/fluence/OverlayPreview.tsx');
const bubblePagePath = path.join(ROOT, 'web/src/routes/BubblePage.tsx');
const generalPagePath = path.join(ROOT, 'web/src/routes/GeneralPage.tsx');
const providersPagePath = path.join(ROOT, 'web/src/routes/ProvidersPage.tsx');
const historyPagePath = path.join(ROOT, 'web/src/routes/HistoryPage.tsx');
const sidebarPath = path.join(ROOT, 'web/src/components/fluence/Sidebar.tsx');
const appTsxPath = path.join(ROOT, 'web/src/App.tsx');
const appCssPath = path.join(ROOT, 'web/src/app.css');
const uiCssPath = path.join(ROOT, 'web/src/ui.css');
const wizardCssPath = path.join(ROOT, 'web/src/wizard/wizard.css');

test('waveform.ts declares PreviewMode and MODE_COLORS for transcribe and agent modes', () => {
  const code = readFileSync(waveformPath, 'utf8');
  assert.match(code, /export type PreviewMode = 'transcribe' \| 'agent';/);
  assert.match(code, /transcribe:\s*\{\s*primary:\s*'#8B45D8',\s*forefront:\s*'#F1EAF5'\s*\}/);
  assert.match(code, /agent:\s*\{\s*primary:\s*'#0BD6E3',\s*forefront:\s*'#E6FBFC'\s*\}/);
  assert.match(code, /drawWaveformFrame\([\s\S]*?mode:\s*PreviewMode\s*=\s*'transcribe'/);
});

test('WaveformStage.tsx accepts mode prop and passes it to drawWaveformFrame', () => {
  const code = readFileSync(waveformStagePath, 'utf8');
  assert.match(code, /import\s*\{[\s\S]*?drawWaveformFrame[\s\S]*?type\s*PreviewMode[\s\S]*?\}\s*from\s*'\.\/waveform'/);
  assert.match(code, /mode\s*=\s*'transcribe'/);
  assert.match(code, /mode\?:\s*PreviewMode/);
  assert.match(code, /drawWaveformFrame\(canvas,\s*style,\s*phase,\s*level,\s*'full',\s*mode\)/);
  assert.match(code, /drawWaveformFrame\(canvas,\s*style,\s*0\.6,\s*0\.35,\s*'full',\s*mode\)/);
  assert.match(code, /\[style,\s*animate,\s*mode\]/);
});

test('OverlayPreview.tsx updates markupFor, toggles state-agent, and passes mode to drawWaveformFrame', () => {
  const code = readFileSync(overlayPreviewPath, 'utf8');
  assert.match(code, /import\s*\{[\s\S]*?type\s*PreviewMode[\s\S]*?\}\s*from\s*'\.\/waveform'/);
  assert.match(code, /function markupFor\([\s\S]*?mode:\s*PreviewMode\s*=\s*'transcribe'/);
  assert.match(code, /const modeClass = mode === 'agent' \? ' state-agent' : '';/);
  assert.match(code, /<div class="overlay-root active \$\{cornerClass\}\$\{styleClass\}\$\{glowClass\}\$\{modeClass\}">/);
  assert.match(code, /<span class="mode-badge">\$\{mode === 'agent' \? 'Agent' : 'Transcribe'\}<\/span>/);
  assert.match(code, /root\?\.classList\.toggle\('state-agent',\s*mode === 'agent'\);/);
  assert.match(code, /badge\.textContent = mode === 'agent' \? 'Agent' : 'Transcribe';/);
  assert.match(code, /drawWaveformFrame\(canvas,\s*waveformStyle,\s*0\.6,\s*0\.35,\s*tier,\s*mode\)/);
  assert.match(code, /\[tier,\s*glowOn,\s*position,\s*pillOn,\s*waveformStyle,\s*mode\]/);
});

test('BubblePage.tsx provides interactive PreviewModeToggle and honest autosave footer', () => {
  const code = readFileSync(bubblePagePath, 'utf8');
  assert.match(code, /const \[previewMode,\s*setPreviewMode\] = useState<PreviewMode>\('transcribe'\);/);
  assert.match(code, /<OverlayPreview[\s\S]*?mode=\{previewMode\}/);
  assert.match(code, /<WaveformStage[\s\S]*?mode=\{previewMode\}/);
  assert.match(code, /function PreviewModeToggle\(\{/);
  assert.match(code, /role="group"\s+aria-label="Preview mode"/);
  assert.match(code, /aria-pressed=\{mode === 'transcribe'\}/);
  assert.match(code, /aria-pressed=\{mode === 'agent'\}/);
  assert.match(code, /<PreviewModeToggle mode=\{previewMode\} onChange=\{setPreviewMode\} \/>/);
  // Autosave footer replaces save button
  assert.doesNotMatch(code, /save-bubble-btn/);
  assert.match(code, /<div className="page-actions page-actions-autosave">\s*<p className="autosave-hint">Changes save automatically\.<\/p>\s*<\/div>/);
  // Ctrl+S flush listener stays intact
  assert.match(code, /window\.addEventListener\('fluence:save-page',\s*onSave\);/);
});

test('GeneralPage.tsx has honest autosave footer on all tabs without save-general-btn', () => {
  const code = readFileSync(generalPagePath, 'utf8');
  assert.doesNotMatch(code, /save-general-btn/);
  assert.doesNotMatch(code, /tab !== 'appearance' &&/);
  assert.match(code, /<div className="page-actions page-actions-autosave">\s*<p className="autosave-hint">Changes save automatically\.<\/p>\s*<\/div>/);
  assert.match(code, /window\.addEventListener\('fluence:save-page',\s*onSave\);/);
});

test('ProvidersPage.tsx preserves save-providers-btn', () => {
  const code = readFileSync(providersPagePath, 'utf8');
  assert.match(code, /id="save-providers-btn"/);
});

test('HistoryPage.tsx implements accessible listitem, PageHeader actions, and error states', () => {
  const code = readFileSync(historyPagePath, 'utf8');
  // PageHeader with actions
  assert.match(code, /<PageHeader[\s\S]*?title="History"[\s\S]*?actions=\{/);
  assert.match(code, /id="export-history-btn"/);
  assert.match(code, /id="clear-history-btn"[\s\S]*?className="destructive-action"/);
  // Full-width SearchField in search row
  assert.match(code, /<div className="history-search-row">\s*<SearchField/);
  // WCAG nested interactive fix on .history-item
  const historyItemMatch = code.match(/className=\{cn\(\s*'history-item'[\s\S]*?>/);
  assert.ok(historyItemMatch, 'history-item element must exist');
  assert.match(historyItemMatch[0], /role="listitem"/);
  assert.doesNotMatch(historyItemMatch[0], /role="button"/);
  assert.doesNotMatch(historyItemMatch[0], /aria-expanded=/);
  assert.match(historyItemMatch[0], /aria-label=\{`Transcription from \$\{formatHistoryTimestamp\(entry\.timestamp\)\}`\}/);
  // Click guard against nested interactive elements
  assert.match(code, /target\?\.closest\('button,\s*\[role="checkbox"\],\s*\.candidate-word'\)/);
  // Accessible expand button
  assert.match(code, /className="history-expand-btn"/);
  assert.match(code, /aria-label=\{expanded \? 'Collapse transcription' : 'Expand transcription'\}/);
  assert.match(code, /aria-expanded=\{expanded\}/);
  // Mode badge variant
  assert.match(code, /<Badge variant=\{entry\.mode === 'agent' \? 'default' : 'outline'\}>\{entry\.mode\}<\/Badge>/);
  // List container semantics
  assert.match(code, /id="history-list"[\s\S]*?role="list"[\s\S]*?aria-label="Transcription history"/);
  // Loading & error components
  assert.match(code, /<div className="history-loading">\s*<Skeleton \/>/);
  assert.match(code, /<RouteErrorState[\s\S]*?id="history-load-error"[\s\S]*?title="Couldn't load history"/);
});

test('Sidebar.tsx clusters navigation into 4 scannable groups matching PAGE_ORDER', () => {
  const sidebarCode = readFileSync(sidebarPath, 'utf8');
  const appCode = readFileSync(appTsxPath, 'utf8');

  // Verify NAV_GROUPS labels
  assert.match(sidebarCode, /label:\s*'Home'/);
  assert.match(sidebarCode, /label:\s*'Capture'/);
  assert.match(sidebarCode, /label:\s*'Intelligence'/);
  assert.match(sidebarCode, /label:\s*'App'/);

  // Extract items from NAV_GROUPS
  const itemMatches = [...sidebarCode.matchAll(/page:\s*'([a-z]+)'/g)].map((m) => m[1]);
  // Extract PAGE_ORDER from App.tsx
  const pageOrderMatch = appCode.match(/const PAGE_ORDER: Route\[\] = \[([\s\S]*?)\];/);
  assert.ok(pageOrderMatch, 'PAGE_ORDER must exist in App.tsx');
  const expectedPages = [...pageOrderMatch[1].matchAll(/'([a-z]+)'/g)].map((m) => m[1]);

  assert.deepEqual(itemMatches, expectedPages, 'Sidebar navigation items must match PAGE_ORDER exactly');

  // Attributes preserved
  assert.match(sidebarCode, /id=\{`nav-\$\{item\.page\}`\}/);
  assert.match(sidebarCode, /data-page=\{item\.page\}/);
});

test('CSS hygiene: app.css tokens, ui.css semantic emphasis, and dead wizard removal', () => {
  const appCss = readFileSync(appCssPath, 'utf8');
  const uiCss = readFileSync(uiCssPath, 'utf8');
  const wizardCss = readFileSync(wizardCssPath, 'utf8');

  // app.css contains PR B styles
  assert.match(appCss, /\.bubble-legend/);
  assert.match(appCss, /\.bubble-legend-btn/);
  assert.match(appCss, /\.page-actions-autosave/);
  assert.match(appCss, /\.history-loading/);
  assert.match(appCss, /\.history-expand-btn/);
  assert.match(appCss, /\.sidebar\.collapsed \.sidebar-group \+ \.sidebar-group/);

  // ui.css uses var(--color-on-surface-emphasis) for selected command item
  assert.match(uiCss, /\.command-item\[data-selected='true'\][^{]*\{[^}]*color:\s*var\(--color-on-surface-emphasis\);/s);
  assert.match(uiCss, /\[data-theme="light"\] \.command-item\[data-selected='true'\][^{]*\{[^}]*color:\s*var\(--color-on-surface-emphasis\);/s);
  assert.doesNotMatch(uiCss, /\.command-item\[data-selected='true'\][^{]*\{[^}]*color:\s*#(?:FFFFFF|18181B);/s);

  // wizard.css dead classes removed
  const deadClasses = [
    'wiz-features-showcase',
    'wiz-welcome-list',
    'wizard-feature-info',
    'position-selector',
    'position-option',
    'theme-selector',
    'theme-option',
    'wiz-style-list',
    'wiz-style-stage',
  ];
  for (const cls of deadClasses) {
    assert.doesNotMatch(wizardCss, new RegExp(`\\.${cls}\\b`), `wizard.css must not contain .${cls}`);
  }

  // wizard.css keeps seg-selector and seg-option
  assert.match(wizardCss, /\.seg-selector\b/);
  assert.match(wizardCss, /\.seg-option\b/);
});

test('Sidebar flex scroll containment prevents overflow overlap and pins header/footer', () => {
  const uiCss = readFileSync(uiCssPath, 'utf8');

  // .sidebar-content declares overflow-y: auto; and min-height: 0;
  assert.match(
    uiCss,
    /\.sidebar-content\s*\{[^}]*min-height:\s*0;[^}]*overflow-y:\s*auto;[^}]*overflow-x:\s*hidden;/s,
    '.sidebar-content must declare min-height: 0 and overflow-y: auto',
  );

  // .sidebar-header and .sidebar-footer declare flex-shrink: 0;
  assert.match(
    uiCss,
    /\.sidebar-header,\s*\n?\.sidebar-footer\s*\{[^}]*flex-shrink:\s*0;/s,
    '.sidebar-header and .sidebar-footer must declare flex-shrink: 0',
  );
});
