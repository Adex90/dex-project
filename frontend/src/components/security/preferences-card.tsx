"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { Fingerprint, ShieldCheck, SlidersHorizontal, Timer } from "lucide-react";

import { Badge, Card, CardHeader, Skeleton, cx } from "@/components/ui/primitives";
import { SegmentedControl } from "@/components/ui/segmented-control";
import {
  INACTIVITY_OPTIONS,
  TWO_STEP_SETUP_PATH,
  inactivityLabel,
  isInactivityMinutes,
  passkeyStatus,
  type PasskeyReadiness,
} from "@/lib/security";
import type { InactivityMinutes, SecurityPreferences } from "@/types/security";

/*
 * Session security preferences (roadmap item 20):
 *
 * - The inactivity timeout: saved as soon as it is picked. The server ends
 *   a session unused that long, and this tab signs out after that long
 *   without a click, key or touch.
 * - Two-step verification: on, or a way to turn it on (the enrollment page).
 * - Quick unlock with a passkey (Face ID, Touch ID, Windows Hello): whether
 *   this device could, from the browser's own answer. Status only: nothing
 *   is registered yet.
 */

const OPTIONS = INACTIVITY_OPTIONS.map((minutes) => ({ value: String(minutes), label: inactivityLabel(minutes) }));

export function PreferencesCard({
  preferences,
  onSaveTimeout,
}: {
  /** Null while loading. */
  preferences: SecurityPreferences | null;
  /** Saves the timeout; rejects with the API's error. */
  onSaveTimeout: (minutes: InactivityMinutes) => Promise<void>;
}) {
  const [shown, setShown] = useState<InactivityMinutes | null>(null);
  const [saving, setSaving] = useState(false);
  const [status, setStatus] = useState<{ tone: "ok" | "error"; text: string } | null>(null);
  const passkey = usePasskeyReadiness();

  const timeout = shown ?? preferences?.inactivityMinutes ?? null;

  const choose = async (value: string) => {
    const minutes = Number(value);
    if (!isInactivityMinutes(minutes) || !preferences || minutes === timeout) return;
    const previous = timeout;
    setShown(minutes);
    setSaving(true);
    setStatus(null);
    try {
      await onSaveTimeout(minutes);
      setStatus({ tone: "ok", text: `Saved: sessions now end after ${inactivityLabel(minutes)} without activity.` });
    } catch (error) {
      setShown(previous);
      setStatus({ tone: "error", text: error instanceof Error ? error.message : "The timeout could not be saved." });
    } finally {
      setSaving(false);
    }
  };

  const passkeyWords = passkeyStatus(passkey);

  return (
    <Card data-testid="preferences-card">
      <CardHeader
        icon={<SlidersHorizontal className="size-[18px]" aria-hidden />}
        eyebrow="Preferences"
        title="Session security"
      />
      <div className="divide-y divide-border">
        <section className="space-y-3 px-5 py-4" aria-labelledby="timeout-title">
          <div className="flex items-start gap-2.5">
            <Timer className="mt-0.5 size-4 shrink-0 text-synthetic-yellow" aria-hidden />
            <div className="min-w-0">
              <h3 id="timeout-title" className="text-sm font-semibold text-ink">
                Sign out after inactivity
              </h3>
              <p className="text-xs leading-relaxed text-muted">
                On every device: no activity for this long ends the session.
              </p>
            </div>
          </div>
          {timeout === null ? (
            <Skeleton className="h-10 w-full" />
          ) : (
            <SegmentedControl
              legend="Sign out after inactivity"
              options={OPTIONS}
              value={String(timeout)}
              onChange={(value) => void choose(value)}
              disabled={saving}
              testId="timeout-control"
            />
          )}
          <p
            className={cx("min-h-4 text-xs", status?.tone === "error" ? "text-danger" : "text-ink-2")}
            aria-live="polite"
            data-testid="timeout-status"
          >
            {status?.text ?? ""}
          </p>
        </section>

        <section className="space-y-2 px-5 py-4" aria-labelledby="two-step-title" data-testid="two-step-status">
          <div className="flex items-center justify-between gap-3">
            <div className="flex min-w-0 items-center gap-2.5">
              <ShieldCheck className="size-4 shrink-0 text-synthetic-yellow" aria-hidden />
              <h3 id="two-step-title" className="text-sm font-semibold text-ink">
                Two-step verification
              </h3>
            </div>
            {preferences === null ? (
              <Skeleton className="h-5 w-12" />
            ) : preferences.twoStepEnabled ? (
              <Badge tone="accent" className="bg-synthetic-yellow text-black">
                On
              </Badge>
            ) : (
              <Badge tone="neutral">Off</Badge>
            )}
          </div>
          {preferences?.twoStepEnabled ? (
            <p className="text-xs leading-relaxed text-muted">
              Every sign-in asks for a code from your authenticator app. Moving it to a new phone goes through support.
            </p>
          ) : preferences ? (
            <div className="space-y-3">
              <p className="text-xs leading-relaxed text-muted">
                Add a code from your phone to every sign-in, so a password alone is never enough. Turning it on signs
                out every other device.
              </p>
              <Link
                href={TWO_STEP_SETUP_PATH}
                className="inline-flex h-10 w-full items-center justify-center gap-2 border border-synthetic-yellow px-4 text-sm font-semibold text-synthetic-yellow transition-colors hover:bg-synthetic-yellow hover:text-black focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
                data-testid="two-step-setup"
              >
                Turn on two-step verification
              </Link>
            </div>
          ) : null}
        </section>

        <section className="space-y-2 px-5 py-4" aria-labelledby="passkey-title" data-testid="passkey-status">
          <div className="flex items-center justify-between gap-3">
            <div className="flex min-w-0 items-center gap-2.5">
              <Fingerprint className="size-4 shrink-0 text-synthetic-yellow" aria-hidden />
              <h3 id="passkey-title" className="text-sm font-semibold text-ink">
                Quick unlock with a passkey
              </h3>
            </div>
            <Badge tone="neutral">Coming soon</Badge>
          </div>
          <p className="text-xs font-medium text-ink-2" data-testid="passkey-readiness" data-readiness={passkey}>
            {passkeyWords.label}
          </p>
          <p className="text-xs leading-relaxed text-muted">{passkeyWords.detail}</p>
        </section>
      </div>
    </Card>
  );
}

/** Whether this browser and device could unlock with a passkey (WebAuthn platform authenticator). */
function usePasskeyReadiness(): PasskeyReadiness {
  const [readiness, setReadiness] = useState<PasskeyReadiness>("checking");
  useEffect(() => {
    let alive = true;
    const credential = typeof window === "undefined" ? undefined : window.PublicKeyCredential;
    if (!credential || typeof credential.isUserVerifyingPlatformAuthenticatorAvailable !== "function") {
      setReadiness("unsupported");
      return;
    }
    credential.isUserVerifyingPlatformAuthenticatorAvailable().then(
      (available) => alive && setReadiness(available ? "ready" : "no-authenticator"),
      () => alive && setReadiness("unsupported"),
    );
    return () => {
      alive = false;
    };
  }, []);
  return readiness;
}
