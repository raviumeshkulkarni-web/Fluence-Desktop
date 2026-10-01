import * as React from 'react';
import { Search } from 'lucide-react';
import { Input } from '@/components/ui/input';
import { cn } from '@/lib/cn';

// The one search field for the whole app: shared Input + the magnifier in the
// padding gutter. History and the app picker used to build this separately
// (one hand-rolled wrapper + CSS-positioned icon, one bare labelled Input), so
// the icon, inset, and type size drifted apart. Paint lives in app.css
// (.search-field) and inherits the global input base for surface/border/radius,
// exactly like every other Input in the kit.
//
// Deliberately NOT a shadcn InputGroup: the project deliberately runs without
// Tailwind (vite.config.ts) and has no components.json, so this composes the
// existing primitives instead of importing a Tailwind-flavoured port.
//
// Accessibility: the icon is decorative (aria-hidden) and the field takes no
// label of its own — callers pass aria-label, or pair it with Field's
// label/htmlFor, so the accessible name is never doubled up.

export interface SearchFieldProps
  extends Omit<React.InputHTMLAttributes<HTMLInputElement>, 'type'> {
  /** Wrapper class for layout (flex/width) at the call site. */
  className?: string;
}

export const SearchField = React.forwardRef<HTMLInputElement, SearchFieldProps>(
  ({ className, ...props }, ref) => (
    <div className={cn('search-field', className)}>
      <Search size={15} strokeWidth={2} aria-hidden="true" />
      <Input ref={ref} type="search" {...props} />
    </div>
  ),
);
SearchField.displayName = 'SearchField';
