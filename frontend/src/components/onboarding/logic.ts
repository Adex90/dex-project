/**
 * Pure helpers of the onboarding wizard (no React, no DOM), so `node --test`
 * can import them: where to resume, form <-> API mapping, image sizing, the
 * liveness challenges and the English copy for camera and server errors.
 *
 * The Mexican ID rules themselves live in src/lib/kyc.ts (ported from the
 * backend); this module only adapts them to the wizard's forms.
 */
import type {
  BloqueoKyc,
  DatosPersonalesInput,
  DetalleKyc,
  EstadoKyc,
  ExpedienteKyc,
  VistaExpediente,
} from "../../lib/api/types.ts";
import type { CredentialField, CredentialInput, ParsedCurp, ProfileField } from "../../lib/kyc.ts";
import {
  CONTACT_COMPLIANCE,
  KYC_MESSAGES,
  lockMessage,
  describeKycMessage,
  kycMessageInEnglish,
  parseCurp,
  trimWs,
  validateCredential,
} from "../../lib/kyc.ts";

// -----------------------------------------------------------------------------
// Steps
// -----------------------------------------------------------------------------

export type Step = 1 | 2 | 3 | 4;

export interface StepInfo {
  n: Step;
  /** Short label for the stepper. */
  label: string;
  /** Heading of the step. */
  title: string;
}

export const STEPS: readonly StepInfo[] = [
  { n: 1, label: "Personal data", title: "Personal data" },
  { n: 2, label: "ID (INE)", title: "Your voting credential (INE)" },
  { n: 3, label: "Liveness selfie", title: "Liveness selfie" },
  { n: 4, label: "Status", title: "Verification status" },
];

type ProfileColumns = Pick<ExpedienteKyc, "nombres" | "apellido_paterno" | "curp" | "rfc" | "domicilio">;

/**
 * Step 1 is saved: what POST /kyc/verificar requires before it answers
 * KYC_DATOS_INCOMPLETOS (names, a valid CURP, the RFC and the address).
 * Registration may have stored names, CURP or RFC, but never the address.
 */
export function isProfileSaved(file: ProfileColumns): boolean {
  return Boolean(
    file.nombres &&
      file.apellido_paterno &&
      file.curp &&
      parseCurp(file.curp).ok &&
      file.rfc &&
      file.domicilio?.codigo_postal,
  );
}

/** Where the wizard resumes: step 1 or 2 while PENDIENTE, the status page otherwise. */
export function resumeStep(file: ProfileColumns & Pick<ExpedienteKyc, "estado_kyc">): Step {
  if (file.estado_kyc !== "PENDIENTE") return 4;
  return isProfileSaved(file) ? 2 : 1;
}

/**
 * The step actually shown for the one requested: a file that is no longer
 * PENDIENTE only has the status page; steps 2-3 need step 1 saved; step 3
 * needs the INE photos and data of step 2.
 */
export function effectiveStep(
  requested: Step,
  file: { estado_kyc: EstadoKyc; profileSaved: boolean; ineReady: boolean },
): Step {
  if (file.estado_kyc !== "PENDIENTE") return 4;
  if ((requested === 2 || requested === 3) && !file.profileSaved) return 1;
  if (requested === 3 && !file.ineReady) return 2;
  return requested;
}

/** Steps the stepper lets the customer jump to (besides the current one). */
export function canVisitStep(
  step: Step,
  file: { estado_kyc: EstadoKyc; profileSaved: boolean; ineReady: boolean; hasAttempt: boolean },
): boolean {
  if (file.estado_kyc !== "PENDIENTE") return step === 4;
  switch (step) {
    case 1:
      return true;
    case 2:
      return file.profileSaved;
    case 3:
      return file.profileSaved && file.ineReady;
    case 4:
      return file.hasAttempt;
  }
}

type RetryView = Pick<VistaExpediente, "estado_kyc" | "intentos" | "intentos_restantes"> & {
  bloqueo?: BloqueoKyc | null;
};

/** A verification was submitted and rejected, and the customer may submit again. */
export function canRetry(file: RetryView): boolean {
  return file.estado_kyc === "PENDIENTE" && !file.bloqueo && file.intentos > 0 && file.intentos_restantes > 0;
}

/**
 * The view after POST /kyc/perfil: the saved file plus the view-only fields of
 * the previous one. A CURP lock belongs to the old CURP; the server re-checks
 * the new one on submission.
 */
export function mergeSavedProfile(view: VistaExpediente, saved: ExpedienteKyc): VistaExpediente {
  const bloqueo = view.bloqueo === "IDENTIDAD_BLOQUEADA" && saved.curp !== view.curp ? null : view.bloqueo;
  return {
    ...view,
    ...saved,
    bloqueo,
    intentos_restantes: bloqueo ? 0 : Math.max(0, view.max_intentos - saved.intentos),
  };
}


// -----------------------------------------------------------------------------
// Step 1 form
// -----------------------------------------------------------------------------

export type ProfileForm = Record<ProfileField, string>;

/** Display order of the step 1 fields (also the order of the error summary). */
export const PROFILE_FIELDS: readonly ProfileField[] = [
  "nombres",
  "apellido_paterno",
  "apellido_materno",
  "curp",
  "rfc",
  "calle",
  "numero_exterior",
  "numero_interior",
  "colonia",
  "municipio",
  "estado",
  "codigo_postal",
];

/** DOM id of each step 1 input (stable for e2e tests). */
export const PROFILE_INPUT_IDS: Record<ProfileField, string> = {
  nombres: "nombres",
  apellido_paterno: "apellido-paterno",
  apellido_materno: "apellido-materno",
  curp: "curp",
  rfc: "rfc",
  calle: "calle",
  numero_exterior: "numero-exterior",
  numero_interior: "numero-interior",
  colonia: "colonia",
  municipio: "municipio",
  estado: "estado",
  codigo_postal: "codigo-postal",
};

/** Prefill from the file (registration may already have stored names, CURP and RFC). */
export function profileFormFromFile(file: Partial<ExpedienteKyc> | null): ProfileForm {
  const d = file?.domicilio ?? {};
  return {
    nombres: file?.nombres ?? "",
    apellido_paterno: file?.apellido_paterno ?? "",
    apellido_materno: file?.apellido_materno ?? "",
    curp: file?.curp ?? "",
    rfc: file?.rfc ?? "",
    calle: d.calle ?? "",
    numero_exterior: d.numero_exterior ?? "",
    numero_interior: d.numero_interior ?? "",
    colonia: d.colonia ?? "",
    municipio: d.municipio ?? "",
    estado: d.estado ?? "",
    codigo_postal: d.codigo_postal ?? "",
  };
}

/** The form as the API's input shape (validated and normalized later by `checkProfile`). */
export function profileInputFromForm(form: ProfileForm): DatosPersonalesInput {
  return {
    nombres: form.nombres,
    apellido_paterno: form.apellido_paterno,
    apellido_materno: form.apellido_materno || null,
    curp: form.curp,
    rfc: form.rfc,
    domicilio: {
      calle: form.calle,
      numero_exterior: form.numero_exterior,
      numero_interior: form.numero_interior || null,
      colonia: form.colonia,
      municipio: form.municipio,
      estado: form.estado,
      codigo_postal: form.codigo_postal,
    },
  };
}

/**
 * Field a VALIDACION message of POST /kyc/perfil refers to (the backend
 * stops at the first error), so it can be shown next to the input.
 */
export function profileFieldForMessage(mensaje: string): ProfileField | null {
  const field = /^(nombres|apellido_paterno|apellido_materno|calle|numero_exterior|numero_interior|colonia|municipio|estado)\b/.exec(
    mensaje,
  );
  if (field) return field[1] as ProfileField;
  if (/código postal/i.test(mensaje)) return "codigo_postal";
  if (/\bRFC\b/.test(mensaje)) return "rfc";
  if (/\bCURP\b|entidad de nacimiento|mayores de edad/.test(mensaje)) return "curp";
  return null;
}

/** What to do with an error of POST /kyc/perfil. */
export interface ProfileProblem {
  message: string;
  /** Input the message belongs to. */
  field: ProfileField | null;
  /** The file is no longer editable: reload it (the status page explains why). */
  reload: boolean;
}

/** English copy for the error codes of POST /kyc/perfil; null for codes handled generically. */
export function profileProblem(code: string, mensaje: string): ProfileProblem | null {
  switch (code) {
    case "VALIDACION":
      return { message: kycMessageInEnglish(mensaje) ?? mensaje, field: profileFieldForMessage(mensaje), reload: false };
    case "KYC_MENOR_DE_EDAD":
      return {
        message: `${KYC_MESSAGES.underage} The birth date in your CURP shows you are under 18.`,
        field: "curp",
        reload: false,
      };
    case "KYC_EXPEDIENTE_NO_EDITABLE":
      return {
        message: `Your details can no longer be changed. ${kycMessageInEnglish(mensaje) ?? ""}`.trim(),
        field: null,
        reload: true,
      };
    case "KYC_SOLO_PERSONA_FISICA":
      return { message: MORAL_EXPLANATION, field: null, reload: true };
    case "IDENTIDAD_EN_USO":
      return { message: IDENTITY_IN_USE, field: null, reload: false };
    default:
      return null;
  }
}

export const MORAL_EXPLANATION = `Remote biometric verification is only for individuals (personas físicas). Companies (personas morales) are onboarded at a branch with their legal representative and incorporation documents. To arrange it, ${CONTACT_COMPLIANCE.charAt(0).toLowerCase()}${CONTACT_COMPLIANCE.slice(1)}`;

export const IDENTITY_IN_USE = `That CURP, RFC or voter key is already linked to another account that is verified or under review. If you think this is a mistake, ${CONTACT_COMPLIANCE.charAt(0).toLowerCase()}${CONTACT_COMPLIANCE.slice(1)}`;

/** After an IDENTIDAD_EN_USO on POST /kyc/verificar: every identical retry counts against the account. */
export const IDENTITY_CONFLICT_RETRY =
  "Correct your CURP, RFC or voter key before trying again: sending the same details again will be refused, and repeated conflicts lock remote verification for up to 30 days.";

/**
 * The identity a submission declares (CURP, RFC and voter key), to tell
 * whether the customer changed anything after an IDENTIDAD_EN_USO.
 */
export function identityKey(curp: string | null | undefined, rfc: string | null | undefined, claveElector: string): string {
  return [curp ?? "", rfc ?? "", trimWs(claveElector).toUpperCase()].join("|");
}

// -----------------------------------------------------------------------------
// Step 2: credential draft
// -----------------------------------------------------------------------------

/** Step 2 as typed and captured (years as text while typing). */
export interface IneDraft {
  /** JPEG data URLs, re-encoded in the browser. */
  frente: string | null;
  reverso: string | null;
  clave_elector: string;
  cic: string;
  identificador_ciudadano: string;
  emision: string;
  vigencia: string;
}

export const EMPTY_INE: IneDraft = {
  frente: null,
  reverso: null,
  clave_elector: "",
  cic: "",
  identificador_ciudadano: "",
  emision: "",
  vigencia: "",
};

/** Prefill after a rejected attempt: the backend keeps the typed credential (never the photos). */
export function ineDraftFromFile(file: Partial<ExpedienteKyc> | null): IneDraft {
  return {
    ...EMPTY_INE,
    clave_elector: file?.ine_clave_elector ?? "",
    cic: file?.ine_cic ?? "",
    identificador_ciudadano: file?.ine_identificador_ciudadano ?? "",
    emision: file?.ine_emision != null ? String(file.ine_emision) : "",
    vigencia: file?.ine_vigencia != null ? String(file.ine_vigencia) : "",
  };
}

export const CREDENTIAL_FIELDS: readonly CredentialField[] = [
  "clave_elector",
  "cic",
  "identificador_ciudadano",
  "emision",
  "vigencia",
];

/** DOM id of each step 2 input (stable for e2e tests). */
export const CREDENTIAL_INPUT_IDS: Record<CredentialField, string> = {
  clave_elector: "clave-elector",
  cic: "cic",
  identificador_ciudadano: "identificador",
  emision: "emision",
  vigencia: "vigencia",
};

export interface CredentialDraftCheck {
  fieldErrors: Partial<Record<CredentialField, string>>;
  /** Non-blocking (e.g. birth state in the voter key differs from the CURP's). */
  warnings: string[];
  /** The typed data for POST /kyc/verificar, when there are no errors. */
  input: CredentialInput | null;
}

const REQUIRED_CREDENTIAL: Record<"clave_elector" | "cic" | "emision" | "vigencia", string> = {
  clave_elector: "Enter your voter key (clave de elector).",
  cic: "Enter the 9-digit CIC from the back of your credential.",
  emision: "Enter the issue year (EMISIÓN).",
  vigencia: "Enter the expiry year (VIGENCIA).",
};

const YEAR = /^[0-9]{4}$/;

/**
 * Step 2 fields against the CURP saved in step 1 (`validateCredential`, the
 * backend's ine::validar_credencial), with friendlier messages for empty or
 * partial input.
 */
export function checkCredentialDraft(draft: IneDraft, curp: ParsedCurp, today: string): CredentialDraftCheck {
  const fieldErrors: CredentialDraftCheck["fieldErrors"] = {};
  const year = (value: string) => (YEAR.test(trimWs(value)) ? Number(trimWs(value)) : Number.NaN);
  const input: CredentialInput = {
    clave_elector: trimWs(draft.clave_elector).toUpperCase(),
    cic: trimWs(draft.cic),
    identificador_ciudadano: trimWs(draft.identificador_ciudadano) || null,
    emision: year(draft.emision),
    vigencia: year(draft.vigencia),
  };
  const check = validateCredential(input, curp, today);
  for (const field of CREDENTIAL_FIELDS) {
    const own = check.fieldErrors[field];
    if (own) fieldErrors[field] = own;
  }
  // Empty or partial years: the library would say "Issue year NaN is not valid".
  for (const field of ["emision", "vigencia"] as const) {
    const raw = trimWs(draft[field]);
    if (!raw) fieldErrors[field] = REQUIRED_CREDENTIAL[field];
    else if (!YEAR.test(raw)) fieldErrors[field] = "Enter a 4-digit year.";
  }
  for (const field of ["clave_elector", "cic"] as const) {
    if (!trimWs(draft[field])) fieldErrors[field] = REQUIRED_CREDENTIAL[field];
  }
  const ok = Object.keys(fieldErrors).length === 0;
  return { fieldErrors, warnings: check.warnings, input: ok ? input : null };
}

/** The voter key's birth date (characters 7-12) for a CURP: "531113". */
export function expectedClaveDate(curp: ParsedCurp): string {
  const f = curp.fechaNacimiento;
  return f.slice(2, 4) + f.slice(5, 7) + f.slice(8, 10);
}

/** Missing or repeated INE photos. */
export function checkIneImages(draft: Pick<IneDraft, "frente" | "reverso">): { frente?: string; reverso?: string } {
  const errors: { frente?: string; reverso?: string } = {};
  if (!draft.frente) errors.frente = "Add a photo of the front of your INE.";
  if (!draft.reverso) errors.reverso = "Add a photo of the back of your INE.";
  else if (draft.frente && draft.frente === draft.reverso) {
    errors.reverso = "The back photo is identical to the front one. Capture each side separately.";
  }
  return errors;
}

// -----------------------------------------------------------------------------
// Images and camera
// -----------------------------------------------------------------------------

/** Longest side of the re-encoded INE photos. */
export const ID_MAX_SIDE = 1600;
/** Longest side of the selfie frames. */
export const SELFIE_MAX_SIDE = 720;
export const JPEG_QUALITY = 0.85;
/** Selfie frames captured while the challenge is shown, and the pause between them. */
export const SELFIE_FRAMES = 4;
export const FRAME_INTERVAL_MS = 700;
/** Files larger than this are refused before decoding (a phone photo is 2-12 MB). */
export const MAX_UPLOAD_BYTES = 40 * 1024 * 1024;
export const ACCEPTED_IMAGE_TYPES = ["image/jpeg", "image/png", "image/webp"] as const;

/** Size that fits `width` x `height` within `maxSide` keeping the aspect ratio; never upscales. */
export function fitWithin(width: number, height: number, maxSide: number): { width: number; height: number } {
  if (!(width > 0 && height > 0 && maxSide > 0)) throw new RangeError("dimensions must be positive");
  const scale = Math.min(1, maxSide / Math.max(width, height));
  return { width: Math.max(1, Math.round(width * scale)), height: Math.max(1, Math.round(height * scale)) };
}

/** Indexes of entries equal to an earlier one (byte-identical frames must be recaptured). */
export function duplicateIndexes(values: readonly string[]): number[] {
  const seen = new Set<string>();
  const dup: number[] = [];
  values.forEach((v, i) => {
    if (seen.has(v)) dup.push(i);
    else seen.add(v);
  });
  return dup;
}

/**
 * Why the camera cannot be used: a DOMException name from getUserMedia
 * ("NotAllowedError", "NotFoundError"...) or one of our own codes:
 * "insecure", "unsupported", "ended" (track stopped), "no-frame",
 * "hidden" (released because the page was hidden).
 */
export type CameraProblem = string;

export interface CameraProblemCopy {
  title: string;
  message: string;
}

/**
 * Guidance for a camera failure. `purpose` "id" can fall back to uploading
 * a photo; the liveness selfie cannot (it must come from a live camera).
 */
export function describeCameraProblem(problem: CameraProblem, purpose: "id" | "selfie"): CameraProblemCopy {
  const fallback = purpose === "id" ? " You can upload photos of your INE instead." : "";
  switch (problem) {
    case "insecure":
      return {
        title: "The camera needs a secure connection",
        message:
          "Browsers only allow the camera on pages served over HTTPS (or on localhost). Open Synthetic Capital from its secure https:// address." +
          fallback,
      };
    case "unsupported":
      return {
        title: "This browser cannot use the camera",
        message: "Use an up-to-date version of Chrome, Safari, Edge or Firefox." + fallback,
      };
    case "NotAllowedError":
    case "SecurityError":
    case "PermissionDeniedError":
      return {
        title: "Camera access is blocked",
        message:
          "Allow camera access for this site: select the camera or lock icon in the address bar, choose Allow, then try again. On a phone, also check that your browser may use the camera in the system settings." +
          fallback,
      };
    case "NotFoundError":
    case "DevicesNotFoundError":
    case "OverconstrainedError":
      return {
        title: "No camera found",
        message:
          purpose === "id"
            ? "We could not find a camera on this device." + fallback
            : "We could not find a camera on this device. Continue on a phone or on a computer with a webcam.",
      };
    case "NotReadableError":
    case "TrackStartError":
    case "AbortError":
      return {
        title: "The camera is busy",
        message: "Another app or browser tab may be using the camera. Close it and try again." + fallback,
      };
    case "hidden":
      return {
        title: "The camera was turned off",
        message: "The camera turns off when you leave this page or switch apps, and nothing was sent. Try again." + fallback,
      };
    case "ended":
      return {
        title: "The camera stopped",
        message: "The camera was disconnected or its permission was revoked. Try again." + fallback,
      };
    case "no-frame":
      return {
        title: "The camera is not sending images",
        message: "We could not read an image from the camera. Make sure it is not covered and try again." + fallback,
      };
    default:
      return { title: "The camera could not start", message: "Try again, or reload the page." + fallback };
  }
}

// -----------------------------------------------------------------------------
// Step 3: liveness challenge
// -----------------------------------------------------------------------------

export interface Challenge {
  /** Sent as `selfie.reto` (informative for the provider and the audit trail). */
  code: string;
  /** Instruction shown to the customer. */
  text: string;
}

export const CHALLENGES: readonly Challenge[] = [
  { code: "PARPADEA_DOS_VECES", text: "Blink twice" },
  { code: "GIRA_IZQUIERDA", text: "Turn your head slightly left" },
  { code: "GIRA_DERECHA", text: "Turn your head slightly right" },
  { code: "SONRIE", text: "Smile" },
  { code: "ASIENTE", text: "Nod slowly" },
];

/** Challenge for a uniform random number in [0, 1). */
export function pickChallenge(random: number): Challenge {
  const i = Math.min(CHALLENGES.length - 1, Math.max(0, Math.floor(random * CHALLENGES.length)));
  return CHALLENGES[i] ?? { code: "SONRIE", text: "Smile" };
}

/**
 * What the liveness step should offer after an error of POST /kyc/verificar.
 * "identity": the declared identity is in use elsewhere, so edit the CURP,
 * RFC or voter key before retrying.
 */
export type VerificationAction = "status" | "profile" | "id" | "identity" | "selfie" | "consent" | null;

export interface VerificationProblem {
  message: string;
  action: VerificationAction;
  /** Remote verification cannot be retried now (no more submissions until compliance acts). */
  lock?: boolean;
}

/** English copy and next action for the error codes of POST /kyc/verificar; null for generic codes. */
export function verificationProblem(code: string, mensaje: string): VerificationProblem | null {
  switch (code) {
    case "VALIDACION": {
      const message = kycMessageInEnglish(mensaje) ?? mensaje;
      const selfie = /selfie|cuadros de la cámara|prueba de vida/.test(mensaje) && !/ine\.(frente|reverso)/.test(mensaje);
      return { message, action: selfie ? "selfie" : "id" };
    }
    case "KYC_SIN_CONSENTIMIENTO":
      return {
        message: "To verify your identity we need your explicit consent to process your biometric data.",
        action: "consent",
      };
    case "KYC_DATOS_INCOMPLETOS":
      return { message: "First save your personal details and address (step 1).", action: "profile" };
    case "KYC_SIN_INTENTOS":
    case "KYC_IDENTIDAD_BLOQUEADA":
      return {
        message: kycMessageInEnglish(mensaje) ?? `You cannot submit another verification right now. ${CONTACT_COMPLIANCE}`,
        action: "status",
        lock: true,
      };
    case "KYC_EXPEDIENTE_NO_EDITABLE":
      return { message: kycMessageInEnglish(mensaje) ?? "Your file can no longer be changed.", action: "status" };
    case "KYC_SOLO_PERSONA_FISICA":
      return { message: MORAL_EXPLANATION, action: "status" };
    case "IDENTIDAD_EN_USO":
      return { message: `${IDENTITY_IN_USE} ${IDENTITY_CONFLICT_RETRY}`, action: "identity" };
    case "CUERPO_DEMASIADO_GRANDE":
      return { message: KYC_MESSAGES.bodyTooLarge, action: "id" };
    default:
      return null;
  }
}

// -----------------------------------------------------------------------------
// Step 4: status
// -----------------------------------------------------------------------------

/** "0.9300" -> "93.0%"; null or garbage -> null. */
export function formatPercent(decimal: string | null | undefined, digits = 1): string | null {
  if (decimal == null || trimWs(decimal) === "") return null;
  const n = Number(decimal);
  if (!Number.isFinite(n)) return null;
  return `${(n * 100).toFixed(digits)}%`;
}

/** Findings of a result, translated, split into errors (rejection) and warnings (human review). */
export function splitDetails(details: readonly DetalleKyc[] | null | undefined): { errors: string[]; warnings: string[] } {
  const errors: string[] = [];
  const warnings: string[] = [];
  for (const d of details ?? []) {
    (d.tipo === "ERROR" ? errors : warnings).push(describeKycMessage(d.mensaje));
  }
  return { errors, warnings };
}

export type StatusTone = "verified" | "info" | "warning" | "danger";

export const STATUS_TONES: Record<EstadoKyc, StatusTone> = {
  PENDIENTE: "warning",
  EN_REVISION: "info",
  VERIFICADO: "verified",
  RECHAZADO: "danger",
};

/**
 * Headline of the status page. `awaitingProvider`: a submission is in review
 * but its provider result has not arrived yet (202). `suspended`: the client
 * is SUSPENDIDO (its file reads RECHAZADO).
 */
export function statusHeadline(
  file: RetryView,
  context: { awaitingProvider?: boolean; suspended?: boolean } = {},
): {
  title: string;
  body: string;
} {
  if (context.suspended) {
    return {
      title: "Your account is suspended",
      body: "Operations on this account are suspended for compliance reasons. Only our compliance team can lift the suspension.",
    };
  }
  if (file.estado_kyc === "EN_REVISION" && context.awaitingProvider) {
    return {
      title: "We are checking your photos",
      body: "The verification provider is analyzing your submission. This page updates by itself; you can also leave and come back later.",
    };
  }
  switch (file.estado_kyc) {
    case "VERIFICADO":
      return {
        title: "Your identity is verified",
        body: "Deposits, transfers, withdrawals and fixed-term investments are now enabled on your account.",
      };
    case "EN_REVISION":
      return {
        title: "A compliance analyst is reviewing your file",
        body: "Some checks need a person to confirm them. This page updates by itself; you can also leave and come back later.",
      };
    case "RECHAZADO":
      return {
        title: "We could not verify your identity",
        body: "Your file was closed after the verification checks. Only our compliance team can reopen it.",
      };
    case "PENDIENTE":
      if (file.bloqueo) return lockMessage(file.bloqueo);
      return canRetry(file)
        ? {
            title: "Your last attempt was not approved",
            body: `Review the reasons below and try again. You have ${file.intentos_restantes} ${
              file.intentos_restantes === 1 ? "attempt" : "attempts"
            } left.`,
          }
        : {
            title: "Verification not completed",
            body: "Finish the steps to verify your identity and enable money movements.",
          };
  }
}
