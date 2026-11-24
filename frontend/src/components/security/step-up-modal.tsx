"use client";

import { useEffect, useId, useRef, useState, type FormEvent } from "react";
import { KeyRound, Loader2, LockKeyhole, ShieldCheck, Smartphone } from "lucide-react";

import { cx } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import { normalizeTotpInput } from "@/lib/mfa";
import {
  DEFAULT_WINDOW_SECONDS,
  LOCK_MINUTES,
  attemptsLeftLabel,
  credentialReady,
  isVerified,
  parseStepUpStatus,
  stepUpFeedback,
  windowLabel,
  type StepUpFeedback,
} from "@/lib/step-up";
import type { StepUpCredential, StepUpRequest, StepUpStatus } from "@/types/step-up";

/*
 * Step-up: "Confirm it's you", right before a withdrawal or a change of
 * beneficiaries (roadmap item 22, migration 0022).
 *
 * - It asks for what the server will check: the 6-digit code from the
 *   authenticator app when two-step verification is on, else the account
 *   password (GET /auth/step-up says which).
 * - A right answer opens a 5-minute window on this session and the action
 *   that was waiting goes on by itself. A session already verified (another
 *   tab, a moment ago) skips the question.
 * - Every wrong try counts toward the sign-in lockout and the screen says how
 *   many are left. The one that locks the account also ends this session:
 *   the only way on is signing in again, after the lock.
 * - A native <dialog> above whatever opened it (the withdrawal drawer is one
 *   too): focus stays inside, Escape cancels (not while checking), a click
 *   outside does nothing. Sovereign Gold: square, black and charcoal, one gold
 *   action.
 */

export interface StepUpModalProps {
  token: string;
  request: StepUpRequest;
  /** The window is open: the action goes on. */
  onVerified: () => void;
  /** Closed without confirming: nothing happens. */
  onCancel: () => void;
  /** The session is over: a lockout ended it, or it had already ended. */
  onSessionEnded: (reason: "locked" | "expired") => void;
}

const UNREADABLE_STATUS = "The security check could not start. Please try again.";

const PRIMARY =
  "inline-flex h-12 min-w-0 flex-1 items-center justify-center gap-2 bg-synthetic-yellow px-4 text-sm font-bold tracking-[0.16em] text-black uppercase transition-colors hover:bg-synthetic-yellow-hover active:bg-synthetic-yellow-pressed focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-40";

const SECONDARY =
  "inline-flex h-12 items-center justify-center border border-border-strong px-5 text-sm font-semibold text-ink-2 transition-colors hover:border-ink-2 hover:text-ink focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none disabled:opacity-40";

export function StepUpModal({ token, request, onVerified, onCancel, onSessionEnded }: StepUpModalProps) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const titleId = useId();
  const descriptionId = useId();
  const inputId = useId();
  const feedbackId = useId();
  const [status, setStatus] = useState<StepUpStatus | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loadRound, setLoadRound] = useState(0);
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState<StepUpFeedback | null>(null);
  // The page's callbacks, current at every render: the status request below runs once per check.
  const callbacks = useRef({ onVerified, onSessionEnded });
  useEffect(() => {
    callbacks.current = { onVerified, onSessionEnded };
  });

  // A modal from the first frame, above whatever opened it.
  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (!dialog.open) dialog.showModal();
    return () => {
      if (dialog.open) dialog.close();
    };
  }, []);

  // Which credential the server checks, and whether the session is verified already.
  useEffect(() => {
    const controller = new AbortController();
    api.stepUpStatus(token, controller.signal).then(
      (body) => {
        const parsed = parseStepUpStatus(body);
        if (!parsed) {
          setLoadError(UNREADABLE_STATUS);
          return;
        }
        if (isVerified(parsed)) {
          callbacks.current.onVerified();
          return;
        }
        setStatus(parsed);
      },
      (error: unknown) => {
        if (controller.signal.aborted) return;
        if (error instanceof ApiError && error.isUnauthorized) {
          callbacks.current.onSessionEnded("expired");
          return;
        }
        setLoadError(describeError(error));
      },
    );
    return () => controller.abort();
  }, [token, loadRound]);

  // The field takes the focus when it appears, and again after a refused try.
  useEffect(() => {
    if (status && !busy && !feedback?.locked) inputRef.current?.focus();
  }, [status, busy, feedback]);

  const method = status?.method ?? null;
  const credential: StepUpCredential | null =
    method === "totp" ? { method, code: value } : method === "password" ? { method, password: value } : null;
  const ready = credential !== null && credentialReady(credential) && !busy && !feedback?.locked;
  const windowText = windowLabel(status?.windowSeconds ?? DEFAULT_WINDOW_SECONDS);

  function close() {
    if (busy) return;
    if (feedback?.sessionEnded) onSessionEnded("locked");
    else onCancel();
  }

  async function onSubmit(event: FormEvent) {
    event.preventDefault();
    if (!ready || !credential) return;
    setBusy(true);
    setFeedback(null);
    try {
      // A 2xx opened the window, whatever its body says.
      await api.stepUp(token, credential);
      onVerified();
    } catch (error) {
      const next = stepUpFeedback(error, describeError);
      if (next.sessionEnded && !next.locked) {
        onSessionEnded("expired");
        return;
      }
      setFeedback(next);
      setValue("");
      setBusy(false);
    }
  }

  return (
    <dialog
      ref={dialogRef}
      aria-labelledby={titleId}
      aria-describedby={descriptionId}
      onCancel={(event) => {
        // Escape: the page decides what closing means.
        event.preventDefault();
        close();
      }}
      className={cx(
        // Phone: a sheet along the bottom edge. Wider: a panel in the middle.
        "inset-0 mx-0 mt-auto mb-0 h-fit max-h-[92dvh] w-full max-w-full overflow-y-auto p-0",
        "border-t-2 border-synthetic-yellow bg-surface text-ink sm:m-auto sm:max-w-md sm:border sm:border-t-2 sm:border-x-border sm:border-b-border",
        "backdrop:bg-black/80 backdrop:backdrop-blur-[2px]",
        "transition duration-200 starting:translate-y-6 starting:opacity-0 motion-reduce:transition-none",
      )}
      data-testid="step-up-modal"
      data-method={method ?? "loading"}
      data-state={feedback?.locked ? "locked" : busy ? "checking" : "ready"}
    >
      <header className="border-b border-border px-5 pt-4 pb-3.5">
        <p className="flex items-center gap-1.5 text-[11px] font-semibold tracking-[0.24em] text-synthetic-yellow uppercase">
          <ShieldCheck className="size-3.5" aria-hidden /> Security check
        </p>
        <h2 id={titleId} className="mt-1 text-base font-semibold tracking-tight">
          Confirm it&apos;s you
        </h2>
      </header>

      <form onSubmit={onSubmit} noValidate>
        <div className="space-y-4 px-5 py-5">
          <div className="border border-border bg-black/40 px-4 py-3" data-testid="step-up-action">
            <p className="text-[10px] font-semibold tracking-[0.2em] text-muted uppercase">Waiting for you</p>
            <p className="mt-1 text-sm font-semibold break-words text-ink">{request.action}</p>
            {request.detail ? <p className="mt-0.5 text-xs break-words text-ink-2">{request.detail}</p> : null}
          </div>

          <p id={descriptionId} className="text-sm leading-relaxed text-ink-2">
            {method === "totp"
              ? "Enter the 6-digit code your authenticator app shows now."
              : method === "password"
                ? "Enter your account password."
                : "Withdrawals and changes to your beneficiaries need a fresh check."}{" "}
            Once confirmed, sensitive actions stay unlocked on this device for {windowText}.
          </p>

          {loadError ? (
            <div role="alert" className="border border-danger/40 bg-danger-soft px-3 py-3 text-sm text-danger">
              <p>{loadError}</p>
              <button
                type="button"
                onClick={() => {
                  setLoadError(null);
                  setLoadRound((n) => n + 1);
                }}
                className="mt-2 text-xs font-semibold tracking-[0.16em] text-synthetic-yellow uppercase hover:underline focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
              >
                Try again
              </button>
            </div>
          ) : status === null ? (
            <div className="flex h-14 items-center gap-2 text-sm text-muted" aria-busy="true">
              <Loader2 className="size-4 animate-spin text-synthetic-yellow" aria-hidden /> Preparing the check…
            </div>
          ) : feedback?.locked ? null : (
            <div className="space-y-1.5">
              <label htmlFor={inputId} className="flex items-center gap-1.5 text-xs font-medium text-ink-2">
                {method === "totp" ? (
                  <Smartphone className="size-3.5 text-synthetic-yellow" aria-hidden />
                ) : (
                  <KeyRound className="size-3.5 text-synthetic-yellow" aria-hidden />
                )}
                {method === "totp" ? "Authenticator code" : "Password"}
              </label>
              {method === "totp" ? (
                <input
                  ref={inputRef}
                  id={inputId}
                  value={value}
                  onChange={(e) => {
                    setValue(normalizeTotpInput(e.target.value));
                    setFeedback(null);
                  }}
                  inputMode="numeric"
                  autoComplete="one-time-code"
                  pattern="[0-9]*"
                  maxLength={7}
                  placeholder="000000"
                  disabled={busy}
                  aria-invalid={feedback ? true : undefined}
                  aria-describedby={feedback ? feedbackId : undefined}
                  className="h-16 w-full border border-border-strong bg-black px-4 text-center font-mono text-3xl font-semibold tracking-[0.5em] text-ink tabular-nums placeholder:text-muted/40 focus:border-synthetic-yellow focus:ring-1 focus:ring-synthetic-yellow focus:outline-none disabled:opacity-60"
                  data-testid="step-up-code"
                />
              ) : (
                <input
                  ref={inputRef}
                  id={inputId}
                  type="password"
                  value={value}
                  onChange={(e) => {
                    setValue(e.target.value);
                    setFeedback(null);
                  }}
                  autoComplete="current-password"
                  maxLength={128}
                  disabled={busy}
                  aria-invalid={feedback ? true : undefined}
                  aria-describedby={feedback ? feedbackId : undefined}
                  className="h-12 w-full border border-border-strong bg-black px-4 text-base text-ink focus:border-synthetic-yellow focus:ring-1 focus:ring-synthetic-yellow focus:outline-none disabled:opacity-60"
                  data-testid="step-up-password"
                />
              )}
            </div>
          )}

          {feedback ? (
            <div
              id={feedbackId}
              role="alert"
              className={cx(
                "flex gap-2.5 border px-3 py-3 text-sm",
                feedback.locked
                  ? "border-danger/50 bg-danger-soft text-danger"
                  : "border-warning/40 bg-warning-soft text-warning",
              )}
              data-testid="step-up-error"
            >
              <LockKeyhole className="mt-0.5 size-4 shrink-0" aria-hidden />
              <div className="min-w-0 space-y-1">
                <p className="font-semibold">{feedback.message}</p>
                {!feedback.locked && feedback.remainingAttempts !== null ? (
                  <p className="text-[13px]" data-testid="step-up-attempts">
                    {attemptsLeftLabel(feedback.remainingAttempts)}
                  </p>
                ) : null}
              </div>
            </div>
          ) : null}

          {method === "password" && !feedback?.locked ? (
            <p className="text-xs leading-relaxed text-muted">
              Turn on two-step verification in Security to confirm with your authenticator app instead.
            </p>
          ) : null}
        </div>

        <footer className="space-y-3 border-t border-border px-5 py-4">
          <div className="flex gap-2">
            {feedback?.locked ? (
              feedback.sessionEnded ? (
                <button
                  type="button"
                  onClick={() => onSessionEnded("locked")}
                  className={PRIMARY}
                  data-testid="step-up-sign-in"
                >
                  Sign in again
                </button>
              ) : (
                <button type="button" onClick={onCancel} className={PRIMARY} data-testid="step-up-close">
                  Close
                </button>
              )
            ) : (
              <>
                <button
                  type="button"
                  onClick={close}
                  disabled={busy}
                  className={SECONDARY}
                  data-testid="step-up-cancel"
                >
                  Cancel
                </button>
                <button type="submit" disabled={!ready} className={PRIMARY} data-testid="step-up-submit">
                  {busy ? <Loader2 className="size-4 animate-spin" aria-hidden /> : null}
                  {busy ? "Checking" : "Verify & continue"}
                </button>
              </>
            )}
          </div>
          <p className="text-[11px] leading-relaxed text-muted">
            Five wrong tries lock your account for {LOCK_MINUTES} minutes. Every check is in your security log.
          </p>
        </footer>
      </form>
    </dialog>
  );
}
