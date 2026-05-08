// /login — TODO [37].
//
// Email + password form. On success the backend writes the three auth
// cookies (access_token httpOnly, refresh_token httpOnly Path scoped,
// csrf_token non-httpOnly) and returns no body. The client only needs
// to redirect — `apiFetch` does the cookie work via `credentials: 'include'`.

'use client';

import Link from 'next/link';
import { useRouter } from 'next/navigation';
import { useForm } from 'react-hook-form';
import { zodResolver } from '@hookform/resolvers/zod';
import { z } from 'zod';
import { ApiError, apiFetch } from '@/lib/api';
import { useState } from 'react';

const LoginSchema = z.object({
  email: z.string().email('请输入有效的邮箱地址'),
  password: z.string().min(1, '密码不能为空'),
});

type LoginInput = z.infer<typeof LoginSchema>;

export default function LoginPage() {
  const router = useRouter();
  const [submitError, setSubmitError] = useState<string | null>(null);
  const {
    register,
    handleSubmit,
    formState: { errors, isSubmitting },
  } = useForm<LoginInput>({ resolver: zodResolver(LoginSchema) });

  async function onSubmit(values: LoginInput) {
    setSubmitError(null);
    try {
      await apiFetch('/api/auth/login', { json: values, method: 'POST' });
      router.push('/chat');
    } catch (err) {
      // The backend returns a generic "邮箱或密码错误" for credential
      // failures so the form mirrors that without leaking which field
      // was wrong. Anything else (rate-limit, server error) shows the
      // backend message verbatim.
      if (err instanceof ApiError) {
        setSubmitError(err.message);
      } else {
        setSubmitError('登录失败，请稍后重试');
      }
    }
  }

  return (
    <>
      <h1 className="text-2xl font-semibold mb-1 text-slate-900 dark:text-slate-50">登录</h1>
      <p className="text-sm text-slate-500 dark:text-slate-400 mb-6">
        欢迎回来，请使用邮箱与密码登录。
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
            autoComplete="current-password"
            disabled={isSubmitting}
            {...register('password')}
            className="w-full rounded-md border border-slate-300 dark:border-slate-700 bg-white dark:bg-slate-800 px-3 py-2 text-sm text-slate-900 dark:text-slate-100 focus:outline-none focus:ring-2 focus:ring-blue-500 disabled:opacity-60"
          />
          {errors.password && (
            <p className="text-xs text-red-600 mt-1">{errors.password.message}</p>
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
          {isSubmitting ? '登录中…' : '登录'}
        </button>
      </form>

      <p className="text-sm text-center mt-6 text-slate-600 dark:text-slate-400">
        还没有账号？{' '}
        <Link href="/register" className="text-blue-600 hover:underline">
          注册（需要邀请码）
        </Link>
      </p>
    </>
  );
}
