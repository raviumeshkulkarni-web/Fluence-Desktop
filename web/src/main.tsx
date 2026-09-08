import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './app.css';
import { App } from './App';

const root = document.getElementById('root');
if (!root) throw new Error('Missing #root mount node');
createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

