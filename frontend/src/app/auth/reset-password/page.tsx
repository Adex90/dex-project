"use client";

import { useEffect, useState, type FormEvent } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { KeyRound } from "lucide-react";

import { Alert, Button, Field, Input, fieldDescribedBy } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import { PASSWORD_MIN_LENGTH, newPasswordProblem, resetTokenFromHash } from "@/lib/mfa";
import { useAuthStore } from "@/store/auth";

/**
 * Chooses a new password with the emailed link. The token travels in the
 * link's fragment (#token=...), which browsers never send to a server; it is
 * read once and removed from the address bar.
 */
export default function ResetPasswordPage() {
  const router = useRouter();
  // undefined: not read yet (the fragment exists only in the browser).
  const [token, setToken] = useState<string | null | undefined>(undefined);
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [touched, setTouched] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    setToken(resetTokenFromHash(window.location.hash));
    window.history.replaceState(null, "", window.location.pathname);
  }, []);

  const problem = newPasswordProblem(password, confirmation);
  const hint = `${PASSWORD_MIN_LENGTH}+ characters`;

  const onSubmit = async (event: FormEvent) => {
    event.preventDefault();
    setTouched(true);
    if (!token || problem) return;
    setError(null);
    setSubmitting(true);
    try {
      await api.resetPassword(token, password);
      // Every session was revoked: whatever this tab held is gone too.
      useAuthStore.getState().logout();
      router.replace("/auth/login?reset=1");
    } catch (e) {
      if (e instanceof ApiError && e.code === "TOKEN_RESTABLECIMIENTO_INVALIDO") setToken(null);
      setError(describeError(e));
      setSubmitting(false);
    }
  };

  if (token === undefined) return null;

  if (token === null) {
    return (
      <div className="space-y-5">
        <div className="space-y-1">
          <h1 className="text-xl font-semibold tracking-tight">Reset your password</h1>
        </div>
        <Alert tone="error">
          {error ?? "This reset link is invalid or incomplete. Open the latest link from your email, or request a new one."}
        </Alert>
        <p className="text-center text-sm text-muted">
          <Link href="/auth/forgot-password" className="font-medium text-accent hover:underline">
            Request a new link
          </Link>
        </p>
      </div>
    );
  }

  return (
    <form onSubmit={onSubmit} className="space-y-5" noValidate>
      <div className="space-y-1">
        <h1 className="text-xl font-semibold tracking-tight">Choose a new password</h1>
        <p className="text-sm text-muted">
          Changing it signs you out everywhere. If you use two-step verification, you will still enter your code.
        </p>
      </div>

      {error ? <Alert tone="error">{error}</Alert> : null}

      <Field label="New password" htmlFor="password" hint={hint}>
        <Input
          id="password"
          type="password"
          autoComplete="new-password"
          required
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          aria-describedby={fieldDescribedBy("password", null, hint)}
        />
      </Field>
      <Field label="Repeat the new password" htmlFor="confirmation" error={touched ? problem : null}>
        <Input
          id="confirmation"
          type="password"
          autoComplete="new-password"
          required
          value={confirmation}
          onChange={(e) => setConfirmation(e.target.value)}
          invalid={touched && Boolean(problem)}
          aria-describedby={fieldDescribedBy("confirmation", touched ? problem : null)}
        />
      </Field>

      <Button type="submit" className="w-full" size="lg" loading={submitting} disabled={!password || !confirmation}>
        <KeyRound className="size-4" aria-hidden />
        Change password
      </Button>
    </form>
  );
}
