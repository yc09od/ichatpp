// /profile — edit nickname / signature / visibility, upload avatar
// (TODO [45]).
//
// Avatar upload: standard <input type="file"> with a local-URL preview
// (URL.createObjectURL) before commit. We deliberately skip a heavy
// crop UI here; the backend already centre-crops to 200×200, and the
// SPA can add a richer cropping flow later without a contract change.

'use client';

import { useEffect, useRef, useState } from 'react';
import { useMutation, useQueryClient } from '@tanstack/react-query';
import { ApiError, apiFetch } from '@/lib/api';
import { useSelfUser } from '@/components/TopBar';

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

export default function ProfilePage() {
  const queryClient = useQueryClient();
  const { data: me, isLoading } = useSelfUser();
  const [nickname, setNickname] = useState('');
  const [signature, setSignature] = useState('');
  const [isVisible, setIsVisible] = useState(true);
  const [previewUrl, setPreviewUrl] = useState<string | null>(null);
  const [savedMsg, setSavedMsg] = useState<string | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const fileRef = useRef<HTMLInputElement>(null);

  // Hydrate form once me loads.
  useEffect(() => {
    if (!me) return;
    setNickname(me.nickname ?? '');
    setSignature(me.signature ?? '');
    setIsVisible(me.is_visible);
  }, [me]);

  // Free the local preview URL on unmount / replacement.
  useEffect(() => {
    return () => {
      if (previewUrl) URL.revokeObjectURL(previewUrl);
    };
  }, [previewUrl]);

  const saveProfile = useMutation({
    mutationFn: (payload: {
      nickname?: string;
      signature?: string;
      is_visible?: boolean;
    }) =>
      apiFetch<SelfUser>('/api/users/me', {
        json: payload,
        method: 'PUT',
      }),
    onSuccess: (next) => {
      queryClient.setQueryData(['me'], next);
      setSavedMsg('保存成功');
      setErrorMsg(null);
    },
    onError: (e) => {
      setSavedMsg(null);
      setErrorMsg(e instanceof ApiError ? e.message : '保存失败');
    },
  });

  const uploadAvatar = useMutation({
    mutationFn: async (file: File) => {
      const fd = new FormData();
      fd.append('file', file);
      return apiFetch<{ user: SelfUser; thumbnail_url: string }>(
        '/api/users/avatar',
        { method: 'POST', rawBody: fd },
      );
    },
    onSuccess: (resp) => {
      queryClient.setQueryData(['me'], resp.user);
      // Drop the local preview now that the canonical URL is live.
      if (previewUrl) {
        URL.revokeObjectURL(previewUrl);
        setPreviewUrl(null);
      }
      setSavedMsg('头像已更新');
      setErrorMsg(null);
    },
    onError: (e) => {
      setSavedMsg(null);
      setErrorMsg(e instanceof ApiError ? e.message : '头像上传失败');
    },
  });

  function handleFile(e: React.ChangeEvent<HTMLInputElement>) {
    const f = e.target.files?.[0];
    if (!f) return;
    if (previewUrl) URL.revokeObjectURL(previewUrl);
    setPreviewUrl(URL.createObjectURL(f));
    uploadAvatar.mutate(f);
    e.target.value = '';
  }

  function handleSave(e: React.FormEvent) {
    e.preventDefault();
    setSavedMsg(null);
    setErrorMsg(null);
    // Send only fields the user has actually changed; backend treats
    // missing fields as "leave alone" (COALESCE in the SQL).
    const payload: {
      nickname?: string;
      signature?: string;
      is_visible?: boolean;
    } = {};
    if ((me?.nickname ?? '') !== nickname && nickname.trim()) {
      payload.nickname = nickname.trim();
    }
    if ((me?.signature ?? '') !== signature) {
      payload.signature = signature;
    }
    if (me?.is_visible !== isVisible) payload.is_visible = isVisible;
    if (Object.keys(payload).length === 0) {
      setSavedMsg('没有变更');
      return;
    }
    saveProfile.mutate(payload);
  }

  if (isLoading || !me) {
    return (
      <div className="flex-1 flex items-center justify-center text-slate-500">
        加载中…
      </div>
    );
  }

  return (
    <div className="flex-1 overflow-y-auto p-6 max-w-2xl mx-auto w-full">
      <h1 className="text-2xl font-semibold mb-6 text-slate-900 dark:text-slate-100">
        个人中心
      </h1>

      <section className="mb-8">
        <h2 className="text-sm font-medium mb-3 text-slate-700 dark:text-slate-300">
          头像
        </h2>
        <div className="flex items-center gap-4">
          {previewUrl || me.avatar_url ? (
            // eslint-disable-next-line @next/next/no-img-element
            <img
              src={previewUrl ?? me.avatar_url ?? ''}
              alt=""
              className="w-24 h-24 rounded-full object-cover border border-slate-200 dark:border-slate-700"
            />
          ) : (
            <div className="w-24 h-24 rounded-full bg-blue-600 text-white flex items-center justify-center text-3xl font-medium">
              {(me.nickname ?? me.email).charAt(0).toUpperCase()}
            </div>
          )}
          <div>
            <button
              type="button"
              onClick={() => fileRef.current?.click()}
              disabled={uploadAvatar.isPending}
              className="rounded-md border border-slate-300 dark:border-slate-700 px-3 py-1.5 text-sm hover:bg-slate-100 dark:hover:bg-slate-800 disabled:opacity-50"
            >
              {uploadAvatar.isPending ? '上传中…' : '更换头像'}
            </button>
            <p className="text-xs text-slate-500 mt-1">PNG/JPG, ≤ 2MB</p>
            <input
              ref={fileRef}
              type="file"
              accept="image/png,image/jpeg"
              className="hidden"
              onChange={handleFile}
            />
          </div>
        </div>
      </section>

      <form onSubmit={handleSave} className="space-y-4">
        <div>
          <label className="block text-sm font-medium mb-1 text-slate-700 dark:text-slate-300">
            邮箱
          </label>
          <input
            type="email"
            value={me.email}
            disabled
            className="w-full rounded-md border border-slate-300 dark:border-slate-700 bg-slate-100 dark:bg-slate-800 px-3 py-2 text-sm text-slate-500"
          />
        </div>

        <div>
          <label className="block text-sm font-medium mb-1 text-slate-700 dark:text-slate-300">
            账号
          </label>
          <input
            type="text"
            value={me.account_code}
            disabled
            className="w-full rounded-md border border-slate-300 dark:border-slate-700 bg-slate-100 dark:bg-slate-800 px-3 py-2 text-sm font-mono text-slate-500"
          />
        </div>

        <div>
          <label htmlFor="nickname" className="block text-sm font-medium mb-1 text-slate-700 dark:text-slate-300">
            昵称
          </label>
          <input
            id="nickname"
            type="text"
            value={nickname}
            onChange={(e) => setNickname(e.target.value)}
            className="w-full rounded-md border border-slate-300 dark:border-slate-700 bg-white dark:bg-slate-800 px-3 py-2 text-sm text-slate-900 dark:text-slate-100"
          />
        </div>

        <div>
          <label htmlFor="signature" className="block text-sm font-medium mb-1 text-slate-700 dark:text-slate-300">
            签名
          </label>
          <textarea
            id="signature"
            rows={3}
            value={signature}
            onChange={(e) => setSignature(e.target.value)}
            className="w-full rounded-md border border-slate-300 dark:border-slate-700 bg-white dark:bg-slate-800 px-3 py-2 text-sm text-slate-900 dark:text-slate-100"
          />
        </div>

        <label className="flex items-center gap-2 text-sm text-slate-700 dark:text-slate-300">
          <input
            type="checkbox"
            checked={isVisible}
            onChange={(e) => setIsVisible(e.target.checked)}
          />
          公开个人档案（关闭后非好友看不到你的资料）
        </label>

        {savedMsg && (
          <div className="rounded-md bg-green-50 dark:bg-green-950/40 px-3 py-2 text-sm text-green-700 dark:text-green-300">
            {savedMsg}
          </div>
        )}
        {errorMsg && (
          <div className="rounded-md bg-red-50 dark:bg-red-950/40 px-3 py-2 text-sm text-red-700 dark:text-red-300">
            {errorMsg}
          </div>
        )}

        <button
          type="submit"
          disabled={saveProfile.isPending}
          className="rounded-md bg-blue-600 text-white px-4 py-2 text-sm font-medium hover:bg-blue-700 disabled:opacity-50"
        >
          {saveProfile.isPending ? '保存中…' : '保存'}
        </button>
      </form>
    </div>
  );
}
