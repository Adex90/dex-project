/**
 * Step-up authentication (migration 0022, roadmap item 22): withdrawals,
 * transfers and changes to the beneficiaries need, besides a valid session,
 * a credential confirmed in the last few minutes: the authenticator code with
 * two-step verification on, otherwise the account password.
 *
 * - The server answers such an action with 403 `STEP_UP_REQUERIDO` before
 *   touching anything. The page then asks for the credential
 *   (components/security/step-up-modal.tsx), `POST /auth/step-up` opens a
 *   5-minute window on this session, and the same request goes again, with
 *   the same idempotency key: it cannot run twice.
 * - Every try counts toward the sign-in lockout (5 wrong ones lock the
 *   account for 15 minutes). A refused try says how many are left; the one
 *   that locks the account also closes this session (`SESION_BLOQUEADA`).
 *
 * Pure functions with relative `.ts` imports, so `node --test` runs them.
 */

import { formatMoney } from "./decimal.ts";
import { isTotpCode, normalizeTotpInput } from "./mfa.ts";
import { isRecord, isText } from "./vault.ts";
import type { StepUpCredential, StepUpMethod, StepUpRequest, StepUpStatus, StepUpWindow } from "../types/step-up.ts";

/** The API's 403 for an action that needs a fresh check. */
export const STEP_UP_REQUIRED = "STEP_UP_REQUERIDO";
/** This try locked the account and the server closed the session. */
export const SESSION_LOCKED = "SESION_BLOQUEADA";
/** The account was already locked (by tries elsewhere): nothing was counted. */
export const ACCOUNT_LOCKED = "BLOQUEADO";

/** How long a window lasts until the status says otherwise (the server's `kyc.vigencia_step_up()`). */
export const DEFAULT_WINDOW_SECONDS = 300;
/** Minutes an account stays locked after five wrong tries (the backend's MINUTOS_BLOQUEO). */
export const LOCK_MINUTES = 15;
/** A window this close to its end is not worth resuming on: verify again. */
const MIN_USEFUL_SECONDS = 10;
const MAX_SECONDS = 86_400;

/** What the person sees after a lockout that ended this session. */
export const SESSION_LOCKED_MESSAGE = `Too many failed attempts. For your security, your account is locked for ${LOCK_MINUTES} minutes and this session has ended.`;
/** ...and after a lockout from somewhere else: this session stays, the check cannot run. */
export const ACCOUNT_LOCKED_MESSAGE = `Too many failed attempts. Your account is locked for ${LOCK_MINUTES} minutes; try again later. Nothing was counted.`;

/** Thrown to the action when the person closes the check: the action shows `message` and nothing happened. */
export class StepUpCancelled extends Error {
  constructor(message: string) {
    super(message);
    this.name = "StepUpCancelled";
  }
}

interface ErrorLike {
  status?: unknown;
  code?: unknown;
  remainingAttempts?: unknown;
}

function fields(error: unknown): ErrorLike {
  return typeof error === "object" && error !== null ? (error as ErrorLike) : {};
}

/** Does this error (an ApiError) ask for a fresh check first? */
export function isStepUpRequired(error: unknown): boolean {
  const { status, code } = fields(error);
  return status === 403 && code === STEP_UP_REQUIRED;
}

function methodOf(value: unknown): StepUpMethod | null {
  if (value === "TOTP") return "totp";
  if (value === "CONTRASENA") return "password";
  return null;
}

function isInstant(value: unknown): value is string {
  return isText(value, 64) && !Number.isNaN(Date.parse(value));
}

function isSeconds(value: unknown, max: number): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 && value <= max;
}

/** GET /api/v1/auth/step-up; null if malformed. */
export function parseStepUpStatus(body: unknown): StepUpStatus | null {
  if (!isRecord(body)) return null;
  const { metodo, valido_hasta, segundos_restantes, vigencia_segundos } = body;
  const method = methodOf(metodo);
  if (method === null || !isSeconds(vigencia_segundos, MAX_SECONDS) || vigencia_segundos === 0) return null;
  if (!isSeconds(segundos_restantes, vigencia_segundos)) return null;
  if (!(valido_hasta === null || isInstant(valido_hasta))) return null;
  // Without a window, nothing is left of it.
  if (valido_hasta === null && segundos_restantes !== 0) return null;
  return {
    method,
    validUntil: valido_hasta,
    remainingSeconds: segundos_restantes,
    windowSeconds: vigencia_segundos,
  };
}

/** POST /api/v1/auth/step-up; null if malformed (the window is open all the same: it answered 2xx). */
export function parseStepUpWindow(body: unknown): StepUpWindow | null {
  if (!isRecord(body)) return null;
  const { metodo, valido_hasta, segundos } = body;
  const method = methodOf(metodo);
  if (method === null || !isInstant(valido_hasta) || !isSeconds(segundos, MAX_SECONDS)) return null;
  return { method, validUntil: valido_hasta, seconds: segundos };
}

/** The session is already verified (another tab, a moment ago): the action can go on without asking. */
export function isVerified(status: StepUpStatus): boolean {
  return status.validUntil !== null && status.remainingSeconds >= MIN_USEFUL_SECONDS;
}

/** The request body: the code's six digits, or the password exactly as typed. */
export function stepUpBody(credential: StepUpCredential): { codigo: string } | { password: string } {
  return credential.method === "totp"
    ? { codigo: normalizeTotpInput(credential.code) }
    : { password: credential.password };
}

/** Complete enough to send: six digits, or a password. */
export function credentialReady(credential: StepUpCredential): boolean {
  return credential.method === "totp" ? isTotpCode(normalizeTotpInput(credential.code)) : credential.password !== "";
}

/** "5 minutes", "1 minute": a window's length in words. */
export function windowLabel(seconds: number): string {
  const minutes = Math.max(1, Math.round(seconds / 60));
  return `${minutes} ${minutes === 1 ? "minute" : "minutes"}`;
}

/** "3 attempts left before your account locks for 15 minutes." */
export function attemptsLeftLabel(remaining: number): string {
  if (remaining <= 1) return `Last attempt before your account locks for ${LOCK_MINUTES} minutes.`;
  return `${remaining} attempts left before your account locks for ${LOCK_MINUTES} minutes.`;
}

/** What the check shows after a refused try. */
export interface StepUpFeedback {
  message: string;
  /** Tries left before the lockout (the server counts them together with sign-ins). */
  remainingAttempts: number | null;
  /** The account is locked: no more tries from here. */
  locked: boolean;
  /** The session is over (this try locked the account, or it had already ended): sign in again. */
  sessionEnded: boolean;
}

/** The feedback for `error`; `describe` turns any other error into its English message. */
export function stepUpFeedback(error: unknown, describe: (error: unknown) => string): StepUpFeedback {
  const { status, code, remainingAttempts } = fields(error);
  if (code === SESSION_LOCKED) {
    return {
      message: SESSION_LOCKED_MESSAGE,
      remainingAttempts: 0,
      locked: true,
      sessionEnded: true,
    };
  }
  if (code === ACCOUNT_LOCKED) {
    return {
      message: ACCOUNT_LOCKED_MESSAGE,
      remainingAttempts: null,
      locked: true,
      sessionEnded: false,
    };
  }
  return {
    message: describe(error),
    remainingAttempts: isSeconds(remainingAttempts, 99) ? remainingAttempts : null,
    locked: false,
    sessionEnded: status === 401,
  };
}

/** The check before a withdrawal: "Withdraw $1,250.50", "To BBVA ••0019, by SPEI". */
export function withdrawalCheck(amount: string, account: string): StepUpRequest {
  return {
    action: `Withdraw ${formatMoney(amount)}`,
    detail: `To ${account}, by SPEI`,
    cancelled: "Not confirmed: nothing was sent.",
  };
}

/** "60.00" -> "60", "33.30" -> "33.3": a share as people say it. */
function shareLabel(percent: string): string {
  const p = percent.trim();
  return p.includes(".") ? p.replace(/\.?0+$/, "") : p;
}

/** The check before saving the beneficiaries: who, and each one's share. */
export function designationCheck(drafts: readonly { fullName: string; percent: string }[]): StepUpRequest {
  const cancelled = "Not confirmed: nothing was saved.";
  if (drafts.length === 0) {
    return {
      action: "Remove every beneficiary",
      detail: "Saves a version with no one named",
      cancelled,
    };
  }
  return {
    action: `Save ${drafts.length} ${drafts.length === 1 ? "beneficiary" : "beneficiaries"}`,
    detail: drafts.map((d) => `${d.fullName.trim().replace(/\s+/g, " ")} ${shareLabel(d.percent)}%`).join(" · "),
    cancelled,
  };
}
