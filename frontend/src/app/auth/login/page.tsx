"use client";

import { Suspense, useEffect, useRef, useState, type FormEvent } from "react";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { LogIn, ShieldCheck } from "lucide-react";

import { Alert, Button, Field, Input } from "@/components/ui/primitives";
import { MFA_SETUP_PATH, NO_CONSOLE_MESSAGE, homeAfterSignIn, useClientHomeRedirect } from "@/hooks/use-client-home";
import { ApiError, describeError } from "@/lib/api/client";
import type { Sesion } from "@/lib/api/types";
import { isTotpCode, normalizeTotpInput } from "@/lib/mfa";
import { isPendingUsable, useAuthStore } from "@/store/auth";

function LoginForm() {
  const router = useRouter();
  const params = useSearchParams();
  const login = useAuthStore((s) => s.login);
  const verifyMfa = useAuthStore((s) => s.verifyMfa);
  const pending = useAuthStore((s) => s.pending);
  const clearPending = useAuthStore((s) => s.clearPending);
  // Already signed in: verified clients go to the dashboard, the rest to
  // onboarding; compliance officers and auditors to the compliance console.
  const { claim, release } = useClientHomeRedirect();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const codeInput = useRef<HTMLInputElement>(null);

  // Second step: the password was right and the authenticator code is missing.
  const verifying = pending?.kind === "verify" && isPendingUsable(pending);

  useEffect(() => {
    if (verifying) codeInput.current?.focus();
  }, [verifying]);

  // The session starts: each role goes to its own home.
  const enter = async (session: Sesion) => {
    const home = await homeAfterSignIn(session);
    if (home) {
      router.replace(home);
      return;
    }
    setError(NO_CONSOLE_MESSAGE);
    useAuthStore.getState().logout();
    release();
    setSubmitting(false);
  };

  const onSubmit = async (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    setSubmitting(true);
    claim();
    try {
      const result = await login(email, password);
      if ("kind" in result) {
        setPassword("");
        if (result.kind === "enroll") {
          // Staff without two-step verification set it up before anything else.
          router.push(MFA_SETUP_PATH);
          return;
        }
        setSubmitting(false);
        return;
      }
      await enter(result);
    } catch (e) {
      release();
      setError(describeError(e));
      setSubmitting(false);
    }
  };

  const onVerify = async (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    setSubmitting(true);
    try {
      await enter(await verifyMfa(code));
    } catch (e) {
      // An expired sign-in starts over from the password; a wrong code stays here.
      if (e instanceof ApiError && (e.code === "DESAFIO_MFA_VENCIDO" || e.code === "BLOQUEADO")) {
        clearPending();
        release();
      }
      setError(describeError(e));
      setCode("");
      setSubmitting(false);
    }
  };

  const startOver = () => {
    clearPending();
    release();
    setCode("");
    setError(null);
  };

  if (verifying) {
    return (
      <form onSubmit={onVerify} className="space-y-5" noValidate>
        <div className="space-y-1">
          <h1 className="text-xl font-semibold tracking-tight">Two-step verification</h1>
          <p className="text-sm text-muted">
            Enter the 6-digit code from your authenticator app for <span className="font-medium text-ink">{pending.email}</span>.
          </p>
        </div>

        {error ? <Alert tone="error">{error}</Alert> : null}

        <Field label="Authentication code" htmlFor="totp-code" hint="The code changes every 30 seconds.">
          <Input
            ref={codeInput}
            id="totp-code"
            name="totp-code"
            inputMode="numeric"
            autoComplete="one-time-code"
            pattern="[0-9]*"
            required
            value={code}
            onChange={(e) => setCode(normalizeTotpInput(e.target.value))}
            placeholder="123456"
            className="text-center font-mono text-lg tracking-[0.3em]"
            aria-describedby="totp-code-hint"
          />
        </Field>

        <Button type="submit" className="w-full" size="lg" loading={submitting} disabled={!isTotpCode(code)}>
          <ShieldCheck className="size-4" aria-hidden />
          Verify
        </Button>

        <p className="text-center text-sm text-muted">
          <button type="button" onClick={startOver} className="font-medium text-accent hover:underline">
            Use a different account
          </button>
        </p>
      </form>
    );
  }

  return (
    <form onSubmit={onSubmit} className="space-y-5" noValidate>
      <div className="space-y-1">
        <h1 className="text-xl font-semibold tracking-tight">Sign in</h1>
        <p className="text-sm text-muted">Access your vault and fixed-term contracts.</p>
      </div>

      {params.get("expired") ? (
        <Alert tone="info" title="Your session expired">
          For your security, sessions end automatically. Please sign in again.
        </Alert>
      ) : null}
      {params.get("reset") ? (
        <Alert tone="success" title="Password changed">
          Sign in with your new password.
        </Alert>
      ) : null}
      {error ? <Alert tone="error">{error}</Alert> : null}

      <Field label="Email" htmlFor="email">
        <Input
          id="email"
          type="email"
          autoComplete="email"
          required
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          placeholder="you@example.com"
        />
      </Field>
      <Field label="Password" htmlFor="password">
        <Input
          id="password"
          type="password"
          autoComplete="current-password"
          required
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
      </Field>

      <div className="-mt-2 text-right text-sm">
        <Link href="/auth/forgot-password" className="font-medium text-accent hover:underline">
          Forgot your password?
        </Link>
      </div>

      <Button type="submit" className="w-full" size="lg" loading={submitting} disabled={!email || !password}>
        <LogIn className="size-4" aria-hidden />
        Sign in
      </Button>

      <p className="text-center text-sm text-muted">
        New to Synthetic Capital?{" "}
        <Link href="/auth/register" className="font-medium text-accent hover:underline">
          Open an account
        </Link>
      </p>
    </form>
  );
}

export default function LoginPage() {
  return (
    <Suspense>
      <LoginForm />
    </Suspense>
  );
}
