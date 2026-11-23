/**
 * Legal beneficiaries: validating what the API answers, checking drafts the
 * same way the server does (backend/src/kyc/beneficiarios.rs), the live
 * allocation in whole cents, and the request a save becomes.
 *
 * - Percentages are counted in integer cents (33.34% is 3334), so the
 *   allocation bar never reads 100% for 99.999 and never misses 100% because
 *   of floating point.
 * - Saving replaces the whole designation with a new version on top of the
 *   one the client loaded (`version_base`): the server answers 409 if another
 *   session saved first.
 *
 * Pure functions with relative `.ts` imports, so `node --test` runs them.
 */

import { parseCurp } from "./kyc.ts";
import { isRecord, isText } from "./vault.ts";
import type {
  Beneficiary,
  BeneficiariesView,
  BeneficiaryDraft,
  Designation,
  DesignationRequest,
  DesignationSummary,
  Relationship,
} from "../types/beneficiaries.ts";

/** The Legal Beneficiaries page. */
export const BENEFICIARIES_PATH = "/dashboard/beneficiaries";

export const MAX_BENEFICIARIES = 5;
/** 100% in cents. */
export const FULL = 10_000;

export const RELATIONSHIPS: readonly Relationship[] = [
  "CONYUGE",
  "CONCUBINO",
  "HIJO",
  "PADRE",
  "HERMANO",
  "NIETO",
  "ABUELO",
  "OTRO",
];

export const RELATIONSHIP_LABELS: Record<Relationship, string> = {
  CONYUGE: "Spouse",
  CONCUBINO: "Common-law partner",
  HIJO: "Child",
  PADRE: "Parent",
  HERMANO: "Sibling",
  NIETO: "Grandchild",
  ABUELO: "Grandparent",
  OTRO: "Other",
};

const NAME_MIN = 3;
const NAME_MAX = 120;

// ---------------------------------------------------------------------------
// Percentages in cents
// ---------------------------------------------------------------------------

/** "33.34" -> 3334; null unless it is a number with at most two decimals. */
export function percentToCents(input: string): number | null {
  const s = input.trim();
  const m = /^(\d{1,3})(?:\.(\d{1,2}))?$/.exec(s);
  if (!m) return null;
  const cents = Number(m[1]) * 100 + Number((m[2] ?? "").padEnd(2, "0"));
  return Number.isSafeInteger(cents) ? cents : null;
}

/** 3334 -> "33.34"; 6000 -> "60". */
export function centsToPercent(cents: number): string {
  const whole = Math.trunc(cents / 100);
  const frac = cents % 100;
  return frac === 0 ? String(whole) : `${whole}.${String(frac).padStart(2, "0")}`;
}

export type AllocationStatus = "empty" | "under" | "exact" | "over" | "invalid";

export interface Allocation {
  /** Sum of the valid percentages, in cents. */
  allocated: number;
  status: AllocationStatus;
  /** Each draft's share of the bar, in cents (0 when its percentage is not valid). */
  segments: { key: string; cents: number }[];
}

/** The live allocation of the drafts. */
export function allocation(drafts: readonly BeneficiaryDraft[]): Allocation {
  const segments = drafts.map((d) => ({ key: d.key, cents: percentToCents(d.percent) ?? 0 }));
  const allocated = segments.reduce((sum, s) => sum + s.cents, 0);
  const anyInvalid = drafts.some((d) => {
    const c = percentToCents(d.percent);
    return c === null || c <= 0;
  });
  let status: AllocationStatus;
  if (drafts.length === 0) status = "empty";
  else if (anyInvalid) status = "invalid";
  else if (allocated === FULL) status = "exact";
  else status = allocated < FULL ? "under" : "over";
  return { allocated, status, segments };
}

/** "100% allocated", "85% allocated · 15% left", "110% allocated · 10% over". */
export function allocationLabel(a: Allocation): string {
  const allocated = `${centsToPercent(a.allocated)}% allocated`;
  if (a.status === "empty") return "No beneficiaries";
  if (a.allocated < FULL) return `${allocated} · ${centsToPercent(FULL - a.allocated)}% left`;
  if (a.allocated > FULL) return `${allocated} · ${centsToPercent(a.allocated - FULL)}% over`;
  return allocated;
}

/** 100% split as evenly as cents allow, the first ones taking the odd cents: 3 -> 33.34, 33.33, 33.33. */
export function splitEvenly(count: number): string[] {
  if (count <= 0) return [];
  const base = Math.floor(FULL / count);
  const rest = FULL - base * count;
  return Array.from({ length: count }, (_, i) => centsToPercent(base + (i < rest ? 1 : 0)));
}

// ---------------------------------------------------------------------------
// Checking drafts (the server's rules)
// ---------------------------------------------------------------------------

/** The legal name as the server stores it: NFC, upper case, single spaces. */
export function normalizeLegalName(input: string): string {
  return input.normalize("NFC").toUpperCase().split(/\s+/).filter(Boolean).join(" ");
}

export interface DraftErrors {
  fullName?: string;
  relationship?: string;
  curp?: string;
  birthDate?: string;
  identity?: string;
  percent?: string;
}

/** What is wrong with one beneficiary (empty when nothing). `today`: "YYYY-MM-DD" in Mexico City. */
export function draftErrors(d: BeneficiaryDraft, today: string, holderCurp: string | null = null): DraftErrors {
  const errors: DraftErrors = {};
  const name = normalizeLegalName(d.fullName);
  const length = [...name].length;
  if (length === 0) errors.fullName = "Enter the full legal name.";
  else if (length < NAME_MIN || length > NAME_MAX) errors.fullName = `Use ${NAME_MIN} to ${NAME_MAX} characters.`;
  else if (!/^[\p{L} .'-]+$/u.test(name))
    errors.fullName = "Use letters, spaces, apostrophes, hyphens and periods only.";
  else if (name.split(" ").filter((w) => /\p{L}/u.test(w)).length < 2)
    errors.fullName = "Enter given name(s) and surname(s).";
  if (d.relationship === "") errors.relationship = "Choose the relationship.";

  const curpInput = d.curp.trim();
  let curpDate: string | null = null;
  if (curpInput) {
    const parsed = parseCurp(curpInput);
    if (!parsed.ok) errors.curp = parsed.error;
    else {
      curpDate = parsed.value.fechaNacimiento;
      if (holderCurp && parsed.value.curp === holderCurp) errors.curp = "You cannot name yourself as a beneficiary.";
    }
  }
  if (d.birthDate) {
    if (!/^\d{4}-\d{2}-\d{2}$/.test(d.birthDate) || Number.isNaN(Date.parse(`${d.birthDate}T00:00:00Z`))) {
      errors.birthDate = "Enter a valid date.";
    } else if (d.birthDate > today) errors.birthDate = "The date of birth cannot be in the future.";
    else if (d.birthDate < "1900-01-01") errors.birthDate = "The date of birth is before 1900.";
    else if (curpDate && curpDate !== d.birthDate) errors.birthDate = `The CURP says ${curpDate}.`;
  }
  if (!curpInput && !d.birthDate) errors.identity = "Enter their CURP or date of birth.";

  const cents = percentToCents(d.percent);
  if (d.percent.trim() === "") errors.percent = "Enter a percentage.";
  else if (cents === null) errors.percent = "Use a number with at most two decimals.";
  else if (cents <= 0 || cents > FULL) errors.percent = "Between 0.01 and 100.";
  return errors;
}

export function hasErrors(e: DraftErrors): boolean {
  return Object.keys(e).length > 0;
}

/** Why the designation cannot be saved yet, or null when it can. */
export function designationProblem(
  drafts: readonly BeneficiaryDraft[],
  today: string,
  holderCurp: string | null = null,
): string | null {
  if (drafts.length > MAX_BENEFICIARIES) return `Name at most ${MAX_BENEFICIARIES} beneficiaries.`;
  const invalid = drafts.findIndex((d) => hasErrors(draftErrors(d, today, holderCurp)));
  if (invalid >= 0) return `Check beneficiary ${invalid + 1}.`;
  const curps = drafts.map((d) => d.curp.trim().toUpperCase()).filter(Boolean);
  if (new Set(curps).size !== curps.length) return "The same CURP appears twice.";
  const a = allocation(drafts);
  if (drafts.length > 0 && a.allocated !== FULL) return "The percentages must add up to exactly 100%.";
  return null;
}

/** The body of `POST /api/v1/kyc/beneficiarios`. */
export function designationRequest(drafts: readonly BeneficiaryDraft[], versionBase: number): DesignationRequest {
  return {
    beneficiarios: drafts.map((d) => {
      const curp = d.curp.trim().toUpperCase();
      return {
        nombre_completo: normalizeLegalName(d.fullName),
        parentesco: d.relationship,
        ...(curp ? { curp } : {}),
        ...(d.birthDate ? { fecha_nacimiento: d.birthDate } : {}),
        porcentaje: centsToPercent(percentToCents(d.percent) ?? 0),
      };
    }),
    version_base: versionBase,
  };
}

let counter = 0;
/** A new, empty draft. */
export function emptyDraft(percent = ""): BeneficiaryDraft {
  counter += 1;
  return {
    key: `b${Date.now().toString(36)}${counter}`,
    fullName: "",
    relationship: "",
    curp: "",
    birthDate: "",
    percent,
  };
}

/** The saved designation as drafts to edit. */
export function draftsFrom(designation: Designation | null): BeneficiaryDraft[] {
  return (designation?.beneficiaries ?? []).map((b) => ({
    ...emptyDraft(),
    fullName: b.fullName,
    relationship: b.relationship,
    curp: b.curp ?? "",
    birthDate: b.birthDate ?? "",
    percent: centsToPercent(percentToCents(b.percent) ?? 0),
  }));
}

/** Whether the drafts differ from what is saved (same order, same values). */
export function isDirty(drafts: readonly BeneficiaryDraft[], designation: Designation | null): boolean {
  const saved = draftsFrom(designation);
  if (saved.length !== drafts.length) return true;
  return drafts.some((d, i) => {
    const s = saved[i];
    return (
      !s ||
      normalizeLegalName(d.fullName) !== s.fullName ||
      d.relationship !== s.relationship ||
      d.curp.trim().toUpperCase() !== s.curp ||
      d.birthDate !== s.birthDate ||
      percentToCents(d.percent) !== percentToCents(s.percent)
    );
  });
}

// ---------------------------------------------------------------------------
// Validating the API's answer
// ---------------------------------------------------------------------------

const HASH = /^[0-9a-f]{64}$/;
const isDate = (v: unknown): v is string => typeof v === "string" && /^\d{4}-\d{2}-\d{2}$/.test(v);
const isTimestamp = (v: unknown): v is string => isText(v, 64) && !Number.isNaN(Date.parse(v));
const isVersion = (v: unknown): v is number => typeof v === "number" && Number.isSafeInteger(v) && v >= 0;
const optional = <T>(v: unknown, check: (x: unknown) => x is T): T | null | undefined =>
  v === null || v === undefined ? null : check(v) ? v : undefined;

function parseBeneficiary(value: unknown): Beneficiary | null {
  if (!isRecord(value)) return null;
  const { posicion, nombre_completo, parentesco, curp, fecha_nacimiento, porcentaje } = value;
  const curpOk = optional(curp, (x): x is string => typeof x === "string" && /^[A-Z0-9]{18}$/.test(x));
  const dateOk = optional(fecha_nacimiento, isDate);
  if (
    typeof posicion !== "number" ||
    !Number.isInteger(posicion) ||
    posicion < 1 ||
    posicion > MAX_BENEFICIARIES ||
    !isText(nombre_completo, NAME_MAX) ||
    !RELATIONSHIPS.includes(parentesco as Relationship) ||
    curpOk === undefined ||
    dateOk === undefined ||
    typeof porcentaje !== "string" ||
    percentToCents(porcentaje) === null
  ) {
    return null;
  }
  return {
    position: posicion,
    fullName: nombre_completo,
    relationship: parentesco as Relationship,
    curp: curpOk,
    birthDate: dateOk,
    percent: porcentaje,
  };
}

function parseDesignation(value: unknown): Designation | null {
  if (!isRecord(value) || !Array.isArray(value.beneficiarios)) return null;
  const { version, creada_en, dispositivo, ip, hash, hash_anterior } = value;
  const device = optional(dispositivo, (x): x is string => isText(x, 80));
  const address = optional(ip, (x): x is string => isText(x, 64));
  if (
    !isVersion(version) ||
    !isTimestamp(creada_en) ||
    device === undefined ||
    address === undefined ||
    typeof hash !== "string" ||
    !HASH.test(hash) ||
    typeof hash_anterior !== "string" ||
    !HASH.test(hash_anterior)
  ) {
    return null;
  }
  const beneficiaries: Beneficiary[] = [];
  for (const raw of value.beneficiarios) {
    const b = parseBeneficiary(raw);
    if (!b) return null;
    beneficiaries.push(b);
  }
  if (beneficiaries.length > MAX_BENEFICIARIES) return null;
  return { version, savedAt: creada_en, device, ip: address, hash, previousHash: hash_anterior, beneficiaries };
}

function parseSummary(value: unknown): DesignationSummary | null {
  if (!isRecord(value)) return null;
  const { version, creada_en, num_beneficiarios, dispositivo, hash } = value;
  const device = optional(dispositivo, (x): x is string => isText(x, 80));
  if (
    !isVersion(version) ||
    !isTimestamp(creada_en) ||
    typeof num_beneficiarios !== "number" ||
    !Number.isInteger(num_beneficiarios) ||
    num_beneficiarios < 0 ||
    num_beneficiarios > MAX_BENEFICIARIES ||
    device === undefined ||
    typeof hash !== "string" ||
    !HASH.test(hash)
  ) {
    return null;
  }
  return { version, savedAt: creada_en, count: num_beneficiarios, device, hash };
}

/** `GET`/`POST /api/v1/kyc/beneficiarios`; null when malformed. */
export function parseBeneficiaries(value: unknown): BeneficiariesView | null {
  if (!isRecord(value) || !Array.isArray(value.historial)) return null;
  const { vigente, version, cadena_verificada, puede_designar, maximo } = value;
  const current = vigente === null || vigente === undefined ? null : parseDesignation(vigente);
  if (vigente !== null && vigente !== undefined && current === null) return null;
  if (!isVersion(version) || typeof cadena_verificada !== "boolean" || typeof puede_designar !== "boolean") return null;
  if (typeof maximo !== "number" || maximo !== MAX_BENEFICIARIES) return null;
  if ((current?.version ?? 0) !== version) return null;
  const history: DesignationSummary[] = [];
  for (const raw of value.historial) {
    const s = parseSummary(raw);
    if (!s) return null;
    history.push(s);
  }
  return { current, version, history, chainVerified: cadena_verificada, canDesignate: puede_designar, max: maximo };
}
