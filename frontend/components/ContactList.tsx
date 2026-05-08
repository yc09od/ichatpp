// Friend list — left rail of the chat view (TODO [40]).
//
// Data flow:
//  - TanStack Query owns the cached `/api/friends` snapshot.
//  - WebSocket `user_online` / `user_offline` events flip a tiny local
//    Set so the dot updates instantly without re-fetching the list.
//  - The list is sorted server-side (recent message first); we don't
//    re-sort client-side so the user-visible order matches the cursor
//    we'd send to history queries.
//  - Inline "add friend" + "pending requests" panels share the same
//    sidebar real estate via a `view` switch — keeps the chrome density
//    low and avoids an extra modal/portal layer.

'use client';

import Link from 'next/link';
import { useMemo, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { ApiError, apiFetch } from '@/lib/api';
import { useWebSocketEvent } from '@/components/providers/WebSocketProvider';

interface FriendSummary {
  id: string;
  account_code: string;
  nickname: string | null;
  avatar_url: string | null;
  is_visible: boolean;
  last_message_at: string | null;
}

interface FriendListResponse {
  friends: FriendSummary[];
}

interface PendingRequestPeer {
  id: string;
  account_code: string;
  nickname: string | null;
  avatar_url: string | null;
}

interface PendingRequest {
  id: string;
  from_user: PendingRequestPeer;
  created_at: string;
}

interface PendingListResponse {
  requests: PendingRequest[];
}

export function useFriends() {
  return useQuery<FriendListResponse, Error, FriendSummary[]>({
    queryKey: ['friends'],
    queryFn: () => apiFetch<FriendListResponse>('/api/friends'),
    select: (r) => r.friends,
    staleTime: 30 * 1000,
  });
}

// Pending inbound requests. No WS push from the backend yet, so a 60s
// poll keeps the badge fresh enough that an accepted handshake shows
// up without a manual refresh.
function usePendingRequests() {
  return useQuery<PendingListResponse, Error, PendingRequest[]>({
    queryKey: ['friend-requests', 'pending'],
    queryFn: () => apiFetch<PendingListResponse>('/api/friends/requests/pending'),
    select: (r) => r.requests,
    refetchInterval: 60 * 1000,
    staleTime: 30 * 1000,
  });
}

type View = 'friends' | 'add' | 'requests';

interface ContactListProps {
  /** Currently selected friend id, for highlight. */
  activeFriendId?: string;
  /** Optional close-on-select hook for the mobile drawer. */
  onSelect?: () => void;
}

export function ContactList({ activeFriendId, onSelect }: ContactListProps) {
  const [view, setView] = useState<View>('friends');

  return (
    <aside className="h-full flex flex-col bg-white dark:bg-slate-900 border-r border-slate-200 dark:border-slate-800">
      {view === 'friends' && (
        <FriendsView
          activeFriendId={activeFriendId}
          onSelect={onSelect}
          onOpenAdd={() => setView('add')}
          onOpenRequests={() => setView('requests')}
        />
      )}
      {view === 'add' && <AddFriendView onBack={() => setView('friends')} />}
      {view === 'requests' && <PendingRequestsView onBack={() => setView('friends')} />}
    </aside>
  );
}

// ────────────────────────────────────────────────────────────────────────
// Friends list view
// ────────────────────────────────────────────────────────────────────────

function FriendsView({
  activeFriendId,
  onSelect,
  onOpenAdd,
  onOpenRequests,
}: {
  activeFriendId?: string;
  onSelect?: () => void;
  onOpenAdd: () => void;
  onOpenRequests: () => void;
}) {
  const { data: friends, isLoading, error } = useFriends();
  const { data: pending } = usePendingRequests();
  const pendingCount = pending?.length ?? 0;

  const [search, setSearch] = useState('');
  const [onlineSet, setOnlineSet] = useState<Set<string>>(new Set());

  // Live presence updates. We don't try to seed `onlineSet` from
  // history — backend pushes `user_online` for every friend at upgrade
  // time (presence broadcast in TODO [27]).
  useWebSocketEvent((ev) => {
    if (ev.type === 'user_online') {
      setOnlineSet((s) => {
        if (s.has(ev.user_id)) return s;
        const next = new Set(s);
        next.add(ev.user_id);
        return next;
      });
    } else if (ev.type === 'user_offline') {
      setOnlineSet((s) => {
        if (!s.has(ev.user_id)) return s;
        const next = new Set(s);
        next.delete(ev.user_id);
        return next;
      });
    }
  });

  const filtered = useMemo(() => {
    if (!friends) return [];
    const q = search.trim().toLowerCase();
    if (!q) return friends;
    return friends.filter((f) => {
      const name = (f.nickname ?? '').toLowerCase();
      return name.includes(q) || f.account_code.includes(q);
    });
  }, [friends, search]);

  return (
    <>
      <div className="p-3 border-b border-slate-200 dark:border-slate-800 flex items-center gap-2">
        <input
          type="search"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="搜索昵称或账号"
          className="flex-1 min-w-0 rounded-md border border-slate-300 dark:border-slate-700 bg-slate-50 dark:bg-slate-800 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 focus:outline-none focus:ring-2 focus:ring-blue-500"
        />
        <button
          type="button"
          onClick={onOpenRequests}
          aria-label="好友请求"
          title="好友请求"
          className="relative shrink-0 h-9 w-9 rounded-md border border-slate-300 dark:border-slate-700 text-slate-600 dark:text-slate-300 hover:bg-slate-100 dark:hover:bg-slate-800 flex items-center justify-center"
        >
          <BellIcon />
          {pendingCount > 0 && (
            <span className="absolute -top-1 -right-1 min-w-[18px] h-[18px] px-1 rounded-full bg-red-500 text-white text-[11px] leading-[18px] text-center">
              {pendingCount > 99 ? '99+' : pendingCount}
            </span>
          )}
        </button>
        <button
          type="button"
          onClick={onOpenAdd}
          aria-label="添加好友"
          title="添加好友"
          className="shrink-0 h-9 w-9 rounded-md border border-slate-300 dark:border-slate-700 text-slate-600 dark:text-slate-300 hover:bg-slate-100 dark:hover:bg-slate-800 flex items-center justify-center text-lg leading-none"
        >
          +
        </button>
      </div>
      <div className="flex-1 overflow-y-auto">
        {isLoading && (
          <div className="p-4 text-sm text-slate-500">加载好友列表…</div>
        )}
        {error && (
          <div className="p-4 text-sm text-red-600">加载失败，请刷新</div>
        )}
        {!isLoading && filtered.length === 0 && (
          <div className="p-4 text-sm text-slate-500">
            {search ? '没有匹配的好友' : '还没有好友，点击右上角 + 添加一个吧'}
          </div>
        )}
        <ul>
          {filtered.map((f) => {
            const isActive = f.id === activeFriendId;
            const isOnline = onlineSet.has(f.id);
            return (
              <li key={f.id}>
                <Link
                  href={`/chat/${f.id}`}
                  onClick={onSelect}
                  className={`flex items-center gap-3 px-3 py-3 border-b border-slate-100 dark:border-slate-800 hover:bg-slate-50 dark:hover:bg-slate-800 transition-colors ${
                    isActive ? 'bg-blue-50 dark:bg-blue-950/40' : ''
                  }`}
                >
                  <div className="relative shrink-0">
                    {f.avatar_url ? (
                      // eslint-disable-next-line @next/next/no-img-element
                      <img
                        src={f.avatar_url}
                        alt=""
                        className="w-10 h-10 rounded-full object-cover"
                      />
                    ) : (
                      <div className="w-10 h-10 rounded-full bg-slate-300 dark:bg-slate-600 text-white flex items-center justify-center text-sm font-medium">
                        {(f.nickname ?? f.account_code).charAt(0).toUpperCase()}
                      </div>
                    )}
                    <span
                      className={`absolute -bottom-0.5 -right-0.5 w-3 h-3 rounded-full border-2 border-white dark:border-slate-900 ${
                        isOnline ? 'bg-green-500' : 'bg-slate-400'
                      }`}
                      aria-label={isOnline ? '在线' : '离线'}
                    />
                  </div>
                  <div className="min-w-0 flex-1">
                    <div className="text-sm font-medium text-slate-900 dark:text-slate-100 truncate">
                      {f.nickname || f.account_code}
                    </div>
                    <div className="text-xs text-slate-500 truncate">
                      {f.account_code}
                    </div>
                  </div>
                </Link>
              </li>
            );
          })}
        </ul>
      </div>
    </>
  );
}

// ────────────────────────────────────────────────────────────────────────
// Add-friend view
// ────────────────────────────────────────────────────────────────────────

function AddFriendView({ onBack }: { onBack: () => void }) {
  const queryClient = useQueryClient();
  const [code, setCode] = useState('');
  const [feedback, setFeedback] = useState<
    { kind: 'success' | 'error'; msg: string } | null
  >(null);

  const mutation = useMutation({
    mutationFn: (account_code: string) =>
      apiFetch('/api/friends/requests', {
        method: 'POST',
        json: { account_code },
      }),
    onSuccess: () => {
      setFeedback({ kind: 'success', msg: '好友请求已发送，等待对方接受' });
      setCode('');
      // The receiver's pending list cache will refetch on its own; we
      // invalidate the sender-side too in case a future endpoint surfaces
      // outbound requests.
      queryClient.invalidateQueries({ queryKey: ['friend-requests'] });
    },
    onError: (e: Error) => {
      const msg = e instanceof ApiError ? friendlyError(e) : '发送失败';
      setFeedback({ kind: 'error', msg });
    },
  });

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    setFeedback(null);
    const trimmed = code.trim();
    if (!/^\d{10}$/.test(trimmed)) {
      setFeedback({ kind: 'error', msg: '账号码必须是 10 位数字' });
      return;
    }
    mutation.mutate(trimmed);
  }

  return (
    <div className="flex flex-col h-full">
      <PanelHeader title="添加好友" onBack={onBack} />
      <form onSubmit={handleSubmit} className="p-4 flex flex-col gap-3">
        <label className="text-xs text-slate-500" htmlFor="friend-account-code">
          对方账号码
        </label>
        <input
          id="friend-account-code"
          type="text"
          inputMode="numeric"
          autoComplete="off"
          maxLength={10}
          value={code}
          onChange={(e) => setCode(e.target.value.replace(/\D/g, ''))}
          placeholder="10 位数字账号"
          className="rounded-md border border-slate-300 dark:border-slate-700 bg-slate-50 dark:bg-slate-800 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 focus:outline-none focus:ring-2 focus:ring-blue-500"
        />
        <button
          type="submit"
          disabled={mutation.isPending}
          className="rounded-md bg-blue-600 hover:bg-blue-700 disabled:opacity-60 disabled:cursor-not-allowed text-white px-4 py-2 text-sm font-medium"
        >
          {mutation.isPending ? '发送中…' : '发送请求'}
        </button>
        {feedback && (
          <p
            className={`text-sm ${
              feedback.kind === 'success' ? 'text-green-600' : 'text-red-600'
            }`}
          >
            {feedback.msg}
          </p>
        )}
        <p className="text-xs text-slate-500 mt-2">
          提示：在「个人中心」可以查看自己的账号码，复制给好友。
        </p>
      </form>
    </div>
  );
}

// Map server error codes/messages to friendlier copy. Falls back to the
// raw message when nothing matches so we don't swallow novel errors.
function friendlyError(e: ApiError): string {
  if (e.status === 404) return '没有找到这个账号';
  if (e.status === 409) {
    if (/already friends/i.test(e.message)) return '你们已经是好友';
    if (/pending/i.test(e.message)) return '已经存在一个待处理的请求';
    return '请求冲突';
  }
  if (e.status === 400) {
    if (/yourself/i.test(e.message)) return '不能添加自己为好友';
  }
  return e.message || '发送失败';
}

// ────────────────────────────────────────────────────────────────────────
// Pending-requests view
// ────────────────────────────────────────────────────────────────────────

function PendingRequestsView({ onBack }: { onBack: () => void }) {
  const queryClient = useQueryClient();
  const { data: requests, isLoading, error } = usePendingRequests();

  const respond = useMutation({
    mutationFn: ({ id, action }: { id: string; action: 'accept' | 'reject' }) =>
      apiFetch(`/api/friends/requests/${id}`, {
        method: 'PUT',
        json: { action },
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['friend-requests', 'pending'] });
      queryClient.invalidateQueries({ queryKey: ['friends'] });
    },
  });

  return (
    <div className="flex flex-col h-full">
      <PanelHeader title="好友请求" onBack={onBack} />
      <div className="flex-1 overflow-y-auto">
        {isLoading && (
          <div className="p-4 text-sm text-slate-500">加载中…</div>
        )}
        {error && (
          <div className="p-4 text-sm text-red-600">加载失败，请重试</div>
        )}
        {!isLoading && (!requests || requests.length === 0) && (
          <div className="p-4 text-sm text-slate-500">暂无待处理请求</div>
        )}
        <ul>
          {requests?.map((r) => {
            const display = r.from_user.nickname || r.from_user.account_code;
            const initial = display.charAt(0).toUpperCase();
            const pending =
              respond.isPending && respond.variables?.id === r.id;
            return (
              <li
                key={r.id}
                className="flex items-center gap-3 px-3 py-3 border-b border-slate-100 dark:border-slate-800"
              >
                {r.from_user.avatar_url ? (
                  // eslint-disable-next-line @next/next/no-img-element
                  <img
                    src={r.from_user.avatar_url}
                    alt=""
                    className="w-10 h-10 rounded-full object-cover shrink-0"
                  />
                ) : (
                  <div className="w-10 h-10 rounded-full bg-slate-300 dark:bg-slate-600 text-white flex items-center justify-center text-sm font-medium shrink-0">
                    {initial}
                  </div>
                )}
                <div className="min-w-0 flex-1">
                  <div className="text-sm font-medium text-slate-900 dark:text-slate-100 truncate">
                    {display}
                  </div>
                  <div className="text-xs text-slate-500 truncate">
                    {r.from_user.account_code}
                  </div>
                </div>
                <div className="flex gap-1 shrink-0">
                  <button
                    type="button"
                    disabled={pending}
                    onClick={() => respond.mutate({ id: r.id, action: 'accept' })}
                    className="text-xs px-2.5 py-1.5 rounded-md bg-blue-600 hover:bg-blue-700 disabled:opacity-60 text-white"
                  >
                    接受
                  </button>
                  <button
                    type="button"
                    disabled={pending}
                    onClick={() => respond.mutate({ id: r.id, action: 'reject' })}
                    className="text-xs px-2.5 py-1.5 rounded-md border border-slate-300 dark:border-slate-700 text-slate-600 dark:text-slate-300 hover:bg-slate-100 dark:hover:bg-slate-800 disabled:opacity-60"
                  >
                    拒绝
                  </button>
                </div>
              </li>
            );
          })}
        </ul>
        {respond.isError && (
          <div className="p-3 text-sm text-red-600">
            操作失败：
            {respond.error instanceof ApiError
              ? respond.error.message
              : '未知错误'}
          </div>
        )}
      </div>
    </div>
  );
}

// ────────────────────────────────────────────────────────────────────────
// Shared bits
// ────────────────────────────────────────────────────────────────────────

function PanelHeader({ title, onBack }: { title: string; onBack: () => void }) {
  return (
    <div className="p-3 border-b border-slate-200 dark:border-slate-800 flex items-center gap-2">
      <button
        type="button"
        onClick={onBack}
        aria-label="返回"
        className="shrink-0 h-9 w-9 rounded-md border border-slate-300 dark:border-slate-700 text-slate-600 dark:text-slate-300 hover:bg-slate-100 dark:hover:bg-slate-800 flex items-center justify-center"
      >
        ←
      </button>
      <h3 className="text-sm font-medium text-slate-900 dark:text-slate-100">
        {title}
      </h3>
    </div>
  );
}

function BellIcon() {
  return (
    <svg
      width="18"
      height="18"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9" />
      <path d="M10.3 21a1.94 1.94 0 0 0 3.4 0" />
    </svg>
  );
}
