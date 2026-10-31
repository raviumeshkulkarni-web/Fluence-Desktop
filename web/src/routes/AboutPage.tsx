import { useEffect, useState } from 'react';
import { Button } from '@/components/ui/button';
import { Progress } from '@/components/ui/progress';
import { getAppVersion } from '@/ipc/tauri';
import { FluenceSymbol, FluenceWordmark } from '@/components/fluence/FluenceLogo';
import { updaterStore, useUpdater } from '@/ipc/updater';
import { t } from '@/i18n';

// Faithful port of the vanilla About surface (src/index.html #page-about +
// the About-card branch of setupUpdaterUI). Same DOM ids/classes, same copy,
// same 6 updater states. The action button is the shadcn Button primitive
// wearing the exact vanilla classes.
export function AboutPage({ onDownloadUpdate }: { onDownloadUpdate: () => void }) {
  const [appVersion, setAppVersion] = useState('1.0.0');
  const updater = useUpdater();

  useEffect(() => {
    getAppVersion().then(setAppVersion).catch(() => undefined);
  }, []);

  const card = renderCard();

  return (
    <section className="page active" id="page-about">
      <div className="page-header">
        <h1 className="page-title" tabIndex={-1}>{t('About Fluence')}</h1>
        <p className="page-subtitle">{t('Voice typing for Windows')}</p>
      </div>

      <div className="settings-section settings-section--unboxed">
        <div style={{ padding: 'var(--spacing-xl)', display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 'var(--spacing-lg)', textAlign: 'center' }}>
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 'var(--spacing-12)' }}>
            <FluenceSymbol
              size={48}
              gradientId="logo-grad-about"
            />
            <FluenceWordmark fontSize={26} />
          </div>
          <div className="text-muted text-body-md" style={{ marginTop: 'var(--spacing-xs)' }}>
            {t('Version')} <span id="about-version">{appVersion}</span>
          </div>
          <div className="text-body-md text-muted" style={{ maxWidth: 400, lineHeight: 1.7 }}>
            {t('Fluence turns your voice into text in any Windows app. Press your hotkey to dictate, polish it with Agent Mode, and keep every transcript on this device.')}
          </div>

          <div
            className={updater.state === 'downloading' ? 'update-card downloading' : 'update-card'}
            id="update-card"
            style={{ width: '100%', maxWidth: 420, marginTop: 'var(--spacing-12)', padding: 'var(--spacing-md)', borderRadius: 'var(--radius-md)', background: 'var(--color-surface-container)', border: '1px solid var(--color-outline-variant)', display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 'var(--spacing-10)' }}
          >
            <div style={{ display: 'flex', flexDirection: 'column', alignItems: 'center', gap: 'var(--spacing-xs)' }}>
              <div style={{ fontSize: 'var(--text-label-lg)', fontWeight: 600, color: 'var(--color-on-surface)' }} id="update-status-title">
                {card.title}
              </div>
              <div style={{ fontSize: 'var(--text-label-xs)', color: 'var(--color-on-surface-variant)' }} id="update-last-checked">
                {updater.lastCheckedText
                  ? t('Last checked: {time}', { time: updater.lastCheckedText })
                  : ''}
              </div>
            </div>
            <div
              style={{ fontSize: 'var(--text-label-lg)', color: 'var(--color-on-surface-variant)', display: card.desc ? 'block' : 'none', textAlign: card.descAlign, lineHeight: 1.4 }}
              id="update-status-desc"
            >
              {card.desc ?? ''}
            </div>

            <div id="update-progress-container" style={{ display: updater.state === 'downloading' ? 'block' : 'none', width: '100%', maxWidth: 280, marginTop: 'var(--spacing-xs)' }}>
              <Progress
                id="update-progress-track"
                aria-label={t('Update download progress')}
                value={updater.progress}
              />
              <div style={{ fontSize: 'var(--text-label-xs)', color: 'var(--color-outline)', textAlign: 'center', marginTop: 'var(--spacing-xs)' }} id="update-progress-text">
                {updater.progress}%
              </div>
            </div>

            <div style={{ display: 'flex', gap: 'var(--spacing-sm)', marginTop: 'var(--spacing-xs)' }} id="update-status-actions">
              <Button
                variant={card.btnPrimary ? 'primary' : 'secondary'}
                size="sm"
                id="update-action-btn"
                disabled={card.btnDisabled}
                onClick={card.onAction}
              >
                {card.btnText}
              </Button>
            </div>
          </div>

          <div className="text-label-sm text-muted">{t('Powered by Tauri v2 · Rust · WebView2')}</div>
        </div>
      </div>
    </section>
  );

  function renderCard(): {
    title: string;
    desc?: string;
    descAlign?: 'left' | 'center';
    btnText: string;
    btnDisabled: boolean;
    btnPrimary: boolean;
    onAction: () => void;
  } {
    const check = () => void updaterStore.checkForUpdates(true);
    const download = onDownloadUpdate;
    const restart = () => void updaterStore.restartApp();
    switch (updater.state) {
      case 'checking':
        return {
          title: t('Checking for updates…'),
          btnText: t('Checking…'),
          btnDisabled: true,
          btnPrimary: false,
          onAction: check,
        };
      case 'available':
        return {
          title: t('New Version Available (v{version})', { version: updater.version ?? '' }),
          desc: t('Review what’s included before downloading.'),
          descAlign: 'center' as const,
          btnText: t('Review update'), btnDisabled: false, btnPrimary: true, onAction: download,
        };
      case 'downloading':
        return {
          title: t('Downloading Update…'),
          btnText: t('Downloading {percent}%', { percent: updater.progress }),
          btnDisabled: true,
          btnPrimary: true,
          onAction: check,
        };
      case 'ready':
        return {
          title: t('Update Downloaded & Staged'),
          desc: t('Restart Fluence to apply the update.'),
          descAlign: 'center' as const,
          btnText: t('Restart Fluence'), btnDisabled: false, btnPrimary: true, onAction: restart,
        };
      case 'failed':
        return {
          title: t("Couldn't check for updates"),
          desc: updater.error ?? t('Please check your internet connection or try again later.'),
          descAlign: 'center' as const,
          btnText: t('Try Again'), btnDisabled: false, btnPrimary: false, onAction: check,
        };
      case 'idle':
      default:
        return {
          title: t("You're up to date"),
          btnText: t('Check for Updates'),
          btnDisabled: false,
          btnPrimary: false,
          onAction: check,
        };
    }
  }
}
