/**
 * The Security Center: validating what the API answers about sessions,
 * the security log and the preferences; the words the screens use for them;
 * the password change rules; and the client side of the inactivity timeout.
 *
 * - Every session is a row on the server (migration 0019): the token names
 *   it, and signing a device out ends that token at once.
 * - The inactivity timeout runs on both sides. The server ends a session
 *   nobody used for that long (requests count, an open tab alone does not);
 *   this tab also signs out after that long without a click, key or touch,
 *   even if the dashboard keeps polling in the background.
 * - Network data is validated before anything renders it: a malformed
 *   payload is rejected whole.
 *
 * Pure functions with relative `.ts` imports, so `node --test` runs them.
 */

import { PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH } from "./mfa.ts";
import { windowLabel } from "./step-up.ts";
import { isRecord, isText } from "./vault.ts";
import type {
  DeviceSession,
  DeviceSessions,
  InactivityMinutes,
  PasswordChange,
  SecurityEvent,
  SecurityPreferences,
} from "../types/security.ts";

/** The Security Center (roadmap items 20 and 21). */
export const SECURITY_PATH = "/dashboard/security";
/** Back from turning two-step verification on (the Security page says it is on). */
export const TWO_STEP_ON_PATH = `${SECURITY_PATH}?two-step=on`;
/** Turning two-step verification on: the enrollment page, outside the dashboard. */
export const TWO_STEP_SETUP_PATH = "/auth/mfa-setup";

export const INACTIVITY_OPTIONS: readonly InactivityMinutes[] = [5, 15, 30, 60];
/** The server's default when the client never chose one (backend/src/auth/sesiones.rs). */
export const DEFAULT_INACTIVITY_MINUTES: InactivityMinutes = 30;
/** A client keeps a handful of sessions; anything far beyond that is not a list. */
const MAX_SESSIONS = 500;
const MAX_EVENTS = 100;

export function isInactivityMinutes(value: unknown): value is InactivityMinutes {
  return typeof value === "number" && (INACTIVITY_OPTIONS as readonly number[]).includes(value);
}

/** "5 min", "1 hour": the timeout's label. */
export function inactivityLabel(minutes: InactivityMinutes): string {
  return minutes === 60 ? "1 hour" : `${minutes} min`;
}

// ---------------------------------------------------------------------------
// Validating the API's answers
// ---------------------------------------------------------------------------

function isTimestamp(value: unknown): value is string {
  return isText(value, 64) && !Number.isNaN(Date.parse(value));
}

function optionalText(value: unknown, max: number): string | null | undefined {
  if (value === null || value === undefined) return null;
  return isText(value, max) ? value : undefined;
}

function parseSession(value: unknown): DeviceSession | null {
  if (!isRecord(value)) return null;
  const { id, dispositivo, ip, creada_en, ultimo_uso_en, expira_en, mfa, actual } = value;
  const address = optionalText(ip, 64);
  if (
    !isText(id, 64) ||
    !isText(dispositivo, 80) ||
    address === undefined ||
    !isTimestamp(creada_en) ||
    !isTimestamp(ultimo_uso_en) ||
    !isTimestamp(expira_en) ||
    typeof mfa !== "boolean" ||
    typeof actual !== "boolean"
  ) {
    return null;
  }
  return {
    id,
    device: dispositivo,
    ip: address,
    signedInAt: creada_en,
    lastActiveAt: ultimo_uso_en,
    expiresAt: expira_en,
    twoStep: mfa,
    current: actual,
  };
}

/** GET /api/v1/auth/sessions; null when malformed. */
export function parseDeviceSessions(value: unknown): DeviceSessions | null {
  if (!isRecord(value) || !Array.isArray(value.sesiones) || value.sesiones.length > MAX_SESSIONS) return null;
  if (!isInactivityMinutes(value.inactividad_minutos)) return null;
  const sessions: DeviceSession[] = [];
  for (const raw of value.sesiones) {
    const session = parseSession(raw);
    if (!session) return null;
    sessions.push(session);
  }
  // At most one is this tab's, and it leads the list.
  const current = sessions.filter((s) => s.current);
  if (current.length > 1) return null;
  return {
    sessions: [...current, ...sessions.filter((s) => !s.current)],
    inactivityMinutes: value.inactividad_minutos,
  };
}

function parseEvent(value: unknown): SecurityEvent | null {
  if (!isRecord(value)) return null;
  const { id, evento, ocurrido_en, ip, dispositivo, detalle, sesion_actual } = value;
  const address = optionalText(ip, 64);
  const device = optionalText(dispositivo, 80);
  const detail =
    detalle === null || detalle === undefined
      ? null
      : isText(detalle, 6) && /^\d+$/.test(detalle)
        ? Number(detalle)
        : undefined;
  if (
    typeof id !== "number" ||
    !Number.isSafeInteger(id) ||
    !isText(evento, 64) ||
    // An event code: upper case, digits and underscores ("W8BEN_CERTIFICADO").
    !/^[A-Z][A-Z0-9_]*$/.test(evento) ||
    !isTimestamp(ocurrido_en) ||
    address === undefined ||
    device === undefined ||
    detail === undefined ||
    typeof sesion_actual !== "boolean"
  ) {
    return null;
  }
  return { id, kind: evento, at: ocurrido_en, ip: address, device, detail, fromThisSession: sesion_actual };
}

/** GET /api/v1/auth/security/events, newest first; null when malformed. */
export function parseSecurityEvents(value: unknown): SecurityEvent[] | null {
  if (!isRecord(value) || !Array.isArray(value.eventos) || value.eventos.length > MAX_EVENTS) return null;
  const events: SecurityEvent[] = [];
  for (const raw of value.eventos) {
    const event = parseEvent(raw);
    if (!event) return null;
    events.push(event);
  }
  return events;
}

/** GET (and POST) /api/v1/auth/security/preferences; null when malformed. */
export function parseSecurityPreferences(value: unknown): SecurityPreferences | null {
  if (!isRecord(value)) return null;
  const { inactividad_minutos, opciones_inactividad, mfa_activo } = value;
  if (
    !isInactivityMinutes(inactividad_minutos) ||
    !Array.isArray(opciones_inactividad) ||
    opciones_inactividad.length === 0 ||
    !opciones_inactividad.every(isInactivityMinutes) ||
    typeof mfa_activo !== "boolean"
  ) {
    return null;
  }
  return { inactivityMinutes: inactividad_minutos, options: opciones_inactividad, twoStepEnabled: mfa_activo };
}

/** POST /api/v1/auth/sessions/revoke-others: how many were signed out; null when malformed. */
export function parseRevokedCount(value: unknown): number | null {
  if (!isRecord(value)) return null;
  const n = value.sesiones_revocadas;
  return typeof n === "number" && Number.isSafeInteger(n) && n >= 0 ? n : null;
}

// ---------------------------------------------------------------------------
// Words for the screens
// ---------------------------------------------------------------------------

export type DeviceKind = "phone" | "tablet" | "computer" | "unknown";

/** What kind of device a summary like "Safari on iPhone" names, for its icon. */
export function deviceKind(device: string): DeviceKind {
  if (/\b(iPhone|iPod|Android)\b/.test(device)) return "phone";
  if (/\biPad\b/.test(device)) return "tablet";
  if (/\b(macOS|Windows|Linux|ChromeOS)\b/.test(device)) return "computer";
  return "unknown";
}

export type EventTone = "neutral" | "accent" | "warning" | "danger";

export interface EventDescription {
  title: string;
  /** A second line: the device concerned, a count. */
  detail: string | null;
  tone: EventTone;
}

function plural(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}

/**
 * The log entry in words. The device in `detail` is the one concerned (the
 * one signed out, for a single revocation); the row also shows where the
 * action came from.
 */
export function describeEvent(event: SecurityEvent): EventDescription {
  const n = event.detail ?? 0;
  switch (event.kind) {
    case "INICIO_SESION":
      return { title: "Signed in", detail: event.device, tone: "neutral" };
    case "INICIO_FALLIDO":
      return { title: "Failed sign-in attempt", detail: "Wrong password", tone: "warning" };
    case "CUENTA_BLOQUEADA":
      return { title: "Sign-in locked for 15 minutes", detail: "Too many wrong passwords or codes", tone: "danger" };
    case "SESION_CERRADA":
      return { title: "Signed out", detail: event.device, tone: "neutral" };
    case "SESION_REVOCADA":
      return { title: "Device signed out", detail: event.device, tone: "accent" };
    case "SESIONES_REVOCADAS":
      // Without a session behind it, the bank signed every device out.
      return event.fromThisSession || event.device
        ? {
            title: "Signed out all other devices",
            detail: plural(n, "session ended", "sessions ended"),
            tone: "accent",
          }
        : { title: "Signed out on every device", detail: plural(n, "session ended", "sessions ended"), tone: "accent" };
    case "CONTRASENA_CAMBIADA":
      return {
        title: "Password changed",
        detail: n > 0 ? `${plural(n, "other device", "other devices")} signed out` : "No other device was signed in",
        tone: "accent",
      };
    case "CAMBIO_CONTRASENA_RECHAZADO":
      return { title: "Password change refused", detail: "Wrong current password or code", tone: "warning" };
    case "RESTABLECIMIENTO_SOLICITADO":
      return { title: "Password reset requested", detail: "A reset link was emailed", tone: "neutral" };
    case "CONTRASENA_RESTABLECIDA":
      return {
        title: "Password reset with an email link",
        detail: n > 0 ? `${plural(n, "device", "devices")} signed out` : null,
        tone: "accent",
      };
    case "PREFERENCIAS_ACTUALIZADAS":
      return {
        title: "Inactivity timeout changed",
        detail: isInactivityMinutes(n) ? `Now ${inactivityLabel(n)}` : null,
        tone: "neutral",
      };
    case "BENEFICIARIOS_DESIGNADOS":
      // Who inherits the account changed: worth noticing if it was not the client.
      return { title: "Beneficiaries updated", detail: n > 0 ? `Version ${n} on record` : null, tone: "accent" };
    case "W8BEN_CERTIFICADO":
      return { title: "W-8BEN certified", detail: n > 0 ? `Version ${n} on record` : null, tone: "accent" };
    // SPEI clarifications (migration 0025): `detail` is the case number. The client opens
    // one (from their session); our team moves it to review and closes it (no session).
    case "ACLARACION_SPEI_ABIERTA":
      return { title: "SPEI clarification opened", detail: n > 0 ? `Case #${n}` : null, tone: "accent" };
    case "ACLARACION_SPEI_EN_REVISION":
      return { title: "SPEI clarification in review", detail: n > 0 ? `Case #${n}` : null, tone: "neutral" };
    case "ACLARACION_SPEI_RESUELTA":
      return { title: "SPEI clarification resolved", detail: n > 0 ? `Case #${n}` : null, tone: "neutral" };
    case "ACLARACION_SPEI_RECHAZADA":
      return { title: "SPEI clarification declined", detail: n > 0 ? `Case #${n}` : null, tone: "neutral" };
    // Step-up (migration 0022): `detail` is the window's length in seconds.
    case "STEP_UP_VERIFICADO":
      return {
        title: "Identity confirmed",
        detail: n > 0 ? `Withdrawals and beneficiary changes unlocked for ${windowLabel(n)}` : null,
        tone: "accent",
      };
    case "STEP_UP_FALLIDO":
      return { title: "Identity check failed", detail: "Wrong code or password", tone: "warning" };
    case "MFA_SECRETO_GENERADO":
      return { title: "Two-step verification setup started", detail: null, tone: "neutral" };
    case "MFA_ACTIVADO":
      return { title: "Two-step verification turned on", detail: "Other devices were signed out", tone: "accent" };
    case "MFA_VERIFICADO":
      return { title: "Authenticator code accepted", detail: null, tone: "neutral" };
    case "MFA_CODIGO_RECHAZADO":
      return { title: "Wrong authenticator code", detail: null, tone: "warning" };
    case "MFA_DESACTIVADO":
      return { title: "Two-step verification reset by support", detail: null, tone: "warning" };
    default:
      return { title: "Security event", detail: null, tone: "neutral" };
  }
}

/** "just now", "5 min ago", "3 h ago", "2 days ago". */
export function timeAgo(iso: string, now = Date.now()): string {
  const seconds = Math.max(0, Math.round((now - Date.parse(iso)) / 1000));
  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  const days = Math.floor(hours / 24);
  return plural(days, "day", "days") + " ago";
}

/** "Nov 18, 14:05": a date and time in the reader's time zone. */
export function formatWhen(iso: string, timeZone?: string): string {
  return new Intl.DateTimeFormat("en-US", {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
    timeZone,
  }).format(new Date(iso));
}

// ---------------------------------------------------------------------------
// Password change
// ---------------------------------------------------------------------------

export interface PasswordRule {
  label: string;
  met: boolean;
}

/**
 * The new password's rules, as a checklist (the backend enforces the same:
 * backend/src/auth/password.rs): length, different from the current one,
 * confirmed.
 */
export function passwordRules(
  current: string,
  next: string,
  confirmation: string,
): [length: PasswordRule, different: PasswordRule, confirmed: PasswordRule] {
  const length = [...next].length;
  return [
    {
      label: `${PASSWORD_MIN_LENGTH} to ${PASSWORD_MAX_LENGTH} characters`,
      met: length >= PASSWORD_MIN_LENGTH && length <= PASSWORD_MAX_LENGTH,
    },
    { label: "Different from your current password", met: next.length > 0 && next !== current },
    { label: "Both new passwords match", met: next.length > 0 && next === confirmation },
  ];
}

/** The request, or why it cannot be sent yet. */
export function passwordChangeRequest(
  form: { current: string; next: string; confirmation: string; code: string },
  twoStepEnabled: boolean,
): { request: PasswordChange } | { problem: string } {
  if (!form.current) return { problem: "Enter your current password." };
  const unmet = passwordRules(form.current, form.next, form.confirmation).find((rule) => !rule.met);
  if (unmet) return { problem: `New password: ${unmet.label.toLowerCase()}.` };
  if (twoStepEnabled && !/^\d{6}$/.test(form.code))
    return { problem: "Enter the 6-digit code from your authenticator app." };
  return {
    request: twoStepEnabled
      ? { current: form.current, next: form.next, code: form.code }
      : { current: form.current, next: form.next },
  };
}

/** The body the API expects. */
export function passwordChangeBody(change: PasswordChange): Record<string, string> {
  const body: Record<string, string> = { password_actual: change.current, password_nueva: change.next };
  if (change.code) body.codigo = change.code;
  return body;
}

// ---------------------------------------------------------------------------
// Inactivity, in this tab
// ---------------------------------------------------------------------------

/** Whether a tab whose last click, key or touch was at `lastActivity` has been idle for `minutes`. */
export function isIdle(lastActivity: number, minutes: number, now = Date.now()): boolean {
  return now - lastActivity >= minutes * 60_000;
}

// ---------------------------------------------------------------------------
// Passkeys (quick unlock with Face ID, Touch ID, Windows Hello)
// ---------------------------------------------------------------------------

export type PasskeyReadiness = "checking" | "ready" | "no-authenticator" | "unsupported";

/**
 * What the browser says about a platform authenticator, in words. Status
 * only: quick unlock is not offered yet, and nothing is registered.
 */
export function passkeyStatus(readiness: PasskeyReadiness): { label: string; detail: string } {
  switch (readiness) {
    case "checking":
      return { label: "Checking this device…", detail: "Looking for a built-in authenticator." };
    case "ready":
      return {
        label: "This device is ready",
        detail: "It has a built-in authenticator (Face ID, Touch ID or Windows Hello). Quick unlock is coming soon.",
      };
    case "no-authenticator":
      return {
        label: "No built-in authenticator",
        detail: "This browser supports passkeys, but the device has no fingerprint, face or PIN unlock set up.",
      };
    case "unsupported":
      return { label: "Not supported here", detail: "This browser does not support passkeys." };
  }
}
