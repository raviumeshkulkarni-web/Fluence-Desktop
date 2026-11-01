import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './wizard.css';
import { Wizard } from './Wizard';
import { initI18n } from '@/i18n';

async function boot() {
  await initI18n();

  const root = document.getElementById('wizard-root');
  if (!root) throw new Error('Missing #wizard-root mount node');
  createRoot(root).render(
    <StrictMode>
      <Wizard />
    </StrictMode>,
  );
}

void boot();
