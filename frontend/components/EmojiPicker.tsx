// EmojiPicker — grid of the user's custom emojis (TODO [44]).
//
// One mutation each for upload + delete; both invalidate the `['emojis']`
// query key so the picker stays in sync with the server-side cache.

'use client';

import { useRef, useState } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { ApiError, apiFetch } from '@/lib/api';

export interface EmojiView {
  id: string;
  name: string;
  file_url: string;
  thumbnail_url: string | null;
  mime_type: string | null;
  file_size: number | null;
  created_at: string;
}

interface ListResponse {
  emojis: EmojiView[];
}

export function useEmojis() {
  return useQuery<ListResponse, Error, EmojiView[]>({
    queryKey: ['emojis'],
    queryFn: () => apiFetch<ListResponse>('/api/emojis'),
    select: (r) => r.emojis,
    staleTime: 30 * 1000,
  });
}

interface EmojiPickerProps {
  /** Called when an emoji is clicked — typically inserts a placeholder
   * into the chat input. */
  onPick: (emoji: EmojiView) => void;
}

export function EmojiPicker({ onPick }: EmojiPickerProps) {
  const queryClient = useQueryClient();
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const { data: emojis = [], isLoading } = useEmojis();

  const upload = useMutation({
    mutationFn: async (file: File) => {
      const fd = new FormData();
      fd.append('file', file);
      // Strip extension for the default name; user can rename later
      // (no rename UI yet — fine, default is fine for MVP).
      fd.append('name', file.name.replace(/\.[^/.]+$/, ''));
      return apiFetch<{ emoji: EmojiView }>('/api/emojis', {
        method: 'POST',
        rawBody: fd,
      });
    },
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['emojis'] }),
  });

  const remove = useMutation({
    mutationFn: (id: string) =>
      apiFetch(`/api/emojis/${id}`, { method: 'DELETE' }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['emojis'] }),
  });

  function handleFile(e: React.ChangeEvent<HTMLInputElement>) {
    setErrorMsg(null);
    const f = e.target.files?.[0];
    if (!f) return;
    upload.mutate(f, {
      onError: (err) => {
        setErrorMsg(err instanceof ApiError ? err.message : '上传失败');
      },
    });
    // Reset input so re-uploading the same file fires `change` again.
    e.target.value = '';
  }

  return (
    <div className="w-72 max-h-80 overflow-y-auto rounded-md border border-slate-200 dark:border-slate-800 bg-white dark:bg-slate-900 shadow-lg p-2">
      <div className="flex items-center justify-between mb-2 px-1">
        <span className="text-xs font-medium text-slate-700 dark:text-slate-300">
          表情 ({emojis.length}/100)
        </span>
        <button
          type="button"
          onClick={() => fileInputRef.current?.click()}
          disabled={upload.isPending || emojis.length >= 100}
          className="text-xs text-blue-600 hover:underline disabled:opacity-50"
        >
          {upload.isPending ? '上传中…' : '+ 上传'}
        </button>
        <input
          ref={fileInputRef}
          type="file"
          accept="image/png,image/jpeg"
          className="hidden"
          onChange={handleFile}
        />
      </div>

      {errorMsg && (
        <div className="text-xs text-red-600 px-1 mb-1">{errorMsg}</div>
      )}

      {isLoading && (
        <div className="text-xs text-slate-500 px-1 py-2">加载中…</div>
      )}

      {!isLoading && emojis.length === 0 && (
        <div className="text-xs text-slate-500 px-1 py-2">
          还没有表情，上传一个 PNG/JPG（≤500KB）
        </div>
      )}

      <div className="grid grid-cols-6 gap-1">
        {emojis.map((e) => (
          <div key={e.id} className="relative group">
            <button
              type="button"
              onClick={() => onPick(e)}
              className="w-full aspect-square rounded hover:bg-slate-100 dark:hover:bg-slate-800 p-1"
              title={e.name}
            >
              {/* eslint-disable-next-line @next/next/no-img-element */}
              <img
                src={e.thumbnail_url ?? e.file_url}
                alt={e.name}
                className="w-full h-full object-contain"
              />
            </button>
            <button
              type="button"
              onClick={() => {
                if (confirm(`删除表情 "${e.name}"？`)) remove.mutate(e.id);
              }}
              className="absolute -top-1 -right-1 w-4 h-4 rounded-full bg-red-500 text-white text-[10px] leading-none opacity-0 group-hover:opacity-100"
              aria-label={`删除 ${e.name}`}
            >
              ×
            </button>
          </div>
        ))}
      </div>
    </div>
  );
}
