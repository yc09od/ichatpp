// (app) layout — gates the authenticated routes (TODO [39]).
//
// On 401-without-refresh the apiFetch wrapper notifies via
// `onAuthFailure`; this layout subscribes and routes to /login.

'use client';

import { useEffect, type ReactNode } from 'react';
import { useRouter } from 'next/navigation';
import { TopBar } from '@/components/TopBar';
import { WebSocketProvider } from '@/components/providers/WebSocketProvider';
import { onAuthFailure } from '@/lib/api';

export default function AppLayout({ children }: { children: ReactNode }) {
  const router = useRouter();

  useEffect(() => {
    return onAuthFailure(() => router.push('/login'));
  }, [router]);

  return (
    <WebSocketProvider>
      <div className="min-h-screen flex flex-col bg-slate-50 dark:bg-slate-950">
        <TopBar />
        <main className="flex-1 min-h-0 flex flex-col">{children}</main>
      </div>
    </WebSocketProvider>
  );
}
