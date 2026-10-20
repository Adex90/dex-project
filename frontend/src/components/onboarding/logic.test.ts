/**
 * Onboarding wizard logic: resume step, step routing, form mapping, credential
 * checks against the CURP of step 1, image sizing and error copy.
 *
 * Run with: npm test   (Node >= 22.18 strips TypeScript types natively)
 */
import assert from "node:assert/strict";
import { test } from "node:test";

import type { ExpedienteKyc, VistaExpediente } from "../../lib/api/types.ts";
import { KYC_MESSAGES, buildClaveElector, parseCurp, sampleIdentity } from "../../lib/kyc.ts";
import {
  CHALLENGES,
  EMPTY_INE,
  IDENTITY_IN_USE,
  identityKey,
  PROFILE_FIELDS,
  PROFILE_INPUT_IDS,
  canRetry,
  canVisitStep,
  checkCredentialDraft,
  checkIneImages,
  describeCameraProblem,
  duplicateIndexes,
  effectiveStep,
  expectedClaveDate,
  fitWithin,
  formatPercent,
  ineDraftFromFile,
  isProfileSaved,
  mergeSavedProfile,
  pickChallenge,
  profileFieldForMessage,
  profileFormFromFile,
  profileInputFromForm,
  profileProblem,
  resumeStep,
  splitDetails,
  statusHeadline,
  verificationProblem,
  type IneDraft,
} from "./logic.ts";

const TODAY = "2026-10-14";
const CURP = "LOOA531113HTCPBN07";

function curp(value = CURP) {
  const parsed = parseCurp(value);
  assert.ok(parsed.ok);
  return parsed.value;
}

function expediente(overrides: Partial<ExpedienteKyc> = {}): ExpedienteKyc {
  return {
    cliente_id: "00000000-0000-4000-8000-000000000001",
    nombres: "Andrés Manuel",
    apellido_paterno: "López",
    apellido_materno: "Obrador",
    apellidos: "López Obrador",
    curp: CURP,
    rfc: "LOOA531113AB1",
    fecha_nacimiento: "1953-11-13",
    sexo: "H",
    entidad_nacimiento: "TC",
    nacionalidad: "MEX",
    domicilio: {
      calle: "Av. Paseo Tabasco",
      numero_exterior: "1401",
      numero_interior: null,
      colonia: "Tabasco 2000",
      municipio: "Centro",
      estado: "Tabasco",
      codigo_postal: "86035",
    },
    ine_clave_elector: null,
    ine_cic: null,
    ine_identificador_ciudadano: null,
    ine_emision: null,
    ine_vigencia: null,
    estado_kyc: "PENDIENTE",
    intentos: 0,
    verificacion_en_curso: null,
    enviado_en: null,
    metadata_proveedor: {},
    motivo_rechazo: null,
    verificado_en: null,
    revisado_por: null,
    creado_en: "2026-10-14T10:00:00Z",
    actualizado_en: "2026-10-14T10:00:00Z",
    ...overrides,
  };
}

function vista(overrides: Partial<VistaExpediente> = {}): VistaExpediente {
  return {
    ...expediente(),
    tipo_persona: "FISICA",
    estatus_cliente: "PENDIENTE",
    max_intentos: 3,
    intentos_restantes: 3,
    bloqueo: null,
    ultimo_resultado: null,
    ...overrides,
  };
}

test("resume: step 1 until the profile (with address) is saved, then step 2; status page otherwise", () => {
  // Fresh registration: names (and maybe CURP/RFC) but no address yet.
  assert.equal(resumeStep(expediente({ domicilio: {} })), 1);
  assert.equal(resumeStep(expediente({ curp: null, rfc: null, domicilio: {} })), 1);
  // A stored CURP that does not parse is not a saved profile.
  assert.equal(resumeStep(expediente({ curp: "LOOA531113HTCPBN08" })), 1);
  assert.equal(resumeStep(expediente()), 2);
  for (const estado of ["EN_REVISION", "VERIFICADO", "RECHAZADO"] as const) {
    assert.equal(resumeStep(expediente({ estado_kyc: estado, domicilio: {} })), 4);
  }
  assert.equal(isProfileSaved(expediente({ apellido_paterno: null })), false);
});

test("routing: steps 2-4 need what comes before them; a closed file only has the status page", () => {
  const base = { estado_kyc: "PENDIENTE" as const, profileSaved: true, ineReady: true, hasAttempt: false };
  assert.equal(effectiveStep(3, base), 3);
  assert.equal(effectiveStep(3, { ...base, ineReady: false }), 2);
  assert.equal(effectiveStep(2, { ...base, profileSaved: false }), 1);
  assert.equal(effectiveStep(3, { ...base, profileSaved: false }), 1);
  assert.equal(effectiveStep(4, { ...base, profileSaved: false }), 4);
  assert.equal(effectiveStep(1, { ...base, estado_kyc: "EN_REVISION" }), 4);
  assert.equal(effectiveStep(2, { ...base, estado_kyc: "VERIFICADO" }), 4);

  assert.equal(canVisitStep(1, base), true);
  assert.equal(canVisitStep(3, { ...base, ineReady: false }), false);
  assert.equal(canVisitStep(4, base), false);
  assert.equal(canVisitStep(4, { ...base, hasAttempt: true }), true);
  assert.equal(canVisitStep(1, { ...base, estado_kyc: "RECHAZADO" }), false);
});

test("retry and headline follow the attempts left", () => {
  assert.equal(canRetry(vista()), false); // never submitted
  assert.equal(canRetry(vista({ intentos: 1, intentos_restantes: 2 })), true);
  assert.equal(canRetry(vista({ intentos: 3, intentos_restantes: 0 })), false);
  assert.equal(canRetry(vista({ estado_kyc: "EN_REVISION", intentos: 1, intentos_restantes: 2 })), false);
  assert.match(statusHeadline(vista({ intentos: 2, intentos_restantes: 1 })).body, /1 attempt left/);
  assert.match(statusHeadline(vista({ estado_kyc: "VERIFICADO" })).title, /verified/);
  // 202: the headline must not claim a human review while the provider has not answered.
  assert.doesNotMatch(statusHeadline(vista({ estado_kyc: "EN_REVISION" }), { awaitingProvider: true }).title, /analyst/);
  assert.match(statusHeadline(vista({ estado_kyc: "EN_REVISION" })).title, /analyst/);
  assert.match(statusHeadline(vista({ estado_kyc: "RECHAZADO" }), { suspended: true }).title, /suspended/);
});

test("a submission lock blocks the retry and replaces the attempts headline", () => {
  // The file is PENDIENTE with attempts on paper, but the server will refuse the submission.
  const locked = vista({ intentos: 1, intentos_restantes: 0, bloqueo: "CONFLICTOS_IDENTIDAD" });
  assert.equal(canRetry(locked), false);
  assert.equal(canRetry({ ...locked, intentos_restantes: 2 }), false);
  assert.match(statusHeadline(locked).title, /locked/);
  assert.doesNotMatch(statusHeadline(locked).body, /attempts? left/);
  assert.match(statusHeadline(vista({ bloqueo: "IDENTIDAD_BLOQUEADA", intentos_restantes: 0 })).title, /CURP/);
  assert.match(statusHeadline(vista({ bloqueo: "SIN_INTENTOS", intentos_restantes: 0 })).title, /No verification attempts/);
});

test("saving step 1 keeps the view-only fields and recomputes the attempts left", () => {
  const before = vista({ intentos: 1, intentos_restantes: 2, ultimo_resultado: null });
  const merged = mergeSavedProfile(before, expediente({ intentos: 1, nombres: "Andrés" }));
  assert.equal(merged.nombres, "Andrés");
  assert.equal(merged.tipo_persona, "FISICA");
  assert.equal(merged.max_intentos, 3);
  assert.equal(merged.intentos_restantes, 2);
});

test("saving step 1 keeps a lock, except a CURP lock when the CURP changed", () => {
  const conflicts = vista({ intentos: 1, intentos_restantes: 0, bloqueo: "CONFLICTOS_IDENTIDAD" });
  const kept = mergeSavedProfile(conflicts, expediente({ intentos: 1, curp: "GODE561231HDFRRN04" }));
  assert.equal(kept.bloqueo, "CONFLICTOS_IDENTIDAD");
  assert.equal(kept.intentos_restantes, 0);
  const curpLock = vista({ intentos: 1, intentos_restantes: 0, bloqueo: "IDENTIDAD_BLOQUEADA" });
  assert.equal(mergeSavedProfile(curpLock, expediente({ intentos: 1 })).bloqueo, "IDENTIDAD_BLOQUEADA");
  const lifted = mergeSavedProfile(curpLock, expediente({ intentos: 1, curp: "GODE561231HDFRRN04" }));
  assert.equal(lifted.bloqueo, null);
  assert.equal(lifted.intentos_restantes, 2);
});

test("profile form round-trips to the API body; empty optional fields become null", () => {
  const form = profileFormFromFile(expediente());
  assert.equal(form.codigo_postal, "86035");
  assert.equal(form.numero_interior, "");
  const input = profileInputFromForm({ ...form, apellido_materno: "" });
  assert.equal(input.apellido_materno, null);
  assert.equal(input.domicilio.numero_interior, null);
  assert.deepEqual(profileFormFromFile(null).curp, "");
  // Every field has a stable, unique DOM id.
  assert.equal(new Set(PROFILE_FIELDS.map((f) => PROFILE_INPUT_IDS[f])).size, PROFILE_FIELDS.length);
  assert.equal(PROFILE_INPUT_IDS.codigo_postal, "codigo-postal");
});

test("server messages of POST /kyc/perfil land on the right field", () => {
  assert.equal(profileFieldForMessage("el dígito verificador de la CURP no corresponde"), "curp");
  assert.equal(profileFieldForMessage("la entidad de nacimiento ZZ no existe"), "curp");
  assert.equal(profileFieldForMessage("la fecha de nacimiento del RFC no coincide con la de la CURP"), "rfc");
  assert.equal(profileFieldForMessage("el código postal tiene 5 dígitos"), "codigo_postal");
  assert.equal(profileFieldForMessage("apellido_paterno es obligatorio"), "apellido_paterno");
  assert.equal(profileFieldForMessage("numero_exterior admite como máximo 20 caracteres"), "numero_exterior");
  assert.equal(profileFieldForMessage("algo inesperado"), null);

  const rfc = profileProblem("VALIDACION", "el dígito verificador del RFC no corresponde");
  assert.deepEqual(rfc, { message: KYC_MESSAGES.rfcCheckDigit, field: "rfc", reload: false });
  assert.equal(profileProblem("KYC_MENOR_DE_EDAD", "la apertura remota es solo para mayores de edad")?.field, "curp");
  assert.equal(profileProblem("KYC_EXPEDIENTE_NO_EDITABLE", "el expediente ya está verificado")?.reload, true);
  assert.equal(profileProblem("IDENTIDAD_EN_USO", "x")?.message, IDENTITY_IN_USE);
  assert.equal(profileProblem("RED", "x"), null);
});

function credentialFor(seed: number): { draft: IneDraft; curp: string } {
  const id = sampleIdentity(seed, TODAY);
  return {
    curp: id.curp,
    draft: {
      ...EMPTY_INE,
      clave_elector: id.credential.clave_elector,
      cic: id.credential.cic,
      identificador_ciudadano: id.credential.identificador_ciudadano,
      emision: String(id.credential.emision),
      vigencia: String(id.credential.vigencia),
    },
  };
}

test("credential draft: a consistent credential passes and is normalized", () => {
  const { draft, curp: c } = credentialFor(4242);
  const check = checkCredentialDraft({ ...draft, clave_elector: ` ${draft.clave_elector.toLowerCase()} ` }, curp(c), TODAY);
  assert.deepEqual(check.fieldErrors, {});
  assert.ok(check.input);
  assert.equal(check.input.clave_elector, draft.clave_elector);
  assert.equal(typeof check.input.emision, "number");
  // The citizen identifier is optional.
  const noId = checkCredentialDraft({ ...draft, identificador_ciudadano: "" }, curp(c), TODAY);
  assert.equal(noId.input?.identificador_ciudadano, null);
});

test("credential draft: empty, partial and inconsistent fields get specific messages", () => {
  const empty = checkCredentialDraft(EMPTY_INE, curp(), TODAY);
  assert.equal(empty.input, null);
  assert.match(empty.fieldErrors.clave_elector ?? "", /voter key/);
  assert.match(empty.fieldErrors.cic ?? "", /9-digit CIC/);
  assert.match(empty.fieldErrors.emision ?? "", /issue year/);
  assert.match(empty.fieldErrors.vigencia ?? "", /expiry year/);
  assert.equal(empty.fieldErrors.identificador_ciudadano, undefined);

  const { draft, curp: c } = credentialFor(77);
  const parsed = curp(c);
  assert.equal(checkCredentialDraft({ ...draft, emision: "20" }, parsed, TODAY).fieldErrors.emision, "Enter a 4-digit year.");
  assert.equal(checkCredentialDraft({ ...draft, cic: "12345" }, parsed, TODAY).fieldErrors.cic, KYC_MESSAGES.cic);
  assert.equal(
    checkCredentialDraft({ ...draft, identificador_ciudadano: "1234567890" }, parsed, TODAY).fieldErrors.identificador_ciudadano,
    KYC_MESSAGES.citizenId,
  );
  // Expired (valid through Dec 31 of its year) and over the 10-year span.
  assert.equal(
    checkCredentialDraft({ ...draft, emision: "2015", vigencia: "2025" }, parsed, TODAY).fieldErrors.vigencia,
    KYC_MESSAGES.expired(2025),
  );
  assert.equal(
    checkCredentialDraft({ ...draft, emision: "2020", vigencia: "2031" }, parsed, TODAY).fieldErrors.vigencia,
    KYC_MESSAGES.validitySpan,
  );
  assert.ok(checkCredentialDraft({ ...draft, emision: "2016", vigencia: "2026" }, parsed, TODAY).input);
  // A voter key of somebody else (other birth date) is refused against the CURP of step 1.
  const other = buildClaveElector({
    nombres: "Andrés Manuel",
    apellidoPaterno: "López",
    apellidoMaterno: "Obrador",
    fechaNacimiento: "1953-11-14",
    entidadInegi: 27,
    sexo: "H",
    homoclave: 1,
  });
  assert.equal(
    checkCredentialDraft({ ...draft, clave_elector: other }, curp(), TODAY).fieldErrors.clave_elector,
    KYC_MESSAGES.claveDateMismatch,
  );
  assert.equal(expectedClaveDate(curp()), "531113");
});

test("INE photos: both sides required and different", () => {
  assert.deepEqual(Object.keys(checkIneImages(EMPTY_INE)).sort(), ["frente", "reverso"]);
  assert.match(checkIneImages({ frente: "data:a", reverso: "data:a" }).reverso ?? "", /identical/);
  assert.deepEqual(checkIneImages({ frente: "data:a", reverso: "data:b" }), {});
});

test("prefill of step 2 after a rejected attempt keeps the typed credential, never photos", () => {
  const draft = ineDraftFromFile(
    expediente({ ine_clave_elector: "LPOBAN53111327H100", ine_cic: "123456789", ine_emision: 2020, ine_vigencia: 2030 }),
  );
  assert.equal(draft.frente, null);
  assert.equal(draft.emision, "2020");
  assert.equal(draft.identificador_ciudadano, "");
});

test("images are scaled down to the longest side, never up", () => {
  assert.deepEqual(fitWithin(4032, 3024, 1600), { width: 1600, height: 1200 });
  assert.deepEqual(fitWithin(3024, 4032, 1600), { width: 1200, height: 1600 });
  assert.deepEqual(fitWithin(1280, 720, 720), { width: 720, height: 405 });
  assert.deepEqual(fitWithin(640, 480, 720), { width: 640, height: 480 });
  assert.throws(() => fitWithin(0, 480, 720), RangeError);
  assert.deepEqual(duplicateIndexes(["a", "b", "a", "c", "b"]), [2, 4]);
});

test("liveness challenges are picked uniformly and always valid", () => {
  assert.equal(pickChallenge(0), CHALLENGES[0]);
  assert.equal(pickChallenge(0.999999), CHALLENGES[CHALLENGES.length - 1]);
  assert.equal(pickChallenge(1), CHALLENGES[CHALLENGES.length - 1]);
  assert.equal(pickChallenge(-1), CHALLENGES[0]);
  assert.equal(new Set(CHALLENGES.map((c) => c.code)).size, CHALLENGES.length);
});

test("camera problems explain what to do; only the INE step offers an upload", () => {
  assert.match(describeCameraProblem("NotAllowedError", "id").message, /upload/);
  assert.doesNotMatch(describeCameraProblem("NotAllowedError", "selfie").message, /upload/);
  assert.match(describeCameraProblem("NotFoundError", "selfie").message, /phone/);
  assert.match(describeCameraProblem("insecure", "selfie").message, /HTTPS/);
  assert.equal(describeCameraProblem("Whatever", "selfie").title, "The camera could not start");
});

test("errors of POST /kyc/verificar map to the next action", () => {
  assert.equal(verificationProblem("VALIDACION", "la credencial venció en 2025")?.action, "id");
  assert.equal(verificationProblem("VALIDACION", "la credencial venció en 2025")?.message, KYC_MESSAGES.expired("2025"));
  assert.equal(
    verificationProblem("VALIDACION", "la prueba de vida requiere de 3 a 8 cuadros de la cámara")?.action,
    "selfie",
  );
  assert.equal(verificationProblem("VALIDACION", "ine.frente: la imagen debe pesar entre 1 KB y 3 MB")?.action, "id");
  assert.equal(verificationProblem("KYC_SIN_CONSENTIMIENTO", "x")?.action, "consent");
  assert.equal(verificationProblem("KYC_DATOS_INCOMPLETOS", "x")?.action, "profile");
  assert.equal(verificationProblem("KYC_SIN_INTENTOS", "x")?.action, "status");
  // The conflict lockout and the identity lock are terminal: shown on the status page, no retry.
  const locked = verificationProblem("KYC_SIN_INTENTOS", "demasiados envíos con identidades registradas en otra cuenta");
  assert.equal(locked?.lock, true);
  assert.match(locked?.message ?? "", /locked for up to 30 days/);
  const blocked = verificationProblem(
    "KYC_IDENTIDAD_BLOQUEADA",
    "esa CURP acumula verificaciones rechazadas en otras cuentas; acude a cumplimiento",
  );
  assert.equal(blocked?.action, "status");
  assert.equal(blocked?.lock, true);
  assert.match(blocked?.message ?? "", /compliance team at /);
  assert.equal(verificationProblem("KYC_EXPEDIENTE_NO_EDITABLE", "el expediente ya está en revisión; espera el resultado")?.action, "status");
  assert.equal(verificationProblem("IDENTIDAD_EN_USO", "x")?.action, "identity");
  assert.match(verificationProblem("IDENTIDAD_EN_USO", "x")?.message ?? "", /30 days/);
  assert.equal(identityKey("A", "B", " abc "), identityKey("A", "B", "ABC"));
  assert.notEqual(identityKey("A", "B", "ABC"), identityKey("A", "C", "ABC"));
  assert.equal(verificationProblem("CUERPO_DEMASIADO_GRANDE", "x")?.message, KYC_MESSAGES.bodyTooLarge);
  assert.equal(verificationProblem("RED", "x"), null);
});

test("status page formatting", () => {
  assert.equal(formatPercent("0.9300"), "93.0%");
  assert.equal(formatPercent("0.85", 0), "85%");
  assert.equal(formatPercent(null), null);
  assert.equal(formatPercent("abc"), null);
  const split = splitDetails([
    { tipo: "ERROR", mensaje: "credencial no válida: anverso y reverso son la misma imagen" },
    { tipo: "ADVERTENCIA", mensaje: "texto libre de un operador" },
  ]);
  assert.deepEqual(split.errors, ["The credential is not valid: front and back are the same image."]);
  assert.deepEqual(split.warnings, ["texto libre de un operador"]);
});
