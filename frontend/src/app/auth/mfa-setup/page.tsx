"use client";

import { useEffect, useRef, useState, type FormEvent } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { ShieldCheck } from "lucide-react";

import { Alert, Button, Field, Input, Skeleton } from "@/components/ui/primitives";
import { NO_CONSOLE_MESSAGE, homeAfterSignIn } from "@/hooks/use-client-home";
import { ApiError, describeError } from "@/lib/api/client";
import type { ConfiguracionMfa } from "@/lib/api/types";
import { formatSecretForManualEntry, isTotpCode, normalizeTotpInput } from "@/lib/mfa";
import { isPendingUsable, useAuthStore } from "@/store/auth";

/**
 * Two-step verification enrollment. Staff reach it from sign-in (their
 * password alone never starts a session): scan the QR code with an
 * authenticator app, confirm the first code, and the session starts.
 */
export default function MfaSetupPage() {
  const router = useRouter();
  const pending = useAuthStore((s) => s.pending);
  const setupMfa = useAuthStore((s) => s.setupMfa);
  const enableMfa = useAuthStore((s) => s.enableMfa);
  const clearPending = useAuthStore((s) => s.clearPending);
  const [config, setConfig] = useState<ConfiguracionMfa | null>(null);
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const requested = useRef(false);

  const enrolling = pending?.kind === "enroll" && isPendingUsable(pending);

  // An enrollment that can no longer finish (expired, revoked, locked) goes
  // back to sign-in; a wrong code just tries again.
  const handle = (e: unknown) => {
    if (e instanceof ApiError && (e.code === "DESAFIO_MFA_VENCIDO" || e.code === "BLOQUEADO" || e.isUnauthorized)) {
      clearPending();
      setConfig(null);
    }
    setError(describeError(e));
  };

  useEffect(() => {
    // One secret per visit (React may run effects twice in development).
    if (!enrolling || requested.current) return;
    requested.current = true;
    setupMfa().then(setConfig, handle);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- run once per enrollment
  }, [enrolling]);

  const onSubmit = async (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    setSubmitting(true);
    try {
      const session = await enableMfa(code);
      const home = await homeAfterSignIn(session);
      if (home) {
        router.replace(home);
        return;
      }
      useAuthStore.getState().logout();
      setNotice(NO_CONSOLE_MESSAGE);
      setSubmitting(false);
    } catch (e) {
      handle(e);
      setCode("");
      setSubmitting(false);
    }
  };

  if (notice) {
    return (
      <div className="space-y-5">
        <Alert tone="success" title="Two-step verification is on">
          {notice}
        </Alert>
        <Link href="/auth/login" className="block text-center text-sm font-medium text-accent hover:underline">
          Back to sign in
        </Link>
      </div>
    );
  }

  if (!enrolling && !config) {
    return (
      <div className="space-y-5">
        <div className="space-y-1">
          <h1 className="text-xl font-semibold tracking-tight">Set up two-step verification</h1>
          <p className="text-sm text-muted">Sign in first: staff set up two-step verification as part of signing in.</p>
        </div>
        {error ? <Alert tone="error">{error}</Alert> : null}
        <Link href="/auth/login" className="block text-center text-sm font-medium text-accent hover:underline">
          Go to sign in
        </Link>
      </div>
    );
  }

  return (
    <div className="space-y-5">
      <div className="space-y-1">
        <h1 className="text-xl font-semibold tracking-tight">Set up two-step verification</h1>
        <p className="text-sm text-muted">
          Your role requires a code from your phone every time you sign in. Set it up once with an authenticator
          app such as Google Authenticator, Microsoft Authenticator or 1Password.
        </p>
      </div>

      {error ? <Alert tone="error">{error}</Alert> : null}

      {config ? (
        <>
          <section className="space-y-3" aria-labelledby="mfa-scan">
            <h2 id="mfa-scan" className="text-sm font-medium">
              1. Scan this QR code with the app
            </h2>
            <div className="flex justify-center">
              {/* eslint-disable-next-line @next/next/no-img-element -- SVG data URL from the API, nothing to optimize */}
              <img
                src={config.qr_svg}
                alt="QR code that adds your Synthetic Capital account to an authenticator app"
                width={192}
                height={192}
                className="rounded-lg border border-border bg-white p-2"
                data-testid="mfa-qr"
              />
            </div>
            <div className="space-y-1.5">
              <p className="text-xs text-muted">Cannot scan it? Add a time-based account with this key:</p>
              <code
                data-testid="mfa-secret"
                className="block break-all rounded-lg bg-surface-2 px-3 py-2 text-center font-mono text-sm tracking-wider text-ink"
              >
                {formatSecretForManualEntry(config.secreto)}
              </code>
              <p className="text-center text-xs text-muted">
                {config.emisor} · {config.cuenta}
              </p>
            </div>
          </section>

          <form onSubmit={onSubmit} className="space-y-4" noValidate>
            <Field label="2. Enter the 6-digit code the app shows" htmlFor="totp-code" hint="The code changes every 30 seconds.">
              <Input
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
              Turn on two-step verification
            </Button>
          </form>
        </>
      ) : (
        <div className="space-y-3" aria-busy="true" aria-label="Preparing your authenticator key">
          <Skeleton className="mx-auto size-48" />
          <Skeleton className="h-9 w-full" />
        </div>
      )}
    </div>
  );
}
