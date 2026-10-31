import { useEffect, useRef, useSyncExternalStore } from 'react';

// Port of vanilla showToast (settings.js): max 3 stacked, newest last,
// 3s auto-dismiss, hover pauses, 800ms grace after mouse-leave, fade-out
// exit. Same container contract (.toast-container#toast-container).
export type ToastKind = 'success' | 'error' | 'info';

interface Toast {
  id: number;
  message: string;
  kind: ToastKind;
}

const KIND_ICON: Record<ToastKind, string> = {
  success: '✓',
  error: '✕',
  info: 'i',
};

const KIND_COLOR: Record<ToastKind, string> = {
  success: 'var(--color-success)',
  error: 'var(--color-error)',
  info: 'var(--color-on-surface-variant)',
};

class ToastStore {
  private toasts: Toast[] = [];
  private listeners = new Set<() => void>();
  private nextId = 1;

  subscribe = (cb: () => void) => {
    this.listeners.add(cb);
    return () => {
      this.listeners.delete(cb);
    };
  };

  getSnapshot = () => this.toasts;

  push(message: string, kind: ToastKind = 'info') {
    const list = [...this.toasts, { id: this.nextId++, message, kind }];
    while (list.length > 3) list.shift();
    this.toasts = list;
    this.emit();
  }

  dismiss(id: number) {
    this.toasts = this.toasts.filter((t) => t.id !== id);
    this.emit();
  }

  private emit() {
    this.listeners.forEach((cb) => cb());
  }
}

export const toastStore = new ToastStore();

export function toast(message: string, kind: ToastKind = 'info') {
  toastStore.push(message, kind);
}

function ToastItem({ item }: { item: Toast }) {
  const el = useRef<HTMLDivElement>(null);
  const timer = useRef<number | null>(null);

  const dismiss = () => {
    if (el.current) {
      el.current.style.animation = 'fade-out 0.3s ease forwards';
      window.setTimeout(() => toastStore.dismiss(item.id), 300);
    } else {
      toastStore.dismiss(item.id);
    }
  };

  const arm = (ms: number) => {
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(dismiss, ms);
  };

  useEffect(() => {
    arm(3000);
    return () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div
      ref={el}
      className={`toast ${item.kind}`}
      onMouseEnter={() => {
        if (timer.current !== null) window.clearTimeout(timer.current);
      }}
      onMouseLeave={() => arm(800)}
    >
      <span style={{ color: KIND_COLOR[item.kind], fontWeight: 700 }}>
        {KIND_ICON[item.kind]}
      </span>
      <span>{item.message}</span>
    </div>
  );
}

export function Toaster() {
  const toasts = useSyncExternalStore(
    toastStore.subscribe,
    toastStore.getSnapshot,
  );
  return (
    <div
      className="toast-container"
      id="toast-container"
      role="status"
      aria-live="polite"
    >
      {toasts.map((t) => (
        <ToastItem key={t.id} item={t} />
      ))}
    </div>
  );
}
