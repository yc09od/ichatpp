// /register — TODO [37].
//
// Email + password + invitation_code form. Per the TODO acceptance, the
// invitation code is pre-validated client-side via `/api/invitations/validate`
// before the actual /register submit — this surfaces "邀请码无效 / 已过期"
// as a fast inline error rather than a 4xx after the user has already
// committed to the rest of the form.
//
// On successful register the backend writes the auth cookies; the SPA
// just redirects to /chat.

'use client';

import Link from 'next/link';
import { useRouter } from 'next/navigation';
import { useForm } from 'react-hook-form';
import { zodResolver } from '@hookform/resolvers/zod';
import { z } from 'zod';
import { ApiError, apiFetch } from '@/lib/api';
import { useState } from 'react';

const RegisterSchema = z.object({
  email: z.string().email('请输入有效的邮箱地址'),
  // Mirror the backend's bcrypt-friendly minimum (≥ 8 from the register
  // handler validation). Backend may apply extra rules — we keep the
  // client check lenient so a stricter backend message wins on submit.
  password: z.string().min(8, '密码至少 8 位'),
  invitation_code: z.string().min(1, '请填写邀请码'),
  nickname: z
    .string()
    .max(100, '昵称最多 100 个字符')
    .optional()
    .or(z.literal('')),
});

type RegisterInput = z.infer<typeof RegisterSchema>;

interface ValidateResponse {
  valid: boolean;
  expires_at?: string;
  used_by?: string;
}

export default function RegisterPage() {
  const router = useRouter();
  const [submitError, setSubmitError] = useState<string | null>(null);
  const {
    register,
    handleSubmit,
    setError,
    formState: { errors, isSubmitting },
  } = useForm<RegisterInput>({ resolver: zodResolver(RegisterSchema) });

  async function onSubmit(values: RegisterInput) {
    setSubmitError(null);

    // Pre-flight: ask the backend whether the invitation code is usable.
    // The endpoint is rate-limited (30/min/IP), so this is cheap. A
    // negative response stops us before we send the real /register —
    // matches the TODO acceptance criterion.
    try {
      const validate = await apiFetch<ValidateResponse>('/api/invitations/validate', {
        json: { code: values.invitation_code },
        method: 'POST',
      });
      if (!validate.valid) {
        setError('invitation_code', {
          type: 'remote',
          message: '邀请码无效或已过期',
        });
        return;
      }
    } catch (err) {
      // The validate call itself failed (rate-limit, network). Surface
      // generically — the user can retry.
      const msg = err instanceof ApiError ? err.message : '邀请码校验失败，请稍后重试';
      setError('invitation_code', { type: 'remote', message: msg });
      return;
    }

    // Drop empty nickname so the backend stores NULL, not "".
    const payload: Record<string, unknown> = {
      email: values.email,
      password: values.password,
      invitation_code: values.invitation_code,
    };
    if (values.nickname && values.nickname.trim()) {
      payload.nickname = values.nickname.trim();
    }

    try {
      await apiFetch('/api/auth/register', { json: payload, method: 'POST' });
      router.push('/chat');
    } catch (err) {
      if (err instanceof ApiError) {
        // Field-targeted backend errors map back to the form when
        // possible; otherwise show a generic banner.
        if (err.code === 'CONFLICT') {
          // Either email already in use, or invitation already used.
          setSubmitError(err.message);
        } else if (err.code === 'BAD_REQUEST') {
          setSubmitError(err.message);
        } else {
          setSubmitError(err.message);
        }
      } else {
        setSubmitError('注册失败，请稍后重试');
      }
    }
  }

  return (
    <>
      <h1 className="text-2xl font-semibold mb-1 text-slate-900 dark:text-slate-50">注册</h1>
      <p className="text-sm text-slate-500 dark:text-slate-400 mb-6">
        ichatpp 是邀请制账号，请填写邀请码完成注册。
      </p>

      <form onSubmit={handleSubmit(onSubmit)} className="space-y-4" noValidate>
        <div>
          <label htmlFor="email" className="block text-sm font-medium mb-1 text-slate-700 dark:text-slate-300">
            邮箱
          </label>
          <input
            id="email"
            type="email"
            autoComplete="email"
            disabled={isSubmitting}
            {...register('email')}
            className="w-full rounded-md border border-slate-300 dark:border-slate-700 bg-white dark:bg-slate-800 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 focus:outline-none focus:ring-2 focus:ring-blue-500 disabled:opacity-60"
          />
          {errors.email && (
            <p className="text-xs text-red-600 mt-1">{errors.email.message}</p>
          )}
        </div>

        <div>
          <label htmlFor="password" className="block text-sm font-medium mb-1 text-slate-700 dark:text-slate-300">
            密码
          </label>
          <input
            id="password"
            type="password"
            autoComplete="new-password"
            disabled={isSubmitting}
            {...register('password')}
            className="w-full rounded-md border border-slate-300 dark:border-slate-700 bg-white dark:bg-slate-800 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 focus:outline-none focus:ring-2 focus:ring-blue-500 disabled:opacity-60"
          />
          {errors.password && (
            <p className="text-xs text-red-600 mt-1">{errors.password.message}</p>
          )}
        </div>

        <div>
          <label htmlFor="invitation_code" className="block text-sm font-medium mb-1 text-slate-700 dark:text-slate-300">
            邀请码
          </label>
          <input
            id="invitation_code"
            type="text"
            autoComplete="off"
            disabled={isSubmitting}
            placeholder="INV-XXXXXXXXXXXXXXXXXXXX"
            {...register('invitation_code')}
            className="w-full rounded-md border border-slate-300 dark:border-slate-700 bg-white dark:bg-slate-800 px-3 py-2 text-sm font-mono text-slate-900 dark:text-slate-100 focus:outline-none focus:ring-2 focus:ring-blue-500 disabled:opacity-60"
          />
          {errors.invitation_code && (
            <p className="text-xs text-red-600 mt-1">{errors.invitation_code.message}</p>
          )}
        </div>

        <div>
          <label htmlFor="nickname" className="block text-sm font-medium mb-1 text-slate-700 dark:text-slate-300">
            昵称 <span className="text-slate-400">(可选)</span>
          </label>
          <input
            id="nickname"
            type="text"
            autoComplete="nickname"
            disabled={isSubmitting}
            {...register('nickname')}
            className="w-full rounded-md border border-slate-300 dark:border-slate-700 bg-white dark:bg-slate-800 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 focus:outline-none focus:ring-2 focus:ring-blue-500 disabled:opacity-60"
          />
          {errors.nickname && (
            <p className="text-xs text-red-600 mt-1">{errors.nickname.message}</p>
          )}
        </div>

        {submitError && (
          <div
            role="alert"
            className="rounded-md bg-red-50 dark:bg-red-950/40 px-3 py-2 text-sm text-red-700 dark:text-red-300 border border-red-200 dark:border-red-900"
          >
            {submitError}
          </div>
        )}

        <button
          type="submit"
          disabled={isSubmitting}
          className="w-full rounded-md bg-blue-600 text-white font-medium py-2 text-sm hover:bg-blue-700 transition-colors disabled:opacity-60 disabled:cursor-not-allowed"
        >
          {isSubmitting ? '注册中…' : '注册'}
        </button>
      </form>

      <p className="text-sm text-center mt-6 text-slate-600 dark:text-slate-400">
        已有账号？{' '}
        <Link href="/login" className="text-blue-600 hover:underline">
          直接登录
        </Link>
      </p>
    </>
  );
}
