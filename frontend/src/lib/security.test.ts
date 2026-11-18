/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  DEFAULT_INACTIVITY_MINUTES,
  INACTIVITY_OPTIONS,
  describeEvent,
  deviceKind,
  formatWhen,
  inactivityLabel,
  isIdle,
  parseDeviceSessions,
  parseRevokedCount,
  parseSecurityEvents,
  parseSecurityPreferences,
  passkeyStatus,
  passwordChangeBody,
  passwordChangeRequest,
  passwordRules,
  timeAgo,
} from "./security.ts";
import type { SecurityEvent } from "../types/security.ts";

const MAC = {
  id: "5e000000-0000-0000-0000-000000000001",
  dispositivo: "Chrome on macOS",
  ip: "203.0.113.7",
  creada_en: "2026-11-18T10:00:00Z",
  ultimo_uso_en: "2026-11-18T10:20:00Z",
  expira_en: "2026-11-18T11:00:00Z",
  mfa: false,
  actual: true,
};
const IPHONE = {
  ...MAC,
  id: "5e000000-0000-0000-0000-000000000002",
  dispositivo: "Safari on iPhone",
  ip: null,
  actual: false,
};

test("sessions: validated, the current one first", () => {
  const parsed = parseDeviceSessions({ sesiones: [IPHONE, MAC], inactividad_minutos: 30 });
  assert.ok(parsed);
  assert.deepEqual(
    parsed.sessions.map((s) => [s.device, s.current, s.ip]),
    [
      ["Chrome on macOS", true, "203.0.113.7"],
      ["Safari on iPhone", false, null],
    ],
  );
  assert.equal(parsed.inactivityMinutes, 30);
  assert.equal(parsed.sessions[0]?.signedInAt, MAC.creada_en);
});

test("sessions: anything malformed is rejected whole", () => {
  for (const bad of [
    null,
    { sesiones: "x", inactividad_minutos: 30 },
    { sesiones: [MAC], inactividad_minutos: 10 },
    { sesiones: [{ ...MAC, actual: "yes" }], inactividad_minutos: 30 },
    { sesiones: [{ ...MAC, creada_en: "not a date" }], inactividad_minutos: 30 },
    { sesiones: [{ ...MAC, dispositivo: "" }], inactividad_minutos: 30 },
    { sesiones: [{ ...MAC, dispositivo: "x".repeat(81) }], inactividad_minutos: 30 },
    { sesiones: [MAC, { ...IPHONE, actual: true }], inactividad_minutos: 30 },
  ]) {
    assert.equal(parseDeviceSessions(bad), null, JSON.stringify(bad));
  }
});

const EVENT = {
  id: 7,
  evento: "SESION_REVOCADA",
  ocurrido_en: "2026-11-18T10:21:00Z",
  ip: "203.0.113.7",
  dispositivo: "Safari on iPhone",
  detalle: null,
  sesion_actual: true,
};

test("security log: validated, with the numeric detail as a number", () => {
  const parsed = parseSecurityEvents({
    eventos: [EVENT, { ...EVENT, id: 6, evento: "SESIONES_REVOCADAS", detalle: "3" }],
  });
  assert.ok(parsed);
  assert.deepEqual(
    parsed.map((e) => [e.kind, e.detail, e.fromThisSession]),
    [
      ["SESION_REVOCADA", null, true],
      ["SESIONES_REVOCADAS", 3, true],
    ],
  );
  for (const bad of [
    { eventos: [{ ...EVENT, detalle: "tres" }] },
    { eventos: [{ ...EVENT, evento: "<script>" }] },
    { eventos: [{ ...EVENT, id: "7" }] },
    { eventos: [{ ...EVENT, sesion_actual: null }] },
    { eventos: Array.from({ length: 101 }, () => EVENT) },
    { eventos: null },
  ]) {
    assert.equal(parseSecurityEvents(bad), null, JSON.stringify(bad).slice(0, 80));
  }
});

test("preferences and the revoke-others answer", () => {
  assert.deepEqual(
    parseSecurityPreferences({ inactividad_minutos: 15, opciones_inactividad: [5, 15, 30, 60], mfa_activo: true }),
    { inactivityMinutes: 15, options: [5, 15, 30, 60], twoStepEnabled: true },
  );
  assert.equal(
    parseSecurityPreferences({ inactividad_minutos: 20, opciones_inactividad: [5], mfa_activo: true }),
    null,
  );
  assert.equal(
    parseSecurityPreferences({ inactividad_minutos: 15, opciones_inactividad: [7], mfa_activo: true }),
    null,
  );
  assert.equal(parseSecurityPreferences({ inactividad_minutos: 15, opciones_inactividad: [15] }), null);
  assert.equal(parseRevokedCount({ sesiones_revocadas: 2 }), 2);
  assert.equal(parseRevokedCount({ sesiones_revocadas: -1 }), null);
  assert.equal(parseRevokedCount({}), null);
});

test("inactivity options and labels", () => {
  assert.deepEqual(INACTIVITY_OPTIONS, [5, 15, 30, 60]);
  assert.equal(DEFAULT_INACTIVITY_MINUTES, 30);
  assert.deepEqual(INACTIVITY_OPTIONS.map(inactivityLabel), ["5 min", "15 min", "30 min", "1 hour"]);
});

test("device kinds for the icons", () => {
  assert.equal(deviceKind("Safari on iPhone"), "phone");
  assert.equal(deviceKind("Chrome on Android"), "phone");
  assert.equal(deviceKind("Chrome on iPad"), "tablet");
  assert.equal(deviceKind("Edge on Windows"), "computer");
  assert.equal(deviceKind("Headless Chrome on Linux"), "computer");
  assert.equal(deviceKind("curl"), "unknown");
});

function event(kind: string, extra: Partial<SecurityEvent> = {}): SecurityEvent {
  return {
    id: 1,
    kind,
    at: "2026-11-18T10:00:00Z",
    ip: null,
    device: null,
    detail: null,
    fromThisSession: false,
    ...extra,
  };
}

test("log entries in words", () => {
  assert.deepEqual(describeEvent(event("INICIO_SESION", { device: "Chrome on macOS" })), {
    title: "Signed in",
    detail: "Chrome on macOS",
    tone: "neutral",
  });
  assert.equal(describeEvent(event("SESION_REVOCADA", { device: "Safari on iPhone" })).detail, "Safari on iPhone");
  assert.equal(
    describeEvent(event("SESIONES_REVOCADAS", { device: "Chrome on macOS", detail: 2 })).title,
    "Signed out all other devices",
  );
  // No session and no device behind it: the bank signed every device out.
  assert.deepEqual(describeEvent(event("SESIONES_REVOCADAS", { detail: 1 })), {
    title: "Signed out on every device",
    detail: "1 session ended",
    tone: "accent",
  });
  assert.equal(describeEvent(event("CONTRASENA_CAMBIADA", { detail: 2 })).detail, "2 other devices signed out");
  assert.equal(describeEvent(event("CONTRASENA_CAMBIADA", { detail: 0 })).detail, "No other device was signed in");
  assert.equal(describeEvent(event("PREFERENCIAS_ACTUALIZADAS", { detail: 60 })).detail, "Now 1 hour");
  assert.equal(describeEvent(event("CUENTA_BLOQUEADA")).tone, "danger");
  assert.equal(describeEvent(event("INICIO_FALLIDO")).tone, "warning");
  assert.equal(describeEvent(event("ALGO_NUEVO")).title, "Security event");
});

test("no log entry uses words that would tip off a client under review", () => {
  const TIP_OFF =
    /\b(compliance hold|compliance review|frozen|freeze|restrict\w*|AML|PLD|suspicious|alert|congel\w*|preventiv\w*|UIF|investigat\w*|risk)\b/i;
  const kinds = [
    "INICIO_SESION",
    "INICIO_FALLIDO",
    "CUENTA_BLOQUEADA",
    "SESION_CERRADA",
    "SESION_REVOCADA",
    "SESIONES_REVOCADAS",
    "CONTRASENA_CAMBIADA",
    "CAMBIO_CONTRASENA_RECHAZADO",
    "RESTABLECIMIENTO_SOLICITADO",
    "CONTRASENA_RESTABLECIDA",
    "PREFERENCIAS_ACTUALIZADAS",
    "MFA_SECRETO_GENERADO",
    "MFA_ACTIVADO",
    "MFA_VERIFICADO",
    "MFA_CODIGO_RECHAZADO",
    "MFA_DESACTIVADO",
  ];
  for (const kind of kinds) {
    const { title, detail } = describeEvent(event(kind, { detail: 2 }));
    assert.doesNotMatch(`${title} ${detail ?? ""}`, TIP_OFF, kind);
    assert.notEqual(title, "Security event", `${kind} has its own words`);
  }
});

test("relative and absolute times", () => {
  const now = Date.parse("2026-11-18T12:00:00Z");
  assert.equal(timeAgo("2026-11-18T11:59:30Z", now), "just now");
  assert.equal(timeAgo("2026-11-18T11:55:00Z", now), "5 min ago");
  assert.equal(timeAgo("2026-11-18T09:00:00Z", now), "3 h ago");
  assert.equal(timeAgo("2026-11-17T12:00:00Z", now), "1 day ago");
  assert.equal(timeAgo("2026-11-15T12:00:00Z", now), "3 days ago");
  assert.equal(timeAgo("2026-11-18T12:00:05Z", now), "just now", "a clock slightly ahead is not the future");
  assert.equal(formatWhen("2026-11-18T14:05:00Z", "UTC"), "Nov 18, 14:05");
});

test("password rules and the change request", () => {
  assert.deepEqual(
    passwordRules("old-password-123", "short", "short").map((r) => r.met),
    [false, true, true],
  );
  assert.deepEqual(
    passwordRules("same-password-123", "same-password-123", "same-password-123").map((r) => r.met),
    [true, false, true],
  );
  const form = { current: "old-password-123", next: "new-password-4567", confirmation: "new-password-4567", code: "" };
  assert.deepEqual(passwordChangeRequest(form, false), {
    request: { current: "old-password-123", next: "new-password-4567" },
  });
  assert.deepEqual(passwordChangeRequest(form, true), {
    problem: "Enter the 6-digit code from your authenticator app.",
  });
  assert.deepEqual(passwordChangeRequest({ ...form, code: "123456" }, true), {
    request: { current: "old-password-123", next: "new-password-4567", code: "123456" },
  });
  assert.deepEqual(passwordChangeRequest({ ...form, current: "" }, false), { problem: "Enter your current password." });
  assert.deepEqual(passwordChangeRequest({ ...form, confirmation: "other-password-890" }, false), {
    problem: "New password: both new passwords match.",
  });
  assert.deepEqual(passwordChangeBody({ current: "a", next: "b" }), { password_actual: "a", password_nueva: "b" });
  assert.deepEqual(passwordChangeBody({ current: "a", next: "b", code: "123456" }), {
    password_actual: "a",
    password_nueva: "b",
    codigo: "123456",
  });
});

test("idle after the chosen minutes without activity", () => {
  const start = Date.parse("2026-11-18T12:00:00Z");
  assert.equal(isIdle(start, 15, start + 14 * 60_000), false);
  assert.equal(isIdle(start, 15, start + 15 * 60_000), true);
  assert.equal(isIdle(start, 5, start + 6 * 60_000), true);
});

test("passkey readiness in words, status only", () => {
  assert.match(passkeyStatus("ready").detail, /coming soon/);
  assert.equal(passkeyStatus("unsupported").label, "Not supported here");
  assert.equal(passkeyStatus("no-authenticator").label, "No built-in authenticator");
});
