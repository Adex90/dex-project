"use client";

import { useState, type FormEvent } from "react";
import { Check, Circle, KeyRound } from "lucide-react";

import { Alert, Button, Card, CardHeader, Field, Input, cx } from "@/components/ui/primitives";
import { normalizeTotpInput } from "@/lib/mfa";
import { passwordChangeRequest, passwordRules } from "@/lib/security";
import type { PasswordChange } from "@/types/security";

/*
 * Changing the password (roadmap item 20). The server asks for the current
 * password and, with two-step verification on, the authenticator code; each
 * wrong try counts toward the same lockout as signing in (five in a row lock
 * the account for 15 minutes and end the session that made them). The new
 * password follows the sign-in policy and must differ from the current one.
 * On success every other device is signed out; this one stays.
 */

const EMPTY = { current: "", next: "", confirmation: "", code: "" };

export function PasswordCard({
  twoStepEnabled,
  onSubmit,
}: {
  twoStepEnabled: boolean;
  /** Changes it: how many other devices were signed out. Rejects with the API's error. */
  onSubmit: (change: PasswordChange) => Promise<number>;
}) {
  const [form, setForm] = useState(EMPTY);
  const [submitting, setSubmitting] = useState(false);
  const [attempted, setAttempted] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<string | null>(null);

  const rules = passwordRules(form.current, form.next, form.confirmation);
  const prepared = passwordChangeRequest(form, twoStepEnabled);
  const set = (field: keyof typeof EMPTY) => (value: string) => {
    setForm((f) => ({ ...f, [field]: value }));
    setError(null);
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setAttempted(true);
    setDone(null);
    if ("problem" in prepared) {
      setError(prepared.problem);
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      const signedOut = await onSubmit(prepared.request);
      setForm(EMPTY);
      setAttempted(false);
      setDone(
        signedOut === 0
          ? "Password changed. No other device was signed in."
          : `Password changed. ${signedOut === 1 ? "1 other device was" : `${signedOut} other devices were`} signed out.`,
      );
    } catch (e) {
      // A wrong password or code clears that field only: the new password stays typed.
      setForm((f) => ({ ...f, current: "", code: "" }));
      setError(e instanceof Error ? e.message : "The password could not be changed.");
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Card data-testid="password-card">
      <CardHeader
        icon={<KeyRound className="size-[18px]" aria-hidden />}
        eyebrow="Credentials"
        title="Change password"
      />
      <form onSubmit={submit} className="space-y-4 p-5" noValidate>
        {done ? (
          <Alert tone="success">
            <span data-testid="password-status">{done}</span>
          </Alert>
        ) : null}
        {error ? (
          <Alert tone="error">
            <span data-testid="password-error">{error}</span>
          </Alert>
        ) : null}

        <Field label="Current password" htmlFor="current-password">
          <Input
            id="current-password"
            type="password"
            autoComplete="current-password"
            value={form.current}
            onChange={(e) => set("current")(e.target.value)}
            invalid={attempted && !form.current}
            required
          />
        </Field>
        <Field label="New password" htmlFor="new-password">
          <Input
            id="new-password"
            type="password"
            autoComplete="new-password"
            value={form.next}
            onChange={(e) => set("next")(e.target.value)}
            invalid={attempted && !rules[0].met}
            aria-describedby="password-rules"
            required
          />
        </Field>
        <Field label="Confirm new password" htmlFor="confirm-password">
          <Input
            id="confirm-password"
            type="password"
            autoComplete="new-password"
            value={form.confirmation}
            onChange={(e) => set("confirmation")(e.target.value)}
            invalid={attempted && !rules[2].met}
            required
          />
        </Field>

        <ul id="password-rules" className="space-y-1.5" aria-label="New password requirements">
          {rules.map((rule) => (
            <li
              key={rule.label}
              className={cx("flex items-center gap-2 text-xs", rule.met ? "text-ink" : "text-muted")}
              data-testid="password-rule"
              data-met={rule.met ? "true" : "false"}
            >
              {rule.met ? (
                <Check className="size-3.5 shrink-0 text-synthetic-yellow" strokeWidth={3} aria-hidden />
              ) : (
                <Circle className="size-3.5 shrink-0" aria-hidden />
              )}
              <span>
                {rule.label}
                <span className="sr-only">{rule.met ? " (met)" : " (not met yet)"}</span>
              </span>
            </li>
          ))}
        </ul>

        {twoStepEnabled ? (
          <Field label="Authenticator code" htmlFor="password-code" hint="The 6-digit code your app shows now.">
            <Input
              id="password-code"
              inputMode="numeric"
              autoComplete="one-time-code"
              pattern="[0-9]*"
              value={form.code}
              onChange={(e) => set("code")(normalizeTotpInput(e.target.value))}
              placeholder="123456"
              className="font-mono tracking-[0.3em]"
              invalid={attempted && form.code.length !== 6}
            />
          </Field>
        ) : null}

        <Button type="submit" size="lg" className="w-full" loading={submitting} data-testid="password-submit">
          Change password
        </Button>
        <p className="text-xs leading-relaxed text-muted">
          Changing it signs out every other device. Five wrong tries lock sign-in for 15 minutes.
        </p>
      </form>
    </Card>
  );
}
