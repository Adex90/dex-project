/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  ACCOUNT_LOCKED_MESSAGE,
  SESSION_LOCKED_MESSAGE,
  StepUpCancelled,
  attemptsLeftLabel,
  credentialReady,
  designationCheck,
  isStepUpRequired,
  isVerified,
  parseStepUpStatus,
  parseStepUpWindow,
  stepUpBody,
  stepUpFeedback,
  windowLabel,
  withdrawalCheck,
} from "./step-up.ts";

const UNTIL = "2026-11-24T18:05:00.123456Z";

test("a 403 STEP_UP_REQUERIDO asks for the check; nothing else does", () => {
  assert.equal(isStepUpRequired({ status: 403, code: "STEP_UP_REQUERIDO" }), true);
  assert.equal(isStepUpRequired({ status: 403, code: "PROHIBIDO" }), false);
  assert.equal(isStepUpRequired({ status: 422, code: "STEP_UP_REQUERIDO" }), false);
  assert.equal(isStepUpRequired(new Error("STEP_UP_REQUERIDO")), false);
  assert.equal(isStepUpRequired(null), false);
  assert.equal(isStepUpRequired("STEP_UP_REQUERIDO"), false);
});

test("the status: the method and the open window, or none", () => {
  assert.deepEqual(
    parseStepUpStatus({
      metodo: "CONTRASENA",
      valido_hasta: null,
      segundos_restantes: 0,
      vigencia_segundos: 300,
    }),
    {
      method: "password",
      validUntil: null,
      remainingSeconds: 0,
      windowSeconds: 300,
    },
  );
  assert.deepEqual(
    parseStepUpStatus({
      metodo: "TOTP",
      valido_hasta: UNTIL,
      segundos_restantes: 287,
      vigencia_segundos: 300,
    }),
    {
      method: "totp",
      validUntil: UNTIL,
      remainingSeconds: 287,
      windowSeconds: 300,
    },
  );
});

test("a malformed status is rejected whole", () => {
  const good = {
    metodo: "TOTP",
    valido_hasta: UNTIL,
    segundos_restantes: 287,
    vigencia_segundos: 300,
  };
  for (const bad of [
    null,
    [],
    "TOTP",
    { ...good, metodo: "SMS" },
    { ...good, metodo: "totp" },
    { ...good, metodo: "constructor" },
    { ...good, valido_hasta: "tomorrow" },
    { ...good, valido_hasta: 1764007500 },
    { ...good, segundos_restantes: -1 },
    { ...good, segundos_restantes: 1.5 },
    { ...good, segundos_restantes: 301 },
    { ...good, segundos_restantes: "287" },
    { ...good, vigencia_segundos: 0 },
    { ...good, vigencia_segundos: 10_000_000 },
    // Without a window there is nothing left of it.
    { ...good, valido_hasta: null },
  ]) {
    assert.equal(parseStepUpStatus(bad), null, JSON.stringify(bad));
  }
});

test("the window just opened", () => {
  assert.deepEqual(parseStepUpWindow({ metodo: "TOTP", valido_hasta: UNTIL, segundos: 300 }), {
    method: "totp",
    validUntil: UNTIL,
    seconds: 300,
  });
  assert.equal(parseStepUpWindow({ metodo: "TOTP", valido_hasta: null, segundos: 300 }), null);
  assert.equal(parseStepUpWindow({ metodo: "PIN", valido_hasta: UNTIL, segundos: 300 }), null);
  assert.equal(parseStepUpWindow({ metodo: "TOTP", valido_hasta: UNTIL, segundos: -5 }), null);
});

test("an open window with a few seconds left still asks again", () => {
  const status = {
    method: "totp" as const,
    validUntil: UNTIL,
    remainingSeconds: 120,
    windowSeconds: 300,
  };
  assert.equal(isVerified(status), true);
  assert.equal(isVerified({ ...status, remainingSeconds: 9 }), false);
  assert.equal(isVerified({ ...status, validUntil: null, remainingSeconds: 0 }), false);
});

test("the body: six digits, or the password exactly as typed", () => {
  assert.deepEqual(stepUpBody({ method: "totp", code: " 123 456 " }), {
    codigo: "123456",
  });
  assert.deepEqual(stepUpBody({ method: "totp", code: "123-456" }), {
    codigo: "123456",
  });
  assert.deepEqual(stepUpBody({ method: "password", password: " spaced pass " }), { password: " spaced pass " });
  assert.equal(credentialReady({ method: "totp", code: "123 45" }), false);
  assert.equal(credentialReady({ method: "totp", code: "123 456" }), true);
  assert.equal(credentialReady({ method: "password", password: "" }), false);
  assert.equal(credentialReady({ method: "password", password: "x" }), true);
});

test("copy for the window and the attempts left", () => {
  assert.equal(windowLabel(300), "5 minutes");
  assert.equal(windowLabel(61), "1 minute");
  assert.equal(windowLabel(20), "1 minute");
  assert.equal(attemptsLeftLabel(4), "4 attempts left before your account locks for 15 minutes.");
  assert.equal(attemptsLeftLabel(1), "Last attempt before your account locks for 15 minutes.");
});

test("feedback: attempts left, the lockout that ends this session, one from elsewhere", () => {
  const describe = (e: unknown) => `described ${(e as { code: string }).code}`;
  assert.deepEqual(stepUpFeedback({ status: 422, code: "CODIGO_MFA_INVALIDO", remainingAttempts: 3 }, describe), {
    message: "described CODIGO_MFA_INVALIDO",
    remainingAttempts: 3,
    locked: false,
    sessionEnded: false,
  });
  assert.deepEqual(stepUpFeedback({ status: 423, code: "SESION_BLOQUEADA" }, describe), {
    message: SESSION_LOCKED_MESSAGE,
    remainingAttempts: 0,
    locked: true,
    sessionEnded: true,
  });
  assert.deepEqual(stepUpFeedback({ status: 423, code: "BLOQUEADO" }, describe), {
    message: ACCOUNT_LOCKED_MESSAGE,
    remainingAttempts: null,
    locked: true,
    sessionEnded: false,
  });
  // A 401 here means the session itself is gone.
  assert.equal(stepUpFeedback({ status: 401, code: "NO_AUTENTICADO" }, describe).sessionEnded, true);
  // A count the server did not send, or nonsense, is not shown.
  assert.equal(stepUpFeedback({ status: 400, code: "VALIDACION" }, describe).remainingAttempts, null);
  assert.equal(stepUpFeedback({ status: 422, code: "X", remainingAttempts: "3" }, describe).remainingAttempts, null);
});

test("closing the check is an error the action shows as is", () => {
  const cancelled = new StepUpCancelled("Nothing was sent.");
  assert.equal(cancelled instanceof Error, true);
  assert.equal(cancelled.name, "StepUpCancelled");
  assert.equal(cancelled.message, "Nothing was sent.");
});

test("what the check says is waiting: a withdrawal", () => {
  assert.deepEqual(withdrawalCheck("1250.50", "BBVA ••0019"), {
    action: "Withdraw $1,250.50",
    detail: "To BBVA ••0019, by SPEI",
    cancelled: "Not confirmed: nothing was sent.",
  });
});

test("what the check says is waiting: the beneficiaries, with their shares", () => {
  assert.deepEqual(
    designationCheck([
      { fullName: "  María   López Rojas ", percent: "60.00" },
      { fullName: "José López Rojas", percent: "40" },
    ]),
    {
      action: "Save 2 beneficiaries",
      detail: "María López Rojas 60% · José López Rojas 40%",
      cancelled: "Not confirmed: nothing was saved.",
    },
  );
  assert.equal(designationCheck([{ fullName: "Ana", percent: "33.30" }]).detail, "Ana 33.3%");
  assert.equal(designationCheck([{ fullName: "Ana", percent: "100" }]).action, "Save 1 beneficiary");
  assert.deepEqual(designationCheck([]), {
    action: "Remove every beneficiary",
    detail: "Saves a version with no one named",
    cancelled: "Not confirmed: nothing was saved.",
  });
});
