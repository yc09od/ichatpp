// Shared layout for the unauthenticated routes (login + register).
// Renders a centred card on a quiet background — matches the
// architecture sketch's "auth surface" treatment.

import type { ReactNode } from 'react';

export default function AuthLayout({ children }: { children: ReactNode }) {
  return (
    <div className="min-h-screen flex items-center justify-center bg-slate-50 dark:bg-slate-950 px-4 py-12">
      <main className="w-full max-w-md bg-white dark:bg-slate-900 shadow-md rounded-xl p-8 border border-slate-200 dark:border-slate-800">
        {children}
      </main>
    </div>
  );
}
