// Per-conversation chat surface (TODOs [42] + [43] + [44]).
//
// Composition:
//  - Header: friend display, search toggle.
//  - Body: react-virtuoso list of messages (oldest at top, newest at
//    bottom) with cursor-based history loading on `startReached`.
//  - Footer: textarea + emoji picker + send.
//  - Optional overlay: search results panel.
//
// State model:
//  - TanStack Query owns the chronological message array. The Virtuoso
//    `firstItemIndex` trick keeps scroll anchored when older messages
//    are prepended.
//  - Live WS `message` / `message_received` events update the same
//    cache via a small event-driven setter.
//  - Optimistic outbound: a temp message with id `tmp:<uuid>` is added
//    on send; the server's `message_received` ack swaps it for the
//    real id.

'use client';

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Virtuoso, type VirtuosoHandle } from 'react-virtuoso';
import { useQueries, useQuery, useQueryClient } from '@tanstack/react-query';
import { ApiError, apiFetch } from '@/lib/api';
import {
  useSendWebSocket,
  useWebSocketEvent,
} from '@/components/providers/WebSocketProvider';
import { useSelfUser } from '@/components/TopBar';
import { EmojiPicker, type EmojiView, useEmojis } from '@/components/EmojiPicker';

// ────────────────────────────────────────────────────────────────────────
// Types
// ────────────────────────────────────────────────────────────────────────

export interface MessageView {
  id: string; // real or `tmp:<uuid>`
  from_user_id: string;
  to_user_id: string;
  content: string;
  is_deleted: boolean;
  emoji_ids: string[];
  created_at: string;
  /** Local-only flag for optimistic rows awaiting server ack. */
  pending?: boolean;
  /** Local-only correlation id used to swap the temp row when ack arrives. */
  clientId?: string;
}

interface HistoryResponse {
  messages: MessageView[];
  next_cursor: string | null;
}

interface FriendProfile {
  id: string;
  account_code: string;
  nickname: string | null;
  avatar_url: string | null;
}

interface SearchHit extends MessageView {
  headline: string;
}

interface SearchResponse {
  hits: SearchHit[];
  total: number;
}

// ────────────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────────────

function uniqueAppend(existing: MessageView[], incoming: MessageView): MessageView[] {
  if (existing.some((m) => m.id === incoming.id)) return existing;
  return [...existing, incoming];
}

function uniquePrepend(existing: MessageView[], older: MessageView[]): MessageView[] {
  const seen = new Set(existing.map((m) => m.id));
  const fresh = older.filter((m) => !seen.has(m.id));
  return [...fresh, ...existing];
}

function makeClientId(): string {
  // Sufficient for de-dup against the server ack within a single tab.
  return `tmp:${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
}

// ────────────────────────────────────────────────────────────────────────
// Component
// ────────────────────────────────────────────────────────────────────────

interface ChatWindowProps {
  friendId: string;
}

export function ChatWindow({ friendId }: ChatWindowProps) {
  const queryClient = useQueryClient();
  const sendWs = useSendWebSocket();
  const { data: me } = useSelfUser();

  // Friend profile — small extra fetch so the header has a display name
  // even on a deep-link without the friends list cached.
  const { data: friend } = useQuery<FriendProfile>({
    queryKey: ['user', friendId],
    queryFn: () => apiFetch<FriendProfile>(`/api/users/${friendId}`),
    staleTime: 5 * 60 * 1000,
  });

  // Conversation cache. We do NOT use `infiniteQuery` here because the
  // page set has to merge with live WS pushes — managing that with the
  // built-in `pages` array fights TanStack Query's normal model. A
  // single `messages` array we prepend/append by hand is simpler.
  const messagesKey = useMemo(() => ['messages', friendId] as const, [friendId]);
  const cursorKey = useMemo(() => ['messages-cursor', friendId] as const, [friendId]);

  const [hasInitialLoaded, setHasInitialLoaded] = useState(false);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [searchOpen, setSearchOpen] = useState(false);

  // Initial page fetch.
  useEffect(() => {
    setHasInitialLoaded(false);
    let cancelled = false;
    (async () => {
      try {
        const page = await apiFetch<HistoryResponse>(
          `/api/messages/${friendId}?limit=50`,
        );
        if (cancelled) return;
        // History endpoint returns newest-first; we render oldest-first
        // for the chat timeline.
        const sorted = [...page.messages].reverse();
        queryClient.setQueryData<MessageView[]>(messagesKey, sorted);
        queryClient.setQueryData<string | null>(cursorKey, page.next_cursor);
        setHasInitialLoaded(true);
      } catch (e) {
        if (cancelled) return;
        console.error('[chat] initial load failed', e);
        queryClient.setQueryData<MessageView[]>(messagesKey, []);
        setHasInitialLoaded(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [friendId, messagesKey, cursorKey, queryClient]);

  const messages =
    queryClient.getQueryData<MessageView[]>(messagesKey) ?? [];
  // Force re-render when messages change — TanStack's setQueryData notifies
  // subscribers; we use useQuery to subscribe.
  const { data: subscribedMessages } = useQuery<MessageView[]>({
    queryKey: messagesKey,
    queryFn: () => Promise.resolve(messages),
    enabled: hasInitialLoaded,
    staleTime: Infinity,
  });
  const list = subscribedMessages ?? messages;

  // ── Live event handler ──
  useWebSocketEvent((ev) => {
    if (ev.type === 'message') {
      // Inbound `message` events are only delivered to the recipient
      // (the backend doesn't echo to the sender — sender gets their own
      // optimistic row + a `message_received` ack). So `to_user_id` is
      // implicitly `me`, and we filter by sender.
      if (!me) return;
      if (ev.from_user_id !== friendId) return;
      queryClient.setQueryData<MessageView[]>(messagesKey, (prev) =>
        uniqueAppend(prev ?? [], {
          id: ev.message_id,
          from_user_id: ev.from_user_id,
          to_user_id: me.id,
          content: ev.content,
          is_deleted: false,
          emoji_ids: ev.emoji_ids ?? [],
          created_at: ev.created_at,
        }),
      );
    } else if (ev.type === 'message_received') {
      // Swap the most-recent pending row with the real one. We match
      // on the trailing `pending` row instead of clientId because the
      // ack carries only `message_id` + `timestamp`.
      queryClient.setQueryData<MessageView[]>(messagesKey, (prev) => {
        if (!prev) return prev;
        // Find the OLDEST pending row to keep send-order intact.
        const idx = prev.findIndex((m) => m.pending);
        if (idx < 0) return prev;
        const next = prev.slice();
        next[idx] = {
          ...next[idx],
          id: ev.message_id,
          created_at: ev.timestamp,
          pending: false,
        };
        return next;
      });
    }
  });

  // ── Older-page loader ──
  const loadOlder = useCallback(async () => {
    if (loadingOlder) return;
    const cursor = queryClient.getQueryData<string | null>(cursorKey);
    if (!cursor) return;
    setLoadingOlder(true);
    try {
      const page = await apiFetch<HistoryResponse>(
        `/api/messages/${friendId}?limit=50&cursor=${encodeURIComponent(cursor)}`,
      );
      const olderAsc = [...page.messages].reverse();
      queryClient.setQueryData<MessageView[]>(messagesKey, (prev) =>
        uniquePrepend(prev ?? [], olderAsc),
      );
      queryClient.setQueryData<string | null>(cursorKey, page.next_cursor);
    } catch (e) {
      console.error('[chat] load older failed', e);
    } finally {
      setLoadingOlder(false);
    }
  }, [friendId, messagesKey, cursorKey, loadingOlder, queryClient]);

  // ── Composer ──
  const [draft, setDraft] = useState('');
  const [pickerOpen, setPickerOpen] = useState(false);

  // Used to render images inline in MessageRow when a message carries
  // `emoji_ids`. Two sources stitched together:
  //   1. The user's own emoji catalog (single bulk fetch, already cached).
  //   2. Per-id lookups for emojis owned by the *other* party. Each id
  //      becomes its own TanStack query so identical ids across many
  //      messages collapse to a single network hop and the result is
  //      shared across rows.
  const { data: myEmojis } = useEmojis();
  const ownedEmojiIdSet = useMemo(
    () => new Set((myEmojis ?? []).map((e) => e.id)),
    [myEmojis],
  );
  const unknownEmojiIds = useMemo(() => {
    const ids = new Set<string>();
    for (const m of list) {
      for (const id of m.emoji_ids) {
        if (!ownedEmojiIdSet.has(id)) ids.add(id);
      }
    }
    return Array.from(ids);
  }, [list, ownedEmojiIdSet]);
  const fetchedEmojis = useQueries({
    queries: unknownEmojiIds.map((id) => ({
      queryKey: ['emoji', id] as const,
      queryFn: () => apiFetch<EmojiView>(`/api/emojis/${id}`),
      // Emojis are mostly immutable once uploaded; a 10-minute window
      // keeps the cache useful without chasing renames forever.
      staleTime: 10 * 60 * 1000,
    })),
  });
  const emojiById = useMemo(() => {
    const m = new Map<string, EmojiView>();
    for (const e of myEmojis ?? []) m.set(e.id, e);
    for (const r of fetchedEmojis) {
      if (r.data) m.set(r.data.id, r.data);
    }
    return m;
  }, [myEmojis, fetchedEmojis]);

  const onSend = useCallback(() => {
    const trimmed = draft.trim();
    if (!trimmed || !me) return;
    const clientId = makeClientId();
    const optimistic: MessageView = {
      id: clientId,
      from_user_id: me.id,
      to_user_id: friendId,
      content: trimmed,
      is_deleted: false,
      emoji_ids: [],
      created_at: new Date().toISOString(),
      pending: true,
      clientId,
    };
    queryClient.setQueryData<MessageView[]>(messagesKey, (prev) => [
      ...(prev ?? []),
      optimistic,
    ]);
    sendWs({
      type: 'message',
      to_user_id: friendId,
      content: trimmed,
      emoji_ids: [],
    });
    setDraft('');
    setPickerOpen(false);
  }, [draft, friendId, me, messagesKey, queryClient, sendWs]);

  // Click-to-send: emoji picker now fires off a one-emoji message
  // immediately and auto-closes the popover. Backend rejects empty
  // content, so we use the emoji's name as the visible fallback for
  // receivers who can't resolve emoji_ids → image yet.
  const sendEmoji = useCallback(
    (emoji: EmojiView) => {
      if (!me) return;
      const clientId = makeClientId();
      const optimistic: MessageView = {
        id: clientId,
        from_user_id: me.id,
        to_user_id: friendId,
        content: emoji.name,
        is_deleted: false,
        emoji_ids: [emoji.id],
        created_at: new Date().toISOString(),
        pending: true,
        clientId,
      };
      queryClient.setQueryData<MessageView[]>(messagesKey, (prev) => [
        ...(prev ?? []),
        optimistic,
      ]);
      sendWs({
        type: 'message',
        to_user_id: friendId,
        content: emoji.name,
        emoji_ids: [emoji.id],
      });
      setPickerOpen(false);
    },
    [friendId, me, messagesKey, queryClient, sendWs],
  );

  // ── Virtuoso refs ──
  const virtuosoRef = useRef<VirtuosoHandle>(null);

  return (
    <div className="flex flex-col flex-1 min-h-0 bg-white dark:bg-slate-900">
      <header className="h-14 border-b border-slate-200 dark:border-slate-800 px-4 flex items-center justify-between gap-2 shrink-0">
        <div className="min-w-0">
          <div className="text-sm font-semibold truncate text-slate-900 dark:text-slate-100">
            {friend?.nickname || friend?.account_code || friendId}
          </div>
          {friend?.account_code && (
            <div className="text-xs text-slate-500 truncate">
              {friend.account_code}
            </div>
          )}
        </div>
        <button
          type="button"
          onClick={() => setSearchOpen((v) => !v)}
          className="rounded-md border border-slate-300 dark:border-slate-700 px-3 py-1 text-sm text-slate-700 dark:text-slate-300 hover:bg-slate-100 dark:hover:bg-slate-800"
        >
          搜索
        </button>
      </header>

      <div className="relative flex-1 min-h-0">
        {!hasInitialLoaded ? (
          <div className="h-full flex items-center justify-center text-sm text-slate-500">
            加载消息…
          </div>
        ) : (
          <Virtuoso
            ref={virtuosoRef}
            data={list}
            initialTopMostItemIndex={Math.max(list.length - 1, 0)}
            followOutput={(isAtBottom) => (isAtBottom ? 'smooth' : false)}
            startReached={loadOlder}
            itemContent={(index, m) => (
              <MessageRow
                key={m.id}
                message={m}
                mine={me ? m.from_user_id === me.id : false}
                emojiById={emojiById}
              />
            )}
            components={{
              Header: () =>
                loadingOlder ? (
                  <div className="text-center text-xs text-slate-400 py-2">
                    加载历史消息…
                  </div>
                ) : null,
            }}
          />
        )}

        {searchOpen && (
          <SearchPanel
            friendId={friendId}
            onClose={() => setSearchOpen(false)}
            onPick={(hit) => {
              // Best-effort: scroll to the hit if it's in the rendered set.
              const idx = list.findIndex((m) => m.id === hit.id);
              if (idx >= 0) {
                virtuosoRef.current?.scrollToIndex({
                  index: idx,
                  align: 'center',
                });
                setSearchOpen(false);
              } else {
                // Not in the loaded window — keep the panel open and
                // let the user click "load more" on the timeline.
                alert('该消息不在已加载范围内，请向上滚动加载更多历史');
              }
            }}
          />
        )}
      </div>

      <footer className="border-t border-slate-200 dark:border-slate-800 p-3 shrink-0 relative">
        <div className="flex items-end gap-2">
          <button
            type="button"
            onClick={() => setPickerOpen((v) => !v)}
            className="rounded-md border border-slate-300 dark:border-slate-700 px-3 py-2 text-sm text-slate-700 dark:text-slate-300 hover:bg-slate-100 dark:hover:bg-slate-800"
            aria-label="表情"
          >
            ☺
          </button>
          <textarea
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                onSend();
              }
            }}
            rows={1}
            placeholder="发消息（Enter 发送，Shift+Enter 换行）"
            className="flex-1 resize-none rounded-md border border-slate-300 dark:border-slate-700 bg-white dark:bg-slate-800 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 focus:outline-none focus:ring-2 focus:ring-blue-500"
          />
          <button
            type="button"
            onClick={onSend}
            disabled={!draft.trim()}
            className="rounded-md bg-blue-600 text-white px-4 py-2 text-sm font-medium hover:bg-blue-700 disabled:opacity-50"
          >
            发送
          </button>
        </div>
        {pickerOpen && (
          <div className="absolute bottom-full left-3 mb-2 z-20">
            <EmojiPicker onPick={sendEmoji} />
          </div>
        )}
      </footer>
    </div>
  );
}

// ────────────────────────────────────────────────────────────────────────
// Sub-components
// ────────────────────────────────────────────────────────────────────────

function MessageRow({
  message,
  mine,
  emojiById,
}: {
  message: MessageView;
  mine: boolean;
  emojiById: Map<string, EmojiView>;
}) {
  const align = mine ? 'justify-end' : 'justify-start';
  // Single-emoji messages render as a borderless image bubble — feels
  // closer to a sticker than a text reply. Multi-emoji or text+emoji
  // messages keep the regular bubble; we'll address those when the
  // UX calls for it.
  const singleEmoji =
    !message.is_deleted && message.emoji_ids.length === 1
      ? emojiById.get(message.emoji_ids[0])
      : undefined;

  if (singleEmoji) {
    return (
      <div className={`px-4 py-1 flex ${align}`}>
        <div
          className={`p-1 ${message.pending ? 'opacity-60' : ''}`}
          title={new Date(message.created_at).toLocaleString()}
        >
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img
            src={singleEmoji.thumbnail_url ?? singleEmoji.file_url}
            alt={singleEmoji.name}
            className="w-20 h-20 object-contain"
          />
        </div>
      </div>
    );
  }

  const bg = mine
    ? 'bg-blue-600 text-white'
    : 'bg-slate-100 dark:bg-slate-800 text-slate-900 dark:text-slate-100';
  return (
    <div className={`px-4 py-1 flex ${align}`}>
      <div
        className={`max-w-[70%] rounded-2xl px-3 py-1.5 text-sm whitespace-pre-wrap break-words ${bg} ${
          message.pending ? 'opacity-60' : ''
        }`}
        title={new Date(message.created_at).toLocaleString()}
      >
        {message.is_deleted ? (
          <span className="italic opacity-70">消息已撤回</span>
        ) : (
          message.content || ' '
        )}
      </div>
    </div>
  );
}

function SearchPanel({
  friendId,
  onClose,
  onPick,
}: {
  friendId: string;
  onClose: () => void;
  onPick: (hit: SearchHit) => void;
}) {
  const [q, setQ] = useState('');
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [pending, setPending] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  async function runSearch() {
    setErr(null);
    if (!q.trim()) {
      setHits([]);
      return;
    }
    setPending(true);
    try {
      const res = await apiFetch<SearchResponse>(
        // Scope to this friend (either side).
        `/api/messages/search?q=${encodeURIComponent(q)}&from=${friendId}`,
      );
      setHits(res.hits);
    } catch (e) {
      setErr(e instanceof ApiError ? e.message : '搜索失败');
    } finally {
      setPending(false);
    }
  }

  return (
    <div className="absolute top-2 right-2 w-80 max-h-[80%] flex flex-col rounded-md border border-slate-200 dark:border-slate-800 bg-white dark:bg-slate-900 shadow-lg z-20">
      <div className="p-2 border-b border-slate-200 dark:border-slate-800 flex gap-2">
        <input
          autoFocus
          type="search"
          value={q}
          onChange={(e) => setQ(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') runSearch();
          }}
          placeholder="搜索这段聊天的内容"
          className="flex-1 rounded border border-slate-300 dark:border-slate-700 bg-white dark:bg-slate-800 px-2 py-1 text-sm text-slate-900 dark:text-slate-100"
        />
        <button
          type="button"
          onClick={onClose}
          className="text-sm text-slate-500 hover:text-slate-900 dark:hover:text-slate-100 px-1"
          aria-label="关闭"
        >
          ×
        </button>
      </div>
      <div className="overflow-y-auto flex-1">
        {pending && <div className="p-3 text-sm text-slate-500">搜索中…</div>}
        {err && <div className="p-3 text-sm text-red-600">{err}</div>}
        {!pending && !err && hits.length === 0 && q.trim() && (
          <div className="p-3 text-sm text-slate-500">没有命中</div>
        )}
        <ul>
          {hits.map((h) => (
            <li key={h.id}>
              <button
                type="button"
                onClick={() => onPick(h)}
                className="block w-full text-left px-3 py-2 border-b border-slate-100 dark:border-slate-800 hover:bg-slate-50 dark:hover:bg-slate-800"
              >
                <div
                  className="text-sm text-slate-900 dark:text-slate-100 [&_mark]:bg-yellow-200 [&_mark]:dark:bg-yellow-700"
                  // Headline is server-controlled and only contains
                  // <mark> tags around tokens — safe to inject.
                  dangerouslySetInnerHTML={{ __html: h.headline }}
                />
                <div className="text-xs text-slate-500 mt-0.5">
                  {new Date(h.created_at).toLocaleString()}
                </div>
              </button>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
