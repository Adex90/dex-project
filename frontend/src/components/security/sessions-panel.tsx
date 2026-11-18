"use client";

import { useState } from "react";
import { HelpCircle, Laptop, LogOut, MonitorSmartphone, RefreshCw, Smartphone, Tablet } from "lucide-react";

import { Alert, Button, Card, CardHeader, Skeleton, cx } from "@/components/ui/primitives";
import { deviceKind, formatWhen, inactivityLabel, timeAgo } from "@/lib/security";
import type { DeviceSession, InactivityMinutes } from "@/types/security";

/*
 * Active devices (roadmap item 21): every signed-in session, this one first
 * with its gold "Current device" badge. Each other device can be signed out
 * on its own ("Revoke"), or all of them at once with the panic button, which
 * asks once before acting. A revoked device's token stops working at once:
 * its next request, or its live connection within a minute, ends there.
 */

const ICONS = { phone: Smartphone, tablet: Tablet, computer: Laptop, unknown: HelpCircle } as const;

export function SessionsPanel({
  sessions,
  inactivityMinutes,
  refreshing,
  onRefresh,
  onRevoke,
  onRevokeOthers,
}: {
  /** Null while loading. */
  sessions: DeviceSession[] | null;
  inactivityMinutes: InactivityMinutes;
  refreshing: boolean;
  onRefresh: () => void;
  /** Signs one device out; rejects with the API's error. */
  onRevoke: (session: DeviceSession) => Promise<void>;
  /** Signs every other device out: how many were. */
  onRevokeOthers: () => Promise<number>;
}) {
  const [revoking, setRevoking] = useState<string | null>(null);
  const [confirming, setConfirming] = useState(false);
  const [panicking, setPanicking] = useState(false);
  const [status, setStatus] = useState<{ tone: "success" | "error"; text: string } | null>(null);

  const others = sessions?.filter((s) => !s.current) ?? [];

  const revoke = async (session: DeviceSession) => {
    setRevoking(session.id);
    setStatus(null);
    try {
      await onRevoke(session);
      setStatus({ tone: "success", text: `${session.device} was signed out.` });
    } catch (error) {
      setStatus({
        tone: "error",
        text: error instanceof Error ? error.message : "That device could not be signed out.",
      });
    } finally {
      setRevoking(null);
    }
  };

  const revokeOthers = async () => {
    setPanicking(true);
    setStatus(null);
    try {
      const n = await onRevokeOthers();
      setConfirming(false);
      setStatus({
        tone: "success",
        text:
          n === 0
            ? "No other device was signed in."
            : `${n === 1 ? "1 other device was" : `${n} other devices were`} signed out. They need your password to sign in again.`,
      });
    } catch (error) {
      setStatus({
        tone: "error",
        text: error instanceof Error ? error.message : "The other devices could not be signed out.",
      });
    } finally {
      setPanicking(false);
    }
  };

  return (
    <Card data-testid="sessions-panel">
      <CardHeader
        icon={<MonitorSmartphone className="size-[18px]" aria-hidden />}
        eyebrow="Active devices"
        title="Where you're signed in"
        action={
          <Button variant="ghost" size="sm" onClick={onRefresh} disabled={refreshing} aria-label="Refresh devices">
            <RefreshCw className={cx("size-4", refreshing && "animate-spin")} aria-hidden />
          </Button>
        }
      />

      {sessions === null ? (
        <div className="space-y-3 p-5" aria-busy="true" aria-label="Loading your devices">
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
        </div>
      ) : (
        <ul className="divide-y divide-border" aria-label="Signed-in devices">
          {sessions.map((session) => (
            <SessionRow
              key={session.id}
              session={session}
              revoking={revoking === session.id}
              disabled={revoking !== null || panicking}
              onRevoke={() => void revoke(session)}
            />
          ))}
        </ul>
      )}

      <div className="space-y-4 border-t border-border px-5 py-4">
        {status ? (
          <Alert tone={status.tone} className="text-left">
            <span data-testid="sessions-status">{status.text}</span>
          </Alert>
        ) : null}

        {/* The panic button: high contrast, one confirmation, then every other device is out. */}
        <div className="space-y-3 border border-synthetic-yellow/40 bg-synthetic-yellow/5 p-4" data-testid="panic-zone">
          <div className="space-y-1">
            <p className="text-sm font-semibold text-ink">Lost a device, or see one you don&apos;t recognize?</p>
            <p className="text-xs leading-relaxed text-muted">
              Sign out everywhere except here. Then change your password if someone else may know it.
            </p>
          </div>
          {confirming ? (
            <div className="space-y-3" role="group" aria-label="Confirm signing out the other devices">
              <p className="text-sm font-medium text-ink" data-testid="panic-confirm-text">
                Sign out {others.length === 1 ? "1 other device" : `${others.length} other devices`} now?
              </p>
              <div className="grid gap-2 sm:grid-cols-2">
                <Button
                  size="lg"
                  onClick={() => void revokeOthers()}
                  loading={panicking}
                  data-testid="panic-confirm"
                  className="bg-synthetic-yellow font-semibold text-black hover:bg-synthetic-yellow-hover"
                >
                  Yes, sign them out
                </Button>
                <Button size="lg" variant="secondary" onClick={() => setConfirming(false)} disabled={panicking}>
                  Cancel
                </Button>
              </div>
            </div>
          ) : (
            <Button
              size="lg"
              className="w-full bg-synthetic-yellow font-semibold tracking-wide text-black uppercase hover:bg-synthetic-yellow-hover"
              disabled={sessions === null || others.length === 0 || revoking !== null}
              onClick={() => {
                setStatus(null);
                setConfirming(true);
              }}
              data-testid="panic-button"
            >
              <LogOut className="size-4" aria-hidden />
              Sign out all other devices
            </Button>
          )}
          {sessions !== null && others.length === 0 && !confirming ? (
            <p className="text-xs text-muted" data-testid="no-other-devices">
              No other device is signed in.
            </p>
          ) : null}
        </div>

        <p className="text-xs leading-relaxed text-muted">
          A session ends after {inactivityLabel(inactivityMinutes)} without activity, and always by the time shown on
          it. Signing out on one device never affects the others.
        </p>
      </div>
    </Card>
  );
}

function SessionRow({
  session,
  revoking,
  disabled,
  onRevoke,
}: {
  session: DeviceSession;
  revoking: boolean;
  disabled: boolean;
  onRevoke: () => void;
}) {
  const Icon = ICONS[deviceKind(session.device)];
  return (
    <li
      className={cx("flex items-start gap-3 px-5 py-4", session.current && "bg-synthetic-yellow/[0.04]")}
      data-testid="session-row"
      data-current={session.current ? "true" : "false"}
      data-session-id={session.id}
    >
      <div
        className={cx(
          "flex size-10 shrink-0 items-center justify-center border",
          session.current
            ? "border-synthetic-yellow/50 bg-synthetic-yellow/10 text-synthetic-yellow"
            : "border-border bg-surface-2 text-ink-2",
        )}
      >
        <Icon className="size-5" aria-hidden />
      </div>
      <div className="min-w-0 flex-1 space-y-1">
        <div className="flex flex-wrap items-center gap-2">
          <p className="truncate text-sm font-semibold text-ink" data-testid="session-device">
            {session.device}
          </p>
          {session.current ? (
            <span
              className="inline-flex items-center bg-synthetic-yellow px-2 py-0.5 text-[10px] font-bold tracking-[0.12em] text-black uppercase"
              data-testid="current-device-badge"
            >
              Current device
            </span>
          ) : null}
          {session.twoStep ? (
            <span className="text-[10px] font-semibold tracking-[0.12em] text-muted uppercase">Two-step</span>
          ) : null}
        </div>
        <p className="text-xs text-muted tabular-nums">
          {session.ip ? <span data-testid="session-ip">IP {session.ip} · </span> : null}
          Signed in {formatWhen(session.signedInAt)}
        </p>
        <p className="text-xs text-ink-2 tabular-nums">
          {session.current ? (
            <span className="font-medium text-synthetic-yellow">Active now</span>
          ) : (
            <>Active {timeAgo(session.lastActiveAt)}</>
          )}
          <span className="text-muted"> · Ends by {formatWhen(session.expiresAt)}</span>
        </p>
      </div>
      {session.current ? null : (
        <Button
          variant="secondary"
          size="sm"
          onClick={onRevoke}
          loading={revoking}
          disabled={disabled && !revoking}
          aria-label={`Revoke ${session.device}`}
          data-testid="revoke-session"
          className="shrink-0 hover:border-danger hover:text-danger"
        >
          Revoke
        </Button>
      )}
    </li>
  );
}
