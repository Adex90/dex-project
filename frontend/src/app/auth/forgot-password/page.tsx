"use client";

import { useState, type FormEvent } from "react";
import Link from "next/link";
import { Mail } from "lucide-react";

import { Alert, Button, Field, Input } from "@/components/ui/primitives";
import { describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";

/**
 * Asks for a password reset link. The answer is the same whether or not the
 * email has an account, so this page never reveals who is a customer.
 */
export default function ForgotPasswordPage() {
  const [email, setEmail] = useState("");
  const [sent, setSent] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const onSubmit = async (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    setSubmitting(true);
    try {
      await api.forgotPassword(email.trim());
      setSent(true);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setSubmitting(false);
    }
  };

  if (sent) {
    return (
      <div className="space-y-5">
        <div className="space-y-1">
          <h1 className="text-xl font-semibold tracking-tight">Check your email</h1>
        </div>
        <Alert tone="success" title="Request received">
          If an account exists for <span className="font-medium">{email.trim()}</span>, we sent it a link to choose a
          new password. The link works once and expires in 30 minutes.
        </Alert>
        <p className="text-center text-sm text-muted">
          <Link href="/auth/login" className="font-medium text-accent hover:underline">
            Back to sign in
          </Link>
        </p>
      </div>
    );
  }

  return (
    <form onSubmit={onSubmit} className="space-y-5" noValidate>
      <div className="space-y-1">
        <h1 className="text-xl font-semibold tracking-tight">Reset your password</h1>
        <p className="text-sm text-muted">Enter your account&apos;s email and we will send you a link to choose a new password.</p>
      </div>

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

      <Button type="submit" className="w-full" size="lg" loading={submitting} disabled={!email.includes("@")}>
        <Mail className="size-4" aria-hidden />
        Send reset link
      </Button>

      <p className="text-center text-sm text-muted">
        Remembered it?{" "}
        <Link href="/auth/login" className="font-medium text-accent hover:underline">
          Sign in
        </Link>
      </p>
    </form>
  );
}
