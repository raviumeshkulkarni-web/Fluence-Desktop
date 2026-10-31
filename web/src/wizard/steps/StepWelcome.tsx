import { FluenceSymbol, FluenceWordmark } from '@/components/fluence/FluenceLogo';
import { getLanguagePreference, getUiLanguage, reloadWithLanguage, resolveSystemLanguage, SYSTEM_LANGUAGE, t, UI_LANGUAGES } from '@/i18n';
import { setUiLanguage } from '../ipc';

// Step 1: Welcome. OOBE-minimal: brand mark, title, one-line promise, the
// three things Fluence does, Get Started. The mark is the brand master
// symbol (design/brand/master/fluence-symbol.svg) reproduced verbatim —
// never redrawn. The feature rows use typographic numerals, not icon glyphs:
// no lucide, no hand-rolled SVG, no emoji — just hairlines and type.
export function StepWelcome() {
  const preference = getLanguagePreference();
  const resolved = preference === SYSTEM_LANGUAGE ? resolveSystemLanguage() : preference;
  const resolvedName = getUiLanguage(resolved)?.nativeName ?? 'English';

  const onLanguageChange = async (code: string) => {
    await setUiLanguage(code).catch(() => undefined);
    reloadWithLanguage(code);
  };

  return (
    <>
      <div className="fluence-logo-container">
        <FluenceSymbol
          size={72}
          gradientId="fluence-brand-wizard"
          className="fluence-logo-svg"
        />
      </div>
      <h1
        className="step-title"
        tabIndex={-1}
        style={{
          fontWeight: 600,
          letterSpacing: '-0.03em',
          display: 'inline-flex',
          alignItems: 'baseline',
          justifyContent: 'center',
          gap: '8px',
        }}
      >
        {t('Welcome to')} <FluenceWordmark fontSize="inherit" />
      </h1>
      <p className="step-desc">
        {t('Your AI-powered voice typing assistant for Windows. Speak naturally, and Fluence transcribes your voice into any application, instantly.')}
      </p>

      <div className="wizard-language">
        <label className="wizard-language-label" htmlFor="wizard-language-select">
          {t('Language')}
        </label>
        <select
          id="wizard-language-select"
          className="wizard-language-select"
          value={preference}
          onChange={(event) => void onLanguageChange(event.target.value)}
        >
          <option value={SYSTEM_LANGUAGE}>
            {t('System default')} ({resolvedName})
          </option>
          {UI_LANGUAGES.map((language) => (
            <option key={language.code} value={language.code}>
              {language.nativeName}
            </option>
          ))}
        </select>
      </div>
    </>
  );
}
