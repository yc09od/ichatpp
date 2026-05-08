// /admin/invitations — admin-only batch generate + list + stats + revoke
// (TODO [46]).
//
// Non-admin users get bounced to /chat (the API would 403 anyway, but
// the route guard saves a wasted round-trip and avoids flashing the
// admin UI). Plaintext codes appear exactly once, in a modal that the
// admin must explicitly close — once closed, they can only be recovered
// from the one-shot CSV download (also exposed in the modal).

'use client';

import Link from 'next/link';
import { useEffect, useState } from 'react';
import { useRouter } from 'next/navigation';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { ApiError, apiFetch } from '@/lib/api';
import { useSelfUser } from '@/components/TopBar';

interface GeneratedInvitation {
  id: string;
  code: string;
  code_prefix: string;
  expires_at: string;
}

interface GenerateResponse {
  invitations: GeneratedInvitation[];
  download_url: string | null;
}

interface InvitationListItem {
  id: string;
  code_prefix: string | null;
  masked: string;
  status: string;
  used_by: string | null;
  created_at: string;
  expires_at: string;
  notes: string | null;
}

interface StatsCounts {
  total: number;
  used: number;
  unused: number;
  expired: number;
}

export default function AdminInvitationsPage() {
  const router = useRouter();
  const { data: me, isLoading: meLoading } = useSelfUser();
  const queryClient = useQueryClient();

  const [count, setCount] = useState(10);
  const [notes, setNotes] = useState('');
  const [expiresInDays, setExpiresInDays] = useState(7);
  const [generated, setGenerated] = useState<GenerateResponse | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [statusFilter, setStatusFilter] = useState<string>('');

  // Route guard. The API also enforces 403, but bouncing here saves the
  // user from staring at error toasts.
  useEffect(() => {
    if (!meLoading && me && me.role !== 'admin') {
      router.replace('/chat');
    }
  }, [me, meLoading, router]);

  const stats = useQuery<StatsCounts>({
    queryKey: ['invitations', 'stats'],
    queryFn: () => apiFetch<StatsCounts>('/api/invitations/stats'),
    enabled: me?.role === 'admin',
  });

  const list = useQuery<InvitationListItem[]>({
    queryKey: ['invitations', 'list', statusFilter],
    queryFn: () =>
      apiFetch<InvitationListItem[]>(
        statusFilter
          ? `/api/invitations?status=${encodeURIComponent(statusFilter)}`
          : '/api/invitations',
      ),
    enabled: me?.role === 'admin',
  });

  const generate = useMutation({
    mutationFn: (payload: {
      count: number;
      notes?: string;
      expires_in_days?: number;
    }) =>
      apiFetch<GenerateResponse>('/api/invitations/generate', {
        json: payload,
        method: 'POST',
      }),
    onSuccess: (resp) => {
      setGenerated(resp);
      setErrorMsg(null);
      queryClient.invalidateQueries({ queryKey: ['invitations'] });
    },
    onError: (e) => setErrorMsg(e instanceof ApiError ? e.message : '生成失败'),
  });

  const revoke = useMutation({
    mutationFn: (id: string) =>
      apiFetch(`/api/invitations/${id}`, { method: 'DELETE' }),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: ['invitations'] }),
  });

  if (meLoading) {
    return (
      <div className="flex-1 flex items-center justify-center text-slate-500">
        加载中…
      </div>
    );
  }
  if (!me || me.role !== 'admin') {
    return (
      <div className="flex-1 flex items-center justify-center">
        <div className="text-center">
          <p className="text-lg font-medium text-red-600">403 — 仅管理员可访问</p>
          <Link href="/chat" className="text-sm text-blue-600 hover:underline mt-2 block">
            返回聊天
          </Link>
        </div>
      </div>
    );
  }

  return (
    <div className="flex-1 overflow-y-auto p-6 max-w-5xl mx-auto w-full">
      <h1 className="text-2xl font-semibold mb-6 text-slate-900 dark:text-slate-100">
        邀请码管理
      </h1>

      {/* Stats */}
      <section className="grid grid-cols-2 md:grid-cols-4 gap-3 mb-8">
        <StatCard label="总数" value={stats.data?.total} />
        <StatCard label="未使用" value={stats.data?.unused} />
        <StatCard label="已使用" value={stats.data?.used} />
        <StatCard label="已过期" value={stats.data?.expired} />
      </section>

      {/* Generate */}
      <section className="mb-8 rounded-lg border border-slate-200 dark:border-slate-800 bg-white dark:bg-slate-900 p-4">
        <h2 className="text-lg font-medium mb-4 text-slate-900 dark:text-slate-100">
          批量生成
        </h2>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            setErrorMsg(null);
            const payload: {
              count: number;
              notes?: string;
              expires_in_days?: number;
            } = { count };
            if (notes.trim()) payload.notes = notes.trim();
            if (expiresInDays) payload.expires_in_days = expiresInDays;
            generate.mutate(payload);
          }}
          className="grid grid-cols-1 md:grid-cols-4 gap-3 items-end"
        >
          <div>
            <label className="block text-xs font-medium mb-1 text-slate-600 dark:text-slate-400">
              数量 (1-100)
            </label>
            <input
              type="number"
              min={1}
              max={100}
              value={count}
              onChange={(e) => setCount(Number(e.target.value))}
              className="w-full rounded-md border border-slate-300 dark:border-slate-700 bg-white dark:bg-slate-800 px-3 py-2 text-sm"
            />
          </div>
          <div>
            <label className="block text-xs font-medium mb-1 text-slate-600 dark:text-slate-400">
              过期天数
            </label>
            <input
              type="number"
              min={1}
              max={365}
              value={expiresInDays}
              onChange={(e) => setExpiresInDays(Number(e.target.value))}
              className="w-full rounded-md border border-slate-300 dark:border-slate-700 bg-white dark:bg-slate-800 px-3 py-2 text-sm"
            />
          </div>
          <div className="md:col-span-2">
            <label className="block text-xs font-medium mb-1 text-slate-600 dark:text-slate-400">
              备注 (可选)
            </label>
            <input
              type="text"
              value={notes}
              onChange={(e) => setNotes(e.target.value)}
              maxLength={255}
              className="w-full rounded-md border border-slate-300 dark:border-slate-700 bg-white dark:bg-slate-800 px-3 py-2 text-sm"
            />
          </div>
          <button
            type="submit"
            disabled={generate.isPending}
            className="md:col-span-4 rounded-md bg-blue-600 text-white py-2 text-sm font-medium hover:bg-blue-700 disabled:opacity-50"
          >
            {generate.isPending ? '生成中…' : '生成'}
          </button>
        </form>
        {errorMsg && (
          <div className="rounded-md bg-red-50 dark:bg-red-950/40 px-3 py-2 text-sm text-red-700 dark:text-red-300 mt-3">
            {errorMsg}
          </div>
        )}
      </section>

      {/* List */}
      <section className="rounded-lg border border-slate-200 dark:border-slate-800 bg-white dark:bg-slate-900">
        <div className="p-4 border-b border-slate-200 dark:border-slate-800 flex items-center justify-between">
          <h2 className="text-lg font-medium text-slate-900 dark:text-slate-100">
            邀请码列表
          </h2>
          <select
            value={statusFilter}
            onChange={(e) => setStatusFilter(e.target.value)}
            className="rounded-md border border-slate-300 dark:border-slate-700 bg-white dark:bg-slate-800 px-2 py-1 text-sm"
          >
            <option value="">全部</option>
            <option value="unused">未使用</option>
            <option value="used">已使用</option>
            <option value="expired">已过期</option>
            <option value="revoked">已撤销</option>
          </select>
        </div>
        {list.isLoading ? (
          <div className="p-4 text-sm text-slate-500">加载中…</div>
        ) : (
          <table className="w-full text-sm">
            <thead className="text-left text-xs text-slate-500 border-b border-slate-200 dark:border-slate-800">
              <tr>
                <th className="px-4 py-2 font-medium">编码</th>
                <th className="px-4 py-2 font-medium">状态</th>
                <th className="px-4 py-2 font-medium">备注</th>
                <th className="px-4 py-2 font-medium">过期时间</th>
                <th className="px-4 py-2 font-medium text-right">操作</th>
              </tr>
            </thead>
            <tbody>
              {list.data?.map((inv) => (
                <tr key={inv.id} className="border-b border-slate-100 dark:border-slate-800">
                  <td className="px-4 py-2 font-mono text-xs">{inv.masked}</td>
                  <td className="px-4 py-2">
                    <StatusPill status={inv.status} />
                  </td>
                  <td className="px-4 py-2 text-slate-600 dark:text-slate-400 truncate max-w-xs">
                    {inv.notes ?? ''}
                  </td>
                  <td className="px-4 py-2 text-slate-600 dark:text-slate-400">
                    {new Date(inv.expires_at).toLocaleString()}
                  </td>
                  <td className="px-4 py-2 text-right">
                    {inv.status === 'unused' && (
                      <button
                        type="button"
                        onClick={() => {
                          if (confirm('撤销该邀请码？')) revoke.mutate(inv.id);
                        }}
                        className="text-red-600 hover:underline text-xs"
                      >
                        撤销
                      </button>
                    )}
                  </td>
                </tr>
              ))}
              {list.data && list.data.length === 0 && (
                <tr>
                  <td colSpan={5} className="px-4 py-6 text-center text-sm text-slate-500">
                    没有匹配的邀请码
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        )}
      </section>

      {/* Generated modal */}
      {generated && (
        <GeneratedModal
          payload={generated}
          onClose={() => setGenerated(null)}
        />
      )}
    </div>
  );
}

function StatCard({ label, value }: { label: string; value: number | undefined }) {
  return (
    <div className="rounded-lg border border-slate-200 dark:border-slate-800 bg-white dark:bg-slate-900 p-4">
      <div className="text-xs text-slate-500">{label}</div>
      <div className="text-2xl font-semibold text-slate-900 dark:text-slate-100 mt-1">
        {value ?? '—'}
      </div>
    </div>
  );
}

function StatusPill({ status }: { status: string }) {
  const colour =
    status === 'unused'
      ? 'bg-blue-100 text-blue-700 dark:bg-blue-900/40 dark:text-blue-300'
      : status === 'used'
      ? 'bg-green-100 text-green-700 dark:bg-green-900/40 dark:text-green-300'
      : status === 'expired'
      ? 'bg-slate-100 text-slate-600 dark:bg-slate-800 dark:text-slate-400'
      : 'bg-red-100 text-red-700 dark:bg-red-900/40 dark:text-red-300';
  return (
    <span className={`inline-block rounded px-2 py-0.5 text-xs font-medium ${colour}`}>
      {status}
    </span>
  );
}

function GeneratedModal({
  payload,
  onClose,
}: {
  payload: GenerateResponse;
  onClose: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const allCodes = payload.invitations.map((i) => i.code).join('\n');

  return (
    <div className="fixed inset-0 z-50 bg-slate-900/60 flex items-center justify-center p-4">
      <div className="w-full max-w-2xl rounded-xl bg-white dark:bg-slate-900 shadow-xl border border-slate-200 dark:border-slate-800 max-h-[85vh] flex flex-col">
        <header className="p-4 border-b border-slate-200 dark:border-slate-800 flex items-center justify-between">
          <div>
            <h3 className="text-lg font-semibold text-slate-900 dark:text-slate-100">
              生成成功 — 仅显示一次
            </h3>
            <p className="text-xs text-slate-500 mt-1">
              关闭后无法再次取回明文，请复制保存或下载 CSV。
            </p>
          </div>
        </header>
        <div className="p-4 overflow-y-auto flex-1">
          <pre className="whitespace-pre font-mono text-xs bg-slate-50 dark:bg-slate-950 rounded-md p-3 border border-slate-200 dark:border-slate-800 max-h-80 overflow-y-auto">
            {allCodes}
          </pre>
        </div>
        <footer className="p-4 border-t border-slate-200 dark:border-slate-800 flex flex-wrap gap-2 justify-end">
          <button
            type="button"
            onClick={async () => {
              try {
                await navigator.clipboard.writeText(allCodes);
                setCopied(true);
                setTimeout(() => setCopied(false), 2000);
              } catch {
                // ignore — user can manually select.
              }
            }}
            className="rounded-md border border-slate-300 dark:border-slate-700 px-3 py-1.5 text-sm hover:bg-slate-100 dark:hover:bg-slate-800"
          >
            {copied ? '已复制' : '复制全部'}
          </button>
          {payload.download_url && (
            <a
              href={payload.download_url}
              className="rounded-md border border-slate-300 dark:border-slate-700 px-3 py-1.5 text-sm hover:bg-slate-100 dark:hover:bg-slate-800"
            >
              下载 CSV (一次性)
            </a>
          )}
          <button
            type="button"
            onClick={onClose}
            className="rounded-md bg-blue-600 text-white px-3 py-1.5 text-sm hover:bg-blue-700"
          >
            我已保存，关闭
          </button>
        </footer>
      </div>
    </div>
  );
}
