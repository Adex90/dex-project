"use client";

import { useCallback, useEffect, useState } from "react";
import { useRouter } from "next/navigation";

import { useDashboard } from "@/components/layout/dashboard-shell";
import { PasswordCard } from "@/components/security/password-card";
import { PreferencesCard } from "@/components/security/preferences-card";
import { SecurityLog } from "@/components/security/security-log";
import { SessionsPanel } from "@/components/security/sessions-panel";
import { Alert, Button } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import {
  DEFAULT_INACTIVITY_MINUTES,
  SECURITY_PATH,
  parseDeviceSessions,
  parseRevokedCount,
  parseSecurityEvents,
  parseSecurityPreferences,
} from "@/lib/security";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { useAuthStore } from "@/store/auth";
import { useSecurityStore } from "@/store/security";
import type {
  DeviceSession,
  DeviceSessions,
  InactivityMinutes,
  PasswordChange,
  SecurityEvent,
  SecurityPreferences,
} from "@/types/security";

const UNREADABLE = "Your security settings could not be read. Please try again.";

/*
 * Security: the client's devices and credentials (roadmap items 20 and 21).
 *
 * - Active devices (components/security/sessions-panel.tsx): every session,
 *   this one marked "Current device"; revoke one, or sign out all the others.
 * - Change password (password-card.tsx): the current password, and the
 *   authenticator code with two-step verification on.
 * - Session security (preferences-card.tsx): the inactivity timeout, two-step
 *   verification and passkey readiness.
 * - Recent security activity (security-log.tsx): the append-only log.
 *
 * Every action reloads the devices and the log, so what the page shows is
 * what the server holds. A 401 means this session itself ended (signed out
 * from another device, the password changed elsewhere): sign in again.
 *
 * Layout: one column on phones (devices, password, preferences, activity);
 * from xl, devices and activity in two thirds, password and preferences in
 * the third beside them.
 */
export default function SecurityPage() {
  const router = useRouter();
  const { token } = useDashboard();
  const [sessions, setSessions] = useState<DeviceSessions | null>(null);
  const [events, setEvents] = useState<SecurityEvent[] | null>(null);
  const [preferences, setPreferences] = useState<SecurityPreferences | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [twoStepOn, setTwoStepOn] = useState(false);

  const endSession = useCallback(() => {
    useAuthStore.getState().expire();
    router.replace(EXPIRED_LOGIN_PATH);
  }, [router]);

  /** Rethrows the API's error in English, after ending the session on a 401. */
  const fail = useCallback(
    (error: unknown): never => {
      if (error instanceof ApiError && error.isUnauthorized) endSession();
      throw new Error(describeError(error));
    },
    [endSession],
  );

  const loadSessionsAndLog = useCallback(
    async (signal?: AbortSignal) => {
      const [rawSessions, rawEvents] = await Promise.all([
        api.sessions(token, signal),
        api.securityEvents(token, signal),
      ]);
      const parsedSessions = parseDeviceSessions(rawSessions);
      const parsedEvents = parseSecurityEvents(rawEvents);
      if (!parsedSessions || !parsedEvents) throw new Error(UNREADABLE);
      setSessions(parsedSessions);
      setEvents(parsedEvents);
    },
    [token],
  );

  const load = useCallback(
    async (signal?: AbortSignal) => {
      setRefreshing(true);
      try {
        const [rawPreferences] = await Promise.all([
          api.securityPreferences(token, signal),
          loadSessionsAndLog(signal),
        ]);
        const parsed = parseSecurityPreferences(rawPreferences);
        if (!parsed) throw new Error(UNREADABLE);
        setPreferences(parsed);
        useSecurityStore.getState().setInactivityMinutes(parsed.inactivityMinutes);
        setLoadError(null);
      } catch (error) {
        if (error instanceof DOMException && error.name === "AbortError") return;
        if (error instanceof ApiError && error.isUnauthorized) {
          endSession();
          return;
        }
        setLoadError(describeError(error));
      } finally {
        setRefreshing(false);
      }
    },
    [token, loadSessionsAndLog, endSession],
  );

  useEffect(() => {
    const controller = new AbortController();
    void load(controller.signal);
    return () => controller.abort();
  }, [load]);

  // Back from turning two-step verification on: say so once, then tidy the URL.
  useEffect(() => {
    if (new URLSearchParams(window.location.search).get("two-step") !== "on") return;
    setTwoStepOn(true);
    router.replace(SECURITY_PATH);
  }, [router]);

  const afterChange = useCallback(async () => {
    try {
      await loadSessionsAndLog();
    } catch (error) {
      if (error instanceof ApiError && error.isUnauthorized) endSession();
      // The action itself succeeded; the next refresh shows the rest.
    }
  }, [loadSessionsAndLog, endSession]);

  const revoke = useCallback(
    async (session: DeviceSession) => {
      try {
        await api.revokeSession(token, session.id);
      } catch (error) {
        // Already ended elsewhere (404): the list is out of date, not wrong.
        if (error instanceof ApiError && error.status === 404) {
          await afterChange();
          throw new Error(`${session.device} had already signed out.`);
        }
        fail(error);
      }
      await afterChange();
    },
    [token, afterChange, fail],
  );

  const revokeOthers = useCallback(async () => {
    let body: unknown;
    try {
      body = await api.revokeOtherSessions(token);
    } catch (error) {
      fail(error);
    }
    await afterChange();
    return parseRevokedCount(body) ?? 0;
  }, [token, afterChange, fail]);

  const changePassword = useCallback(
    async (change: PasswordChange) => {
      let body: unknown;
      try {
        body = await api.changePassword(token, change);
      } catch (error) {
        await afterChange(); // a refused try is in the log now
        fail(error);
      }
      await afterChange();
      return parseRevokedCount(body) ?? 0;
    },
    [token, afterChange, fail],
  );

  const saveTimeout = useCallback(
    async (minutes: InactivityMinutes) => {
      let body: unknown;
      try {
        body = await api.saveSecurityPreferences(token, minutes);
      } catch (error) {
        fail(error);
      }
      const parsed = parseSecurityPreferences(body);
      if (!parsed) throw new Error(UNREADABLE);
      setPreferences(parsed);
      useSecurityStore.getState().setInactivityMinutes(parsed.inactivityMinutes);
      await afterChange();
    },
    [token, afterChange, fail],
  );

  return (
    <div className="space-y-6" data-testid="security-page">
      <div className="space-y-1">
        <p className="text-[11px] font-semibold tracking-[0.24em] text-synthetic-yellow uppercase">Security center</p>
        <h2 className="text-2xl font-semibold tracking-tight text-ink">Your devices and credentials</h2>
        <p className="max-w-2xl text-sm text-muted">
          See every device signed in to your account, sign out the ones you don&apos;t use, and keep your password and
          sign-in protections current.
        </p>
      </div>

      {twoStepOn ? (
        <Alert tone="success" title="Two-step verification is on">
          Every sign-in now asks for a code from your authenticator app. Your other devices were signed out.
        </Alert>
      ) : null}
      {loadError ? (
        <Alert tone="error" title="Could not load your security settings">
          {loadError}{" "}
          <Button variant="ghost" size="sm" onClick={() => void load()}>
            Retry
          </Button>
        </Alert>
      ) : null}

      {/* One column on phones in this order; from xl the controls take the third column, beside both. */}
      <div className="grid grid-cols-1 items-start gap-6 xl:grid-cols-3">
        <div className="min-w-0 xl:col-span-2">
          <SessionsPanel
            sessions={sessions?.sessions ?? null}
            inactivityMinutes={
              sessions?.inactivityMinutes ?? preferences?.inactivityMinutes ?? DEFAULT_INACTIVITY_MINUTES
            }
            refreshing={refreshing}
            onRefresh={() => void load()}
            onRevoke={revoke}
            onRevokeOthers={revokeOthers}
          />
        </div>
        <div className="min-w-0 space-y-6 xl:col-start-3 xl:row-span-2 xl:row-start-1">
          <PasswordCard twoStepEnabled={preferences?.twoStepEnabled ?? false} onSubmit={changePassword} />
          <PreferencesCard preferences={preferences} onSaveTimeout={saveTimeout} />
        </div>
        <div className="min-w-0 xl:col-span-2">
          <SecurityLog events={events} />
        </div>
      </div>
    </div>
  );
}
