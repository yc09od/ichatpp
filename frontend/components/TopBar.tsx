// Top navigation bar — shown across the (app) routes (TODO [39]).
//
// Right-aligned avatar opens a tiny dropdown (个人中心 / 管理员邀请码 /
// 登出). Implemented as a controlled <details>-style state so we don't
// need a popover library; clicks outside close it via a `mousedown`
// listener on document.

'use client';

import Link from 'next/link';
import { useRouter } from 'next/navigation';
import { useEffect, useRef, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { apiFetch } from '@/lib/api';
import { wsClient } from '@/lib/websocket';

interface SelfUser {
  id: string;
  account_code: string;
  email: string;
  nickname: string | null;
  avatar_url: string | null;
  signature: string | null;
  is_visible: boolean;
  role: string;
  created_at: string;
}

export function useSelfUser() {
  return useQuery<SelfUser>({
    queryKey: ['me'],
    queryFn: () => apiFetch<SelfUser>('/api/users/me'),
    staleTime: 5 * 60 * 1000,
  });
}

export function TopBar() {
  const router = useRouter();
  const queryClient = useQueryClient();
  const { data: me } = useSelfUser();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDocClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener('mousedown', onDocClick);
    return () => document.removeEventListener('mousedown', onDocClick);
  }, [open]);

  async function logout() {
    setOpen(false);
    try {
      await apiFetch('/api/auth/logout', { method: 'POST' });
    } catch {
      // even if the server-side delete fails, blow away local state.
    }
    wsClient.disconnect();
    queryClient.clear();
    router.push('/login');
  }

  const display = me?.nickname || me?.email || '加载中…';
  const initial = (me?.nickname || me?.email || '?').charAt(0).toUpperCase();

  return (
    <header className="h-14 border-b border-slate-200 dark:border-slate-800 bg-white dark:bg-slate-900 flex items-center justify-between px-4">
      <Link href="/chat" className="font-semibold text-slate-900 dark:text-slate-100">
        ichatpp
      </Link>
      <div className="relative" ref={ref}>
        <button
          type="button"
          onClick={() => setOpen((v) => !v)}
          className="flex items-center gap-2 rounded-full hover:bg-slate-100 dark:hover:bg-slate-800 px-2 py-1 transition-colors"
        >
          {me?.avatar_url ? (
            // Inline avatar: <img> not next/image so user-supplied
            // remote URLs (MinIO) don't require next.config domain
            // whitelisting.
            // eslint-disable-next-line @next/next/no-img-element
            <img
              src={me.avatar_url}
              alt=""
              className="w-8 h-8 rounded-full object-cover"
            />
          ) : (
            <div className="w-8 h-8 rounded-full bg-blue-600 text-white flex items-center justify-center text-sm font-medium">
              {initial}
            </div>
          )}
          <span className="hidden sm:inline text-sm text-slate-700 dark:text-slate-300">
            {display}
          </span>
        </button>
        {open && (
          <div className="absolute right-0 top-full mt-2 w-48 rounded-md border border-slate-200 dark:border-slate-800 bg-white dark:bg-slate-900 shadow-lg z-30 py-1">
            <Link
              href="/profile"
              onClick={() => setOpen(false)}
              className="block px-3 py-2 text-sm text-slate-700 dark:text-slate-300 hover:bg-slate-100 dark:hover:bg-slate-800"
            >
              个人中心
            </Link>
            {me?.role === 'admin' && (
              <Link
                href="/admin/invitations"
                onClick={() => setOpen(false)}
                className="block px-3 py-2 text-sm text-slate-700 dark:text-slate-300 hover:bg-slate-100 dark:hover:bg-slate-800"
              >
                邀请码管理
              </Link>
            )}
            <button
              type="button"
              onClick={logout}
              className="block w-full text-left px-3 py-2 text-sm text-red-600 hover:bg-slate-100 dark:hover:bg-slate-800"
            >
              退出登录
            </button>
          </div>
        )}
      </div>
    </header>
  );
}
