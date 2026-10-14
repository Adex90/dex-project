/**
 * Mexican identity rules for the digital onboarding (KYC) forms, ported from
 * the backend so the customer gets the same verdict before submitting:
 *
 * - CURP (RENAPO): backend/src/kyc/curp.rs
 * - RFC of an individual (SAT): backend/src/kyc/rfc.rs
 * - Voting credential (INE): backend/src/kyc/ine.rs
 * - Name normalization and short text fields: backend/src/kyc/texto.rs
 * - Step 1 (personal data) and step 2/3 images: backend/src/kyc/expediente.rs
 *
 * Errors block the submission (the backend answers 400 VALIDACION or 422).
 * Warnings (e.g. CURP letters that do not match the typed name) do not block,
 * but the backend sends the file to human review instead of approving it.
 *
 * Pure and framework-free so `node --test` can import it (no TS-only runtime
 * syntax). Dates are "YYYY-MM-DD" strings; "today" is always an argument
 * (see `todayInMexicoCity`) so results are reproducible.
 */
import type { DatosIneInput, DatosPersonalesInput, Domicilio, EstadoKyc, Sexo } from "./api/types.ts";

/** Result of a validator: the normalized value, or a stable code plus an English message. */
export type Validated<T, C extends string> = { ok: true; value: T } | { ok: false; code: C; error: string };

function fail<C extends string>(code: C, error: string): { ok: false; code: C; error: string } {
  return { ok: false, code, error };
}

// -----------------------------------------------------------------------------
// Catalogs
// -----------------------------------------------------------------------------

export interface Entidad {
  /** RENAPO code used in the CURP (positions 12-13). */
  code: string;
  name: string;
  /** INEGI numeric code used in the clave de elector; 0 for NE (born abroad). */
  inegi: number;
}

/** RENAPO birth-state catalog (curp.rs ENTIDADES). */
export const ENTIDADES: readonly Entidad[] = [
  { code: "AS", name: "Aguascalientes", inegi: 1 },
  { code: "BC", name: "Baja California", inegi: 2 },
  { code: "BS", name: "Baja California Sur", inegi: 3 },
  { code: "CC", name: "Campeche", inegi: 4 },
  { code: "CL", name: "Coahuila", inegi: 5 },
  { code: "CM", name: "Colima", inegi: 6 },
  { code: "CS", name: "Chiapas", inegi: 7 },
  { code: "CH", name: "Chihuahua", inegi: 8 },
  { code: "DF", name: "Ciudad de México", inegi: 9 },
  { code: "DG", name: "Durango", inegi: 10 },
  { code: "GT", name: "Guanajuato", inegi: 11 },
  { code: "GR", name: "Guerrero", inegi: 12 },
  { code: "HG", name: "Hidalgo", inegi: 13 },
  { code: "JC", name: "Jalisco", inegi: 14 },
  { code: "MC", name: "Estado de México", inegi: 15 },
  { code: "MN", name: "Michoacán", inegi: 16 },
  { code: "MS", name: "Morelos", inegi: 17 },
  { code: "NT", name: "Nayarit", inegi: 18 },
  { code: "NL", name: "Nuevo León", inegi: 19 },
  { code: "OC", name: "Oaxaca", inegi: 20 },
  { code: "PL", name: "Puebla", inegi: 21 },
  { code: "QT", name: "Querétaro", inegi: 22 },
  { code: "QR", name: "Quintana Roo", inegi: 23 },
  { code: "SP", name: "San Luis Potosí", inegi: 24 },
  { code: "SL", name: "Sinaloa", inegi: 25 },
  { code: "SR", name: "Sonora", inegi: 26 },
  { code: "TC", name: "Tabasco", inegi: 27 },
  { code: "TS", name: "Tamaulipas", inegi: 28 },
  { code: "TL", name: "Tlaxcala", inegi: 29 },
  { code: "VZ", name: "Veracruz", inegi: 30 },
  { code: "YN", name: "Yucatán", inegi: 31 },
  { code: "ZS", name: "Zacatecas", inegi: 32 },
  { code: "NE", name: "Born abroad", inegi: 0 },
];

/** The 32 federal entities (for an address "state" select): the catalog without NE. */
export const ESTADOS: readonly Entidad[] = ENTIDADES.filter((e) => e.inegi > 0);

/** Name of a RENAPO code ("DF" -> "Ciudad de México"), or null if unknown. */
export function entidadName(code: string | null | undefined): string | null {
  return ENTIDADES.find((e) => e.code === code)?.name ?? null;
}

/** INEGI code of a RENAPO code (curp::clave_inegi); null for NE or unknown codes. */
export function inegiCode(code: string): number | null {
  const n = ENTIDADES.find((e) => e.code === code)?.inegi;
  return n !== undefined && n > 0 ? n : null;
}

export const SEXO_LABELS: Record<Sexo, string> = { H: "Male", M: "Female", X: "Non-binary" };

export const ESTADO_KYC_LABELS: Record<EstadoKyc, string> = {
  PENDIENTE: "Pending",
  EN_REVISION: "In review",
  VERIFICADO: "Verified",
  RECHAZADO: "Rejected",
};

// -----------------------------------------------------------------------------
// English messages (one per backend message; see `kycMessageInEnglish`)
// -----------------------------------------------------------------------------

const MIN_SELFIE = 3;
const MAX_SELFIE = 8;

export const KYC_MESSAGES = {
  curpLength: "The CURP must have 18 characters.",
  curpStructure:
    "The CURP does not follow the official format (4 letters, birth date YYMMDD, sex, state, 3 consonants, homoclave and check digit).",
  curpDate: "The birth date in the CURP does not exist.",
  curpEntity: (code: string) => `Birth state ${code} in the CURP does not exist.`,
  curpCheckDigit: "The CURP check digit does not match.",
  curpLetters: (real: string, expected: string) =>
    `The CURP letters (${real.slice(0, 4)}…${real.slice(4)}) do not match the name you entered (expected ${expected.slice(0, 4)}…${expected.slice(4)}).`,
  rfcLength: "An individual's RFC has 13 characters.",
  rfcStructure: "The RFC does not follow the official format (4 letters, date YYMMDD and homoclave).",
  rfcDate: "The date in the RFC does not exist.",
  rfcCheckDigit: "The RFC check digit does not match.",
  rfcGeneric: "A generic RFC does not identify a person.",
  rfcInvalid: "Invalid RFC.",
  rfcDateMismatch: "The birth date in the RFC does not match the one in the CURP.",
  rfcLettersMismatch: "The RFC letters do not match the CURP's.",
  claveFormat:
    "The voter key (clave de elector) has 18 characters: 6 letters, birth date YYMMDD, state, sex and 3 digits.",
  claveDate: "The date in the voter key does not exist.",
  cic: "The credential's CIC has 9 digits.",
  citizenId: "The citizen identifier has 9 or 13 digits.",
  claveDateMismatch: "The birth date in the voter key does not match the one in the CURP.",
  claveSexMismatch: "The sex in the voter key does not match the one in the CURP.",
  claveEntityMismatch: "The birth state in the voter key does not match the one in the CURP.",
  issueYear: (year: number | string) => `Issue year ${year} is not valid.`,
  validitySpan: "A voting credential is valid for up to 10 years from its issue year.",
  expired: (year: number | string) => `The credential expired in ${year}.`,
  postalCode: "The postal code has 5 digits.",
  underage: "Remote account opening is only available to adults (18 or older).",
  implausibleBirth: "The birth date in the CURP is not plausible.",
  required: (label: string) => `${label} is required.`,
  tooLong: (label: string, max: number | string) => `${label} can have at most ${max} characters.`,
  invalidChars: (label: string) => `${label} contains invalid characters.`,
  needsLetters: (label: string) => `${label} must contain letters.`,
  imageBase64: (label: string) => `${label}: the image must be base64-encoded.`,
  imageSize: (label: string) => `${label}: the image must be between 1 KB and 3 MB.`,
  imageType: (label: string) => `${label}: only JPEG, PNG or WebP images are accepted.`,
  frameCount: `The liveness check needs ${MIN_SELFIE} to ${MAX_SELFIE} camera frames.`,
  distinctImages: "Each image (front, back and every selfie frame) must be different.",
  bodyTooLarge: "The photos are too large to send together (12 MB maximum). Retake them at a lower resolution.",
} as const;

const M = KYC_MESSAGES;

// -----------------------------------------------------------------------------
// Whitespace as Rust sees it
// -----------------------------------------------------------------------------

// Rust's str::trim, split_whitespace and char::is_whitespace use the Unicode
// White_Space property; JavaScript's trim() and \s differ on U+0085 (NEL,
// whitespace in Rust) and U+FEFF (BOM, whitespace in JavaScript).
const EDGE_WHITESPACE = /^\p{White_Space}+|\p{White_Space}+$/gu;
const WHITESPACE_RUN = /\p{White_Space}+/gu;

/** `str::trim`: removes leading and trailing Unicode whitespace. */
export function trimWs(s: string): string {
  return s.replace(EDGE_WHITESPACE, "");
}

/** Removes every Unicode whitespace character. */
function stripWs(s: string): string {
  return s.replace(WHITESPACE_RUN, "");
}

// -----------------------------------------------------------------------------
// Dates
// -----------------------------------------------------------------------------

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

function dateExists(year: number, month: number, day: number): boolean {
  if (![year, month, day].every(Number.isInteger)) return false;
  // setUTCFullYear: Date.UTC would map years 0-99 to 1900-1999.
  const d = new Date(0);
  d.setUTCFullYear(year, month - 1, day);
  return d.getUTCFullYear() === year && d.getUTCMonth() === month - 1 && d.getUTCDate() === day;
}

function isoDate(year: number, month: number, day: number): string {
  return `${String(year).padStart(4, "0")}-${pad2(month)}-${pad2(day)}`;
}

/** Splits a "YYYY-MM-DD" date; throws RangeError if it is not a real date. */
function splitIsoDate(value: string, what: string): { year: number; month: number; day: number } {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  const [year, month, day] = m ? [Number(m[1]), Number(m[2]), Number(m[3])] : [NaN, NaN, NaN];
  if (!m || !dateExists(year, month, day)) throw new RangeError(`${what} must be a YYYY-MM-DD date, got "${value}"`);
  return { year, month, day };
}

/** A YYMMDD date exists with century 1900 or 2000 (RFC and clave de elector). */
function yymmddExists(yymmdd: string): boolean {
  const [yy, mm, dd] = [Number(yymmdd.slice(0, 2)), Number(yymmdd.slice(2, 4)), Number(yymmdd.slice(4, 6))];
  return [1900, 2000].some((century) => dateExists(century + yy, mm, dd));
}

/**
 * The backend's "today" (spei::hoy_cdmx): Mexico City has been UTC-6 all year
 * since daylight saving time was abolished in 2022.
 */
export function todayInMexicoCity(now: Date = new Date()): string {
  return new Date(now.getTime() - 6 * 3_600_000).toISOString().slice(0, 10);
}

/** Age in whole years on `today` (expediente.rs edad). */
export function ageOn(birthDate: string, today: string): number {
  const b = splitIsoDate(birthDate, "birthDate");
  const t = splitIsoDate(today, "today");
  let years = t.year - b.year;
  if (t.month < b.month || (t.month === b.month && t.day < b.day)) years -= 1;
  return years;
}

// -----------------------------------------------------------------------------
// Name normalization (texto.rs)
// -----------------------------------------------------------------------------

const ACCENTS: Record<string, string> = {
  Á: "A", À: "A", Â: "A", Ä: "A", Ã: "A",
  É: "E", È: "E", Ê: "E", Ë: "E",
  Í: "I", Ì: "I", Î: "I", Ï: "I",
  Ó: "O", Ò: "O", Ô: "O", Ö: "O", Õ: "O",
  Ú: "U", Ù: "U", Û: "U", Ü: "U",
  Ç: "C",
};

/**
 * Upper case, without accents or diaeresis, any other sign (hyphen, period,
 * apostrophe, digit...) turned into a space and spaces collapsed. Ñ is kept:
 * each rule decides what to do with it. ("José-María O'Neill" -> "JOSE MARIA O NEILL")
 *
 * The text is NFC-normalized first: the backend maps precomposed letters
 * only, and `checkProfile` sends NFC text, so a decomposed "é" typed here
 * gives the same verdict the backend will give on the submitted body.
 */
export function normalizeName(s: string): string {
  let out = "";
  for (const ch of s.normalize("NFC")) {
    for (const up of ch.toUpperCase()) {
      out += ACCENTS[up] ?? ((up >= "A" && up <= "Z") || up === "Ñ" ? up : " ");
    }
  }
  return out.split(" ").filter(Boolean).join(" ");
}

/** Normalized words of a name. */
export function nameWords(s: string): string[] {
  return normalizeName(s).split(" ").filter(Boolean);
}

/** Same name as written by two sources; non-strict treats Ñ as N (the MRZ has no Ñ). */
export function sameName(a: string, b: string, strict: boolean): boolean {
  const [x, y] = [normalizeName(a), normalizeName(b)];
  return strict ? x === y : x.replace(/Ñ/g, "N") === y.replace(/Ñ/g, "N");
}

/** Short free text (texto::texto_corto): trimmed, bounded, no control characters, spaces collapsed. */
export function shortText(label: string, value: string, max: number): Validated<string, "REQUIRED" | "TOO_LONG" | "INVALID"> {
  const v = trimWs(value);
  if (!v) return fail("REQUIRED", M.required(label));
  if ([...v].length > max) return fail("TOO_LONG", M.tooLong(label, max));
  if (/\p{Cc}/u.test(v)) return fail("INVALID", M.invalidChars(label));
  return { ok: true, value: v.replace(WHITESPACE_RUN, " ") };
}

// -----------------------------------------------------------------------------
// CURP (curp.rs)
// -----------------------------------------------------------------------------

/**
 * What an ID field (CURP, RFC, clave de elector) should hold while typing:
 * upper case without spaces. The validators also accept surrounding spaces
 * and lower case, like the backend.
 */
export function cleanIdInput(value: string): string {
  return stripWs(value).toUpperCase();
}

export const CURP_PATTERN = /^[A-Z]{4}[0-9]{6}[HMX][A-Z]{5}[A-Z0-9][0-9]$/;
const CURP_ALPHABET = "0123456789ABCDEFGHIJKLMNÑOPQRSTUVWXYZ";

export type CurpErrorCode = "LENGTH" | "STRUCTURE" | "DATE" | "ENTITY" | "CHECK_DIGIT";

export interface ParsedCurp {
  /** Upper-case CURP. */
  curp: string;
  /** "YYYY-MM-DD": position 17 is a digit for people born before 2000, a letter since 2000. */
  fechaNacimiento: string;
  sexo: Sexo;
  /** RENAPO birth-state code ("DF", "NE"...). */
  entidad: string;
}

/**
 * Check digit (position 18) of the first 17 characters, or null if they are
 * not 17 characters of the CURP alphabet (curp::digito_verificador): values
 * over "0-9A-NÑO-Z" weighted 18..2, digit = (10 - sum % 10) % 10.
 */
export function curpCheckDigit(first17: string): string | null {
  const chars = [...first17];
  if (chars.length !== 17) return null;
  let sum = 0;
  for (const [i, ch] of chars.entries()) {
    const value = CURP_ALPHABET.indexOf(ch);
    if (value < 0) return null;
    sum += value * (18 - i);
  }
  return String((10 - (sum % 10)) % 10);
}

/** Structure, birth date, birth state and check digit (Curp::parse). Trims and upper-cases. */
export function parseCurp(input: string): Validated<ParsedCurp, CurpErrorCode> {
  const curp = trimWs(input).toUpperCase();
  // Rust compares both the character and the byte count with 18.
  if (curp.length !== 18 || [...curp].some((ch) => ch.charCodeAt(0) > 0x7f)) return fail("LENGTH", M.curpLength);
  if (!CURP_PATTERN.test(curp)) return fail("STRUCTURE", M.curpStructure);
  const century = /[0-9]/.test(curp.charAt(16)) ? 1900 : 2000;
  const year = century + Number(curp.slice(4, 6));
  const month = Number(curp.slice(6, 8));
  const day = Number(curp.slice(8, 10));
  if (!dateExists(year, month, day)) return fail("DATE", M.curpDate);
  const entidad = curp.slice(11, 13);
  if (!ENTIDADES.some((e) => e.code === entidad)) return fail("ENTITY", M.curpEntity(entidad));
  if (curpCheckDigit(curp.slice(0, 17)) !== curp.charAt(17)) return fail("CHECK_DIGIT", M.curpCheckDigit);
  return { ok: true, value: { curp, fechaNacimiento: isoDate(year, month, day), sexo: curp.charAt(10) as Sexo, entidad } };
}

// Letters derived from the name (RENAPO normative instructions).

/** Prepositions, conjunctions and contractions skipped in compound names. */
const PARTICLES = new Set([
  "DA", "DAS", "DE", "DEL", "DER", "DI", "DIE", "DD", "EL", "LA", "LOS", "LAS", "LE", "LES", "MAC", "MC", "VAN", "VON", "Y",
]);

/** In a compound given name starting with one of these, the second one is used. */
const SKIPPED_GIVEN_NAMES = new Set(["MARIA", "MA", "M", "JOSE", "J"]);

/** Offensive words: their second letter becomes X. */
const INCONVENIENT_WORDS = new Set([
  "BACA", "BAKA", "BUEI", "BUEY", "CACA", "CACO", "CAGA", "CAGO", "CAKA", "CAKO", "COGE", "COGI", "COJA", "COJE",
  "COJI", "COJO", "COLA", "CULO", "FALO", "FETO", "GETA", "GUEI", "GUEY", "JETA", "JOTO", "KACA", "KACO", "KAGA",
  "KAGO", "KAKA", "KAKO", "KOGE", "KOGI", "KOJA", "KOJE", "KOJI", "KOJO", "KOLA", "KULO", "LILO", "LOCA", "LOCO",
  "LOKA", "LOKO", "MAME", "MAMO", "MEAR", "MEAS", "MEON", "MIAR", "MION", "MOCO", "MOKO", "MULA", "MULO", "NACA",
  "NACO", "PEDA", "PEDO", "PENE", "PIPI", "PITO", "POPO", "PUTA", "PUTO", "QULO", "RATA", "ROBA", "ROBE", "ROBO",
  "RUIN", "SENO", "TETA", "VACA", "VAGA", "VAGO", "VAKA", "VUEI", "VUEY",
]);

function withoutParticles(s: string): string[] {
  const all = nameWords(s);
  const useful = all.filter((w) => !PARTICLES.has(w));
  return useful.length > 0 ? useful : all;
}

function surnameWord(s: string): string | null {
  return withoutParticles(s)[0] ?? null;
}

function givenNameWord(s: string): string | null {
  const words = withoutParticles(s);
  const [first, second] = words;
  if (first !== undefined && second !== undefined && SKIPPED_GIVEN_NAMES.has(first)) return second;
  return first ?? null;
}

const isVowel = (c: string) => c === "A" || c === "E" || c === "I" || c === "O" || c === "U";
const curpLetter = (c: string) => (c === "Ñ" ? "X" : c);

function initial(word: string | null): string {
  const c = word === null ? undefined : [...word][0];
  return c === undefined ? "X" : curpLetter(c);
}

function internalVowel(word: string | null): string {
  return (word === null ? undefined : [...word].slice(1).find(isVowel)) ?? "X";
}

function internalConsonant(word: string | null): string {
  const c = word === null ? undefined : [...word].slice(1).find((ch) => !isVowel(ch));
  return c === undefined ? "X" : curpLetter(c);
}

/** The 7 CURP letters that depend on the name: positions 1-4 and 14-16 (curp::derivar_letras). */
export function deriveCurpLetters(nombres: string, apellidoPaterno: string, apellidoMaterno?: string | null): string {
  const pat = surnameWord(apellidoPaterno);
  const mat = apellidoMaterno == null ? null : surnameWord(apellidoMaterno);
  const nom = givenNameWord(nombres);
  const four = [initial(pat), internalVowel(pat), initial(mat), initial(nom)];
  if (INCONVENIENT_WORDS.has(four.join(""))) four[1] = "X";
  return four.join("") + internalConsonant(pat) + internalConsonant(mat) + internalConsonant(nom);
}

function curpText(curp: string | ParsedCurp): string {
  return typeof curp === "string" ? trimWs(curp).toUpperCase() : curp.curp;
}

/**
 * Warning when the CURP letters do not correspond to the typed name
 * (Curp::advertencias_de_nombre). Not an error: RENAPO has exceptions
 * (homonyms, manual assignment) that cannot be reproduced with certainty;
 * the backend sends those files to human review.
 */
export function curpNameWarnings(
  curp: string | ParsedCurp,
  nombres: string,
  apellidoPaterno: string,
  apellidoMaterno?: string | null,
): string[] {
  const value = curpText(curp);
  const expected = deriveCurpLetters(nombres, apellidoPaterno, apellidoMaterno);
  const real = value.slice(0, 4) + value.slice(13, 16);
  return expected === real ? [] : [M.curpLetters(real, expected)];
}

export interface PersonName {
  nombres: string;
  apellidoPaterno: string;
  apellidoMaterno?: string | null;
}

function nonNegativeInteger(n: number, what: string): number {
  if (!Number.isSafeInteger(n) || n < 0) throw new RangeError(`${what} must be a non-negative integer, got ${n}`);
  return n;
}

/**
 * A valid CURP for a person (curp::construir). RENAPO assigns the real
 * homoclave (position 17); here it is an argument: digit `homoclave % 10`
 * before 2000, letter `A + homoclave % 26` since 2000. For tests and demos.
 */
export function buildCurp(
  person: PersonName & { fechaNacimiento: string; sexo: Sexo; entidad: string; homoclave: number },
): string {
  const { year, month, day } = splitIsoDate(person.fechaNacimiento, "fechaNacimiento");
  if (year < 1900 || year > 2099) throw new RangeError("a CURP encodes birth years 1900-2099");
  if (!ENTIDADES.some((e) => e.code === person.entidad)) throw new RangeError(`unknown entidad ${person.entidad}`);
  const h = nonNegativeInteger(person.homoclave, "homoclave");
  const letters = deriveCurpLetters(person.nombres, person.apellidoPaterno, person.apellidoMaterno);
  const differentiator = year < 2000 ? String(h % 10) : String.fromCharCode(65 + (h % 26));
  const first17 =
    letters.slice(0, 4) +
    pad2(year % 100) +
    pad2(month) +
    pad2(day) +
    person.sexo +
    person.entidad +
    letters.slice(4) +
    differentiator;
  const digit = curpCheckDigit(first17);
  if (digit === null) throw new RangeError(`cannot build a CURP from ${first17}`);
  return first17 + digit;
}

// -----------------------------------------------------------------------------
// RFC of an individual (rfc.rs)
// -----------------------------------------------------------------------------

export const RFC_PATTERN = /^[A-ZÑ&]{4}[0-9]{6}[A-Z0-9]{2}[0-9A]$/u;
const RFC_ALPHABET = "0123456789ABCDEFGHIJKLMN&OPQRSTUVWXYZ Ñ";
const GENERIC_RFCS = new Set(["XAXX010101000", "XEXX010101000"]);

export type RfcErrorCode = "LENGTH" | "STRUCTURE" | "DATE" | "CHECK_DIGIT" | "GENERIC";

/**
 * Check digit of the first 12 (individual) or 11 (company, padded with a
 * leading space) characters (rfc::digito_verificador): values over
 * "0-9A-N&O-Z Ñ" weighted 13..2; remainder 0 -> "0", 11 - r = 10 -> "A".
 */
export function rfcCheckDigit(base: string): string | null {
  const chars = [...base];
  if (chars.length === 11) chars.unshift(" ");
  if (chars.length !== 12) return null;
  let sum = 0;
  for (const [i, ch] of chars.entries()) {
    const value = RFC_ALPHABET.indexOf(ch);
    if (value < 0) return null;
    sum += value * (13 - i);
  }
  const r = sum % 11;
  if (r === 0) return "0";
  return 11 - r === 10 ? "A" : String(11 - r);
}

/** Validates an individual's RFC (rfc::validar_persona_fisica) and returns it upper-cased. */
export function validateRfc(input: string): Validated<string, RfcErrorCode> {
  const rfc = trimWs(input).toUpperCase();
  const chars = [...rfc];
  if (chars.length !== 13) return fail("LENGTH", M.rfcLength);
  if (GENERIC_RFCS.has(rfc)) return fail("GENERIC", M.rfcGeneric);
  if (!RFC_PATTERN.test(rfc)) return fail("STRUCTURE", M.rfcStructure);
  if (!yymmddExists(chars.slice(4, 10).join(""))) return fail("DATE", M.rfcDate);
  if (rfcCheckDigit(chars.slice(0, 12).join("")) !== chars[12]) return fail("CHECK_DIGIT", M.rfcCheckDigit);
  return { ok: true, value: rfc };
}

/**
 * Cross-check with the CURP (rfc::cotejar_con_curp): the birth date must be
 * the same (error); the letters almost always are (SAT uses another list of
 * offensive words: warning).
 */
export function rfcCurpCheck(rfc: string, curp: string | ParsedCurp): { errors: string[]; warnings: string[] } {
  const errors: string[] = [];
  const warnings: string[] = [];
  const r = [...trimWs(rfc).toUpperCase()];
  const c = [...curpText(curp)];
  if (r.length !== 13) {
    errors.push(M.rfcInvalid);
    return { errors, warnings };
  }
  if (r.slice(4, 10).join("") !== c.slice(4, 10).join("")) errors.push(M.rfcDateMismatch);
  if (r.slice(0, 4).join("") !== c.slice(0, 4).join("")) warnings.push(M.rfcLettersMismatch);
  return { errors, warnings };
}

/**
 * A valid RFC for a CURP with the given 2-character homoclave
 * (rfc::construir_desde_curp): the CURP's 4 letters and birth date, the
 * homoclave (SAT assigns the real one) and the check digit. For tests and demos.
 */
export function buildRfcFromCurp(curp: string, homoclave: string): string {
  const prefix = trimWs(curp).toUpperCase().slice(0, 10);
  const h = trimWs(homoclave).toUpperCase();
  if (!/^[A-ZÑ&]{4}[0-9]{6}$/u.test(prefix) || !/^[A-Z0-9]{2}$/.test(h)) {
    throw new RangeError(`cannot build an RFC from CURP "${curp}" and homoclave "${homoclave}"`);
  }
  const base = prefix + h;
  const digit = rfcCheckDigit(base);
  if (digit === null) throw new RangeError(`cannot build an RFC from ${base}`);
  return base + digit;
}

// -----------------------------------------------------------------------------
// Voting credential (ine.rs)
// -----------------------------------------------------------------------------

export const CLAVE_ELECTOR_PATTERN = /^[A-Z]{6}[0-9]{8}[HMX][0-9]{3}$/;

export interface ParsedClaveElector {
  claveElector: string;
  /** Birth date YYMMDD (characters 7-12). */
  aammdd: string;
  /** INEGI code of the birth state (characters 13-14). */
  entidad: number;
  sexo: Sexo;
}

/** Structure and date of a clave de elector (ClaveElector::parse). Trims and upper-cases. */
export function parseClaveElector(input: string): Validated<ParsedClaveElector, "FORMAT" | "DATE"> {
  const clave = trimWs(input).toUpperCase();
  if (!CLAVE_ELECTOR_PATTERN.test(clave) || clave.slice(12, 14) === "00") return fail("FORMAT", M.claveFormat);
  if (!yymmddExists(clave.slice(6, 12))) return fail("DATE", M.claveDate);
  return {
    ok: true,
    value: { claveElector: clave, aammdd: clave.slice(6, 12), entidad: Number(clave.slice(12, 14)), sexo: clave.charAt(14) as Sexo },
  };
}

/** Error message, or null if the CIC (9 digits) is valid. */
export function validateCic(cic: string): string | null {
  return /^[0-9]{9}$/.test(cic) ? null : M.cic;
}

/** Error message, or null if the citizen identifier (9 or 13 digits) is valid. */
export function validateCitizenId(id: string): string | null {
  return /^(?:[0-9]{9}|[0-9]{13})$/.test(id) ? null : M.citizenId;
}

/** Credential data the customer types in step 2 (the API's `ine` object without images). */
export type CredentialInput = Pick<DatosIneInput, "clave_elector" | "cic" | "identificador_ciudadano" | "emision" | "vigencia">;
export type CredentialField = keyof CredentialInput;

export interface CredentialCheck {
  /** In the backend's order (it joins them with "; " in a 400 VALIDACION). */
  errors: string[];
  warnings: string[];
  /** First error of each field, for inline form messages. */
  fieldErrors: Partial<Record<CredentialField, string>>;
}

/**
 * The typed credential against the CURP of step 1 (ine::validar_credencial),
 * after the same trimming the backend applies before validating. A credential
 * is valid through December 31 of its `vigencia` year.
 */
export function validateCredential(input: CredentialInput, curp: ParsedCurp, today: string): CredentialCheck {
  const check: CredentialCheck = { errors: [], warnings: [], fieldErrors: {} };
  const error = (field: CredentialField, message: string) => {
    check.errors.push(message);
    check.fieldErrors[field] ??= message;
  };
  const clave = parseClaveElector(input.clave_elector);
  if (!clave.ok) {
    error("clave_elector", clave.error);
  } else {
    if (clave.value.aammdd !== curp.fechaNacimiento.slice(2, 4) + curp.fechaNacimiento.slice(5, 7) + curp.fechaNacimiento.slice(8, 10)) {
      error("clave_elector", M.claveDateMismatch);
    }
    if (clave.value.sexo !== "X" && curp.sexo !== "X" && clave.value.sexo !== curp.sexo) {
      error("clave_elector", M.claveSexMismatch);
    }
    const inegi = inegiCode(curp.entidad);
    if (inegi !== null && inegi !== clave.value.entidad) check.warnings.push(M.claveEntityMismatch);
  }
  const cicError = validateCic(trimWs(input.cic));
  if (cicError) error("cic", cicError);
  const citizenId = trimWs(input.identificador_ciudadano ?? "");
  if (citizenId) {
    const idError = validateCitizenId(citizenId);
    if (idError) error("identificador_ciudadano", idError);
  }
  const year = splitIsoDate(today, "today").year;
  const { emision, vigencia } = input;
  if (!Number.isInteger(emision) || emision > year || emision < 1990) error("emision", M.issueYear(emision));
  if (!Number.isInteger(vigencia) || vigencia < emision || vigencia - emision > 10) error("vigencia", M.validitySpan);
  if (vigencia < year) error("vigencia", M.expired(vigencia));
  return check;
}

/**
 * A valid clave de elector for a person (ine::construir_clave_elector): the
 * 6 letters are the initial and first internal consonant of each surname and
 * of the given names. For tests and demos.
 */
export function buildClaveElector(
  person: PersonName & { fechaNacimiento: string; entidadInegi: number; sexo: Sexo; homoclave: number },
): string {
  const { year, month, day } = splitIsoDate(person.fechaNacimiento, "fechaNacimiento");
  const inegi = nonNegativeInteger(person.entidadInegi, "entidadInegi");
  if (inegi < 1 || inegi > 99) throw new RangeError("entidadInegi must be 1-99 (00 is not a valid state)");
  const h = nonNegativeInteger(person.homoclave, "homoclave");
  const two = (s: string) => {
    const chars = [...normalizeName(s).replace(/Ñ/g, "X").replace(/ /g, "")];
    const letters = chars.slice(0, 1);
    const consonant = chars.slice(1).find((c) => !isVowel(c));
    if (consonant !== undefined) letters.push(consonant);
    while (letters.length < 2) letters.push("X");
    return letters.join("");
  };
  return (
    two(person.apellidoPaterno) +
    two(person.apellidoMaterno ?? "X") +
    two(person.nombres) +
    pad2(year % 100) +
    pad2(month) +
    pad2(day) +
    pad2(inegi) +
    person.sexo +
    String(h % 1000).padStart(3, "0")
  );
}

// -----------------------------------------------------------------------------
// Step 1: personal data and address (expediente.rs validar_datos)
// -----------------------------------------------------------------------------

/** Error message, or null if the postal code (5 digits) is valid. */
export function validatePostalCode(cp: string): string | null {
  return /^[0-9]{5}$/.test(trimWs(cp)) ? null : M.postalCode;
}

export type ProfileField = Exclude<keyof DatosPersonalesInput, "domicilio"> | keyof Domicilio;

export const PROFILE_LABELS: Record<ProfileField, string> = {
  nombres: "Given names",
  apellido_paterno: "Paternal surname",
  apellido_materno: "Maternal surname",
  curp: "CURP",
  rfc: "RFC",
  calle: "Street",
  numero_exterior: "Exterior number",
  numero_interior: "Interior number",
  colonia: "Neighborhood (colonia)",
  municipio: "Municipality",
  estado: "State",
  codigo_postal: "Postal code",
};

/** Maximum lengths of the step 1 text fields (characters). */
export const PROFILE_MAX_LENGTH = {
  nombres: 120,
  apellido_paterno: 120,
  apellido_materno: 120,
  calle: 120,
  numero_exterior: 20,
  numero_interior: 20,
  colonia: 120,
  municipio: 120,
  estado: 60,
} as const;

export interface ProfileCheck {
  /** First error of each field; empty when the form can be submitted. */
  errors: Partial<Record<ProfileField, string>>;
  /** Non-blocking findings the backend will also report in `advertencias`. */
  warnings: string[];
  /** The parsed CURP (birth date, sex and state derived from it), when valid. */
  curp: ParsedCurp | null;
  /** Normalized body for POST /kyc/perfil (only the allowed fields), when there are no errors. */
  body: DatosPersonalesInput | null;
}

const nfc = (s: string | null | undefined) => (s ?? "").normalize("NFC");

/**
 * Step 1 validation as the backend does it, with every field checked (the
 * backend stops at the first error). Text is NFC-normalized first so the
 * browser and the backend derive the CURP letters from the same characters.
 */
export function checkProfile(input: DatosPersonalesInput, today: string): ProfileCheck {
  const errors: ProfileCheck["errors"] = {};
  const warnings: string[] = [];
  const text = (field: keyof typeof PROFILE_MAX_LENGTH, value: string, optional = false): string | null => {
    if (optional && !trimWs(value)) return null;
    const r = shortText(PROFILE_LABELS[field], value, PROFILE_MAX_LENGTH[field]);
    if (!r.ok) {
      errors[field] = r.error;
      return null;
    }
    return r.value;
  };
  const name = (field: "nombres" | "apellido_paterno" | "apellido_materno", value: string, optional = false) => {
    const v = text(field, value, optional);
    if (v !== null && nameWords(v).length === 0) {
      errors[field] = M.needsLetters(PROFILE_LABELS[field]);
      return null;
    }
    return v;
  };

  const nombres = name("nombres", nfc(input.nombres));
  const paterno = name("apellido_paterno", nfc(input.apellido_paterno));
  const materno = name("apellido_materno", nfc(input.apellido_materno), true);

  const parsed = parseCurp(nfc(input.curp));
  const curp = parsed.ok ? parsed.value : null;
  if (!parsed.ok) errors.curp = parsed.error;
  const rfc = validateRfc(nfc(input.rfc));
  if (!rfc.ok) errors.rfc = rfc.error;
  if (curp && rfc.ok) {
    const cross = rfcCurpCheck(rfc.value, curp);
    if (cross.errors[0]) errors.rfc = cross.errors[0];
    warnings.push(...cross.warnings);
  }
  if (curp && nombres !== null && paterno !== null && !errors.apellido_materno) {
    warnings.push(...curpNameWarnings(curp, nombres, paterno, materno));
  }
  if (curp) {
    const age = ageOn(curp.fechaNacimiento, today);
    if (age < 18) errors.curp = M.underage;
    else if (age > 120) errors.curp = M.implausibleBirth;
  }

  const d = input.domicilio;
  const calle = text("calle", nfc(d.calle));
  const numeroExterior = text("numero_exterior", nfc(d.numero_exterior));
  const numeroInterior = text("numero_interior", nfc(d.numero_interior), true);
  const colonia = text("colonia", nfc(d.colonia));
  const municipio = text("municipio", nfc(d.municipio));
  const estado = text("estado", nfc(d.estado));
  const cpError = validatePostalCode(nfc(d.codigo_postal));
  if (cpError) errors.codigo_postal = cpError;

  const valid =
    Object.keys(errors).length === 0 &&
    nombres !== null &&
    paterno !== null &&
    curp !== null &&
    rfc.ok &&
    calle !== null &&
    numeroExterior !== null &&
    colonia !== null &&
    municipio !== null &&
    estado !== null;
  const body: DatosPersonalesInput | null = valid
    ? {
        nombres,
        apellido_paterno: paterno,
        apellido_materno: materno,
        curp: curp.curp,
        rfc: rfc.value,
        domicilio: {
          calle,
          numero_exterior: numeroExterior,
          numero_interior: numeroInterior,
          colonia,
          municipio,
          estado,
          codigo_postal: trimWs(nfc(d.codigo_postal)),
        },
      }
    : null;
  return { errors, warnings, curp, body };
}

// -----------------------------------------------------------------------------
// Steps 2-3: images (expediente.rs decodificar_imagen / decodificar_imagenes)
// -----------------------------------------------------------------------------

export const MIN_SELFIE_FRAMES = MIN_SELFIE;
export const MAX_SELFIE_FRAMES = MAX_SELFIE;
export const MIN_IMAGE_BYTES = 1024;
export const MAX_IMAGE_BYTES = 3 * 1024 * 1024;
/** Body limit of POST /kyc/verificar. */
export const MAX_VERIFICATION_BODY_BYTES = 12 * 1024 * 1024;

export type ImageType = "image/jpeg" | "image/png" | "image/webp";

export interface InspectedImage {
  type: ImageType;
  /** Decoded size. */
  bytes: number;
  /** Base64 payload without the data URL header or whitespace (equal payloads = equal bytes). */
  base64: string;
}

const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/** Decoded bytes of a short canonical base64 string. */
function decodeBase64Prefix(s: string): number[] {
  const binary = atob(s);
  return Array.from(binary, (ch) => ch.charCodeAt(0));
}

/**
 * Checks an image as the backend will (base64 or data URL; 1 KB to 3 MB
 * decoded; JPEG, PNG or WebP by its magic bytes) without decoding all of it.
 * `label` prefixes the messages ("INE front", "Selfie frame 2").
 */
export function inspectImage(label: string, input: string): Validated<InspectedImage, "BASE64" | "SIZE" | "TYPE"> {
  const comma = input.indexOf(",");
  const payload = comma >= 0 && input.slice(0, comma).startsWith("data:") ? input.slice(comma + 1) : input;
  const base64 = stripWs(payload);
  const len = base64.length;
  const padding = base64.endsWith("==") ? 2 : base64.endsWith("=") ? 1 : 0;
  // Canonical standard base64 with padding (the backend's base64 engine).
  let canonical = len % 4 === 0 && /^[A-Za-z0-9+/]*={0,2}$/.test(base64);
  if (canonical && padding === 2) canonical = (B64.indexOf(base64.charAt(len - 3)) & 0x0f) === 0;
  if (canonical && padding === 1) canonical = (B64.indexOf(base64.charAt(len - 2)) & 0x03) === 0;
  if (!canonical) return fail("BASE64", M.imageBase64(label));
  const bytes = (len / 4) * 3 - padding;
  if (bytes < MIN_IMAGE_BYTES || bytes > MAX_IMAGE_BYTES) return fail("SIZE", M.imageSize(label));
  const head = decodeBase64Prefix(base64.slice(0, 16));
  const starts = (sig: number[], at = 0) => sig.every((b, i) => head[at + i] === b);
  const ascii = (s: string) => Array.from(s, (c) => c.charCodeAt(0));
  let type: ImageType | null = null;
  if (starts([0xff, 0xd8, 0xff])) type = "image/jpeg";
  else if (starts([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])) type = "image/png";
  else if (bytes > 12 && starts(ascii("RIFF")) && starts(ascii("WEBP"), 8)) type = "image/webp";
  if (type === null) return fail("TYPE", M.imageType(label));
  return { ok: true, value: { type, bytes, base64 } };
}

/** Labels the backend uses for each image field, in English. */
export function imageLabel(field: "frente" | "reverso" | number): string {
  if (field === "frente") return "INE front";
  if (field === "reverso") return "INE back";
  return `Selfie frame ${field + 1}`;
}

/**
 * Every image check of POST /kyc/verificar that does not need the server:
 * 3 to 8 selfie frames, each image valid, all byte-distinct, and the whole
 * body under the 12 MB limit. Returns English messages (empty = OK).
 */
export function checkVerificationImages(images: { frente: string; reverso: string; frames: readonly string[] }): string[] {
  const errors: string[] = [];
  const n = images.frames.length;
  if (n < MIN_SELFIE_FRAMES || n > MAX_SELFIE_FRAMES) errors.push(M.frameCount);
  const all: [string, string][] = [
    [imageLabel("frente"), images.frente],
    [imageLabel("reverso"), images.reverso],
    ...images.frames.map((f, i): [string, string] => [imageLabel(i), f]),
  ];
  const payloads: string[] = [];
  for (const [label, data] of all) {
    const r = inspectImage(label, data);
    if (r.ok) payloads.push(r.value.base64);
    else errors.push(r.error);
  }
  if (payloads.length === all.length && new Set(payloads).size !== payloads.length) errors.push(M.distinctImages);
  // JSON overhead (keys, typed fields) is well under 4 KB.
  const total = all.reduce((sum, [, data]) => sum + data.length, 4096);
  if (total > MAX_VERIFICATION_BODY_BYTES) errors.push(M.bodyTooLarge);
  return errors;
}

// -----------------------------------------------------------------------------
// Sample identities (tests, e2e and demos)
// -----------------------------------------------------------------------------

const SAMPLE_GIVEN: readonly [string, Sexo][] = [
  ["Andrés", "H"],
  ["Beatriz", "M"],
  ["Carlos", "H"],
  ["Daniela", "M"],
  ["Emilio", "H"],
  ["Fernanda", "M"],
  ["Gustavo", "H"],
  ["Hilda", "M"],
  ["Ignacio", "H"],
  ["Josefina", "M"],
];
const SAMPLE_PATERNAL = ["López", "Martínez", "Navarro", "Ortega", "Pérez", "Quintero", "Ramírez", "Salazar", "Torres", "Vázquez"];
const SAMPLE_MATERNAL = ["Aguilar", "Bravo", "Cruz", "Domínguez", "Espinoza", "Flores", "Guerrero", "Herrera", "Ibarra", "Juárez"];
const SAMPLE_FIRST_DAY = Date.UTC(1950, 0, 1);
const SAMPLE_DAYS = 18_262; // 1950-01-01 .. 1999-12-31
const ALNUM = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/** Number of distinct identities `sampleIdentity` produces (seeds are taken modulo this). */
export const SAMPLE_IDENTITY_SPACE = SAMPLE_DAYS * 1000;

export interface SampleIdentity {
  nombres: string;
  apellidoPaterno: string;
  apellidoMaterno: string;
  fechaNacimiento: string;
  sexo: Sexo;
  entidad: string;
  curp: string;
  rfc: string;
  /** Body for POST /kyc/perfil. */
  profile: DatosPersonalesInput;
  /** Credential fields of POST /kyc/verificar (add `frente` and `reverso`). */
  credential: Required<CredentialInput> & { identificador_ciudadano: string };
}

/**
 * A complete, internally consistent identity that passes every check here
 * and in the backend without warnings (so the biometric simulator approves
 * it). Distinct seeds in [0, SAMPLE_IDENTITY_SPACE) give distinct CURPs,
 * RFCs and claves de elector, which the backend requires to be unique across
 * verified and in-review files (IDENTIDAD_EN_USO).
 */
export function sampleIdentity(seed: number, today: string): SampleIdentity {
  const s = nonNegativeInteger(Math.trunc(Math.abs(seed)), "seed") % SAMPLE_IDENTITY_SPACE;
  const nameIdx = s % 100;
  const digit = Math.floor(s / 100) % 10;
  const dayIdx = Math.floor(s / 1000);
  const [nombres, sexo] = SAMPLE_GIVEN[nameIdx % 10] ?? ["Andrés", "H"];
  const apellidoPaterno = SAMPLE_PATERNAL[Math.floor(nameIdx / 10)] ?? "López";
  const apellidoMaterno = SAMPLE_MATERNAL[(nameIdx * 3 + digit) % 10] ?? "Aguilar";
  const fechaNacimiento = new Date(SAMPLE_FIRST_DAY + dayIdx * 86_400_000).toISOString().slice(0, 10);
  const estado = ESTADOS[(nameIdx * 7 + digit) % ESTADOS.length] ?? { code: "DF", name: "Ciudad de México", inegi: 9 };
  const person = { nombres, apellidoPaterno, apellidoMaterno };
  const curp = buildCurp({ ...person, fechaNacimiento, sexo, entidad: estado.code, homoclave: digit });
  // RFC and clave homoclaves encode (digit, name): unique even when two names share letters.
  const tag = digit * 100 + nameIdx;
  const rfc = buildRfcFromCurp(curp, (ALNUM[Math.floor(tag / 36)] ?? "0") + (ALNUM[tag % 36] ?? "0"));
  const claveElector = buildClaveElector({ ...person, fechaNacimiento, entidadInegi: estado.inegi, sexo, homoclave: tag });
  const year = splitIsoDate(today, "today").year;
  const emision = year - 2 - (s % 5);
  return {
    ...person,
    fechaNacimiento,
    sexo,
    entidad: estado.code,
    curp,
    rfc,
    profile: {
      nombres,
      apellido_paterno: apellidoPaterno,
      apellido_materno: apellidoMaterno,
      curp,
      rfc,
      domicilio: {
        calle: "Avenida Paseo de la Reforma",
        numero_exterior: String(100 + (s % 900)),
        numero_interior: null,
        colonia: "Juárez",
        municipio: "Cuauhtémoc",
        estado: "Ciudad de México",
        codigo_postal: "06600",
      },
    },
    credential: {
      clave_elector: claveElector,
      cic: String(s % 1_000_000_000).padStart(9, "0"),
      identificador_ciudadano: String((s * 7919 + 104_729) % 1_000_000_000).padStart(9, "0"),
      emision,
      vigencia: emision + 10,
    },
  };
}

// -----------------------------------------------------------------------------
// Backend messages in English
// -----------------------------------------------------------------------------

/** Field names the backend puts in its messages (texto_corto, nombre). */
const BACKEND_FIELDS: Record<string, string> = {
  nombres: PROFILE_LABELS.nombres,
  apellido_paterno: PROFILE_LABELS.apellido_paterno,
  apellido_materno: PROFILE_LABELS.apellido_materno,
  calle: PROFILE_LABELS.calle,
  numero_exterior: PROFILE_LABELS.numero_exterior,
  numero_interior: PROFILE_LABELS.numero_interior,
  colonia: PROFILE_LABELS.colonia,
  municipio: PROFILE_LABELS.municipio,
  estado: PROFILE_LABELS.estado,
};

function backendImageLabel(field: string): string {
  if (field === "ine.frente") return imageLabel("frente");
  if (field === "ine.reverso") return imageLabel("reverso");
  const frame = /^selfie\.frames\[(\d+)\]$/.exec(field);
  return frame ? imageLabel(Number(frame[1])) : field;
}

/** Reasons the provider gives after "prueba de vida fallida:" / "credencial no válida:". */
const PROVIDER_REASONS: Record<string, string> = {
  "no se detectó un rostro vivo frente a la cámara": "no live face was detected in front of the camera",
  "los cuadros de la cámara son idénticos (posible foto o repetición)":
    "the camera frames are identical (possible photo or replay)",
  "la credencial no está vigente en la lista nominal": "the credential is not current in the voter registry",
  "anverso y reverso son la misma imagen": "front and back are the same image",
  "sin detalle del proveedor": "no details from the provider",
  "foto de una pantalla": "photo of a screen",
};

const EXACT: Record<string, string> = {
  // curp.rs
  "la CURP debe tener 18 caracteres": M.curpLength,
  "la CURP no tiene la estructura oficial (4 letras, fecha AAMMDD, sexo, entidad, 3 consonantes, homoclave y dígito)":
    M.curpStructure,
  "la fecha de nacimiento de la CURP no existe": M.curpDate,
  "el dígito verificador de la CURP no corresponde": M.curpCheckDigit,
  // rfc.rs
  "el RFC de persona física tiene 13 caracteres": M.rfcLength,
  "el RFC no tiene la estructura oficial (4 letras, fecha AAMMDD y homoclave)": M.rfcStructure,
  "la fecha del RFC no existe": M.rfcDate,
  "el dígito verificador del RFC no corresponde": M.rfcCheckDigit,
  "el RFC genérico no identifica a una persona": M.rfcGeneric,
  "RFC inválido": M.rfcInvalid,
  "la fecha de nacimiento del RFC no coincide con la de la CURP": M.rfcDateMismatch,
  "las letras del RFC no coinciden con las de la CURP": M.rfcLettersMismatch,
  // ine.rs: typed credential
  "la clave de elector tiene 18 caracteres: 6 letras, fecha AAMMDD, entidad, sexo y 3 dígitos": M.claveFormat,
  "la fecha de la clave de elector no existe": M.claveDate,
  "el CIC de la credencial tiene 9 dígitos": M.cic,
  "el identificador del ciudadano tiene 9 o 13 dígitos": M.citizenId,
  "la fecha de nacimiento de la clave de elector no coincide con la de la CURP": M.claveDateMismatch,
  "el sexo de la clave de elector no coincide con el de la CURP": M.claveSexMismatch,
  "la entidad de nacimiento de la clave de elector no coincide con la de la CURP": M.claveEntityMismatch,
  "la vigencia de una credencial es de hasta 10 años desde su emisión": M.validitySpan,
  // ine.rs: what the provider read from the credential
  "la CURP de la credencial no es la del expediente": "The CURP on the credential is not the one in your file.",
  "la clave de elector de la credencial no es la capturada": "The voter key on the credential is not the one you entered.",
  "la fecha de nacimiento de la credencial no coincide": "The birth date on the credential does not match.",
  "el sexo de la credencial no coincide": "The sex on the credential does not match.",
  "el CIC de la credencial no es el capturado": "The CIC on the credential is not the one you entered.",
  "la emisión o la vigencia de la credencial no son las capturadas":
    "The issue or expiry year on the credential is not the one you entered.",
  "la credencial está vencida": "The credential has expired.",
  "la MRZ de una credencial TD1 tiene 3 líneas de 30 caracteres (A-Z, 0-9, <)":
    "The machine-readable zone (MRZ) of the credential could not be read.",
  "la MRZ no es de una identificación mexicana": "The machine-readable zone is not from a Mexican ID.",
  "el número de documento de la MRZ no es el CIC": "The document number in the machine-readable zone is not the CIC.",
  "la fecha de nacimiento de la MRZ no coincide": "The birth date in the machine-readable zone does not match.",
  "el sexo de la MRZ no coincide": "The sex in the machine-readable zone does not match.",
  "el vencimiento de la MRZ no coincide con la vigencia": "The expiry in the machine-readable zone does not match the credential.",
  "el nombre de la MRZ no coincide con el capturado": "The name in the machine-readable zone does not match the one you entered.",
  "el nombre impreso en la credencial no coincide con el capturado":
    "The name printed on the credential does not match the one you entered.",
  "el código postal de la credencial no es el del domicilio declarado":
    "The postal code on the credential is not the one of the address you declared.",
  "el identificador del ciudadano no coincide": "The citizen identifier does not match.",
  // verificacion.rs
  "las imágenes analizadas por el proveedor no son las que envió el cliente":
    "The images analyzed by the provider are not the ones you sent.",
  "el proveedor no entregó la lectura de la credencial": "The provider did not return a reading of the credential.",
  "score de similitud fuera de rango": "The face similarity score is out of range.",
  "no se encontró un rostro comparable entre la selfie y la credencial":
    "No comparable face was found between the selfie and the credential.",
  // expediente.rs
  "el código postal tiene 5 dígitos": M.postalCode,
  "la fecha de nacimiento de la CURP no es plausible": M.implausibleBirth,
  "cada imagen (anverso, reverso y cuadros de la selfie) debe ser distinta": M.distinctImages,
  [`la prueba de vida requiere de ${MIN_SELFIE} a ${MAX_SELFIE} cuadros de la cámara`]: M.frameCount,
  "la apertura remota es solo para mayores de edad": M.underage,
  "la identificación biométrica remota es para personas físicas; las personas morales se dan de alta en sucursal":
    "Remote biometric verification is only for individuals; companies are onboarded at a branch.",
  "el expediente ya está en revisión; espera el resultado": "Your file is already under review; please wait for the result.",
  "el expediente ya está verificado": "Your identity is already verified.",
  "el expediente fue rechazado; solo cumplimiento puede reabrirlo":
    "Your file was rejected; only the compliance team can reopen it.",
  "el expediente no se pudo actualizar": "Your file could not be updated.",
  "para verificar tu identidad necesitamos tu consentimiento expreso para tratar tus datos biométricos":
    "To verify your identity we need your explicit consent to process your biometric data.",
  "primero captura tus datos personales y tu domicilio": "First save your personal details and address.",
  "el expediente no tiene completos sus datos de identidad": "Your file does not have complete identity details yet.",
  "expediente KYC no encontrado": "Identity file not found.",
  // error.rs (unique indexes of kyc_perfiles)
  "esa CURP, RFC o clave de elector ya está en un expediente verificado o en revisión":
    "That CURP, RFC or voter key is already in a verified or in-review file.",
};

const PATTERNS: [RegExp, (m: RegExpExecArray) => string][] = [
  [/^la entidad de nacimiento (.+) no existe$/, (m) => M.curpEntity(m[1] ?? "")],
  [
    /^las letras de la CURP \(([A-Z0-9Ñ]{4})…([A-Z0-9Ñ]{3})\) no corresponden al nombre capturado \(se esperaban ([A-Z0-9Ñ]{4})…([A-Z0-9Ñ]{3})\)$/u,
    (m) => M.curpLetters(`${m[1]}${m[2]}`, `${m[3]}${m[4]}`),
  ],
  [/^año de emisión (-?\d+) inválido$/, (m) => M.issueYear(m[1] ?? "")],
  [/^la credencial venció en (-?\d+)$/, (m) => M.expired(m[1] ?? "")],
  [
    /^(nombres|apellido_paterno|apellido_materno|calle|numero_exterior|numero_interior|colonia|municipio|estado) es obligatorio$/,
    (m) => M.required(BACKEND_FIELDS[m[1] ?? ""] ?? ""),
  ],
  [
    /^(nombres|apellido_paterno|apellido_materno|calle|numero_exterior|numero_interior|colonia|municipio|estado) admite como máximo (\d+) caracteres$/,
    (m) => M.tooLong(BACKEND_FIELDS[m[1] ?? ""] ?? "", m[2] ?? ""),
  ],
  [
    /^(nombres|apellido_paterno|apellido_materno|calle|numero_exterior|numero_interior|colonia|municipio|estado) contiene caracteres no válidos$/,
    (m) => M.invalidChars(BACKEND_FIELDS[m[1] ?? ""] ?? ""),
  ],
  [
    /^(nombres|apellido_paterno|apellido_materno) debe contener letras$/,
    (m) => M.needsLetters(BACKEND_FIELDS[m[1] ?? ""] ?? ""),
  ],
  [/^(ine\.frente|ine\.reverso|selfie\.frames\[\d+\]): la imagen debe venir en base64$/, (m) => M.imageBase64(backendImageLabel(m[1] ?? ""))],
  [/^(ine\.frente|ine\.reverso|selfie\.frames\[\d+\]): la imagen debe pesar entre 1 KB y 3 MB$/, (m) => M.imageSize(backendImageLabel(m[1] ?? ""))],
  [/^(ine\.frente|ine\.reverso|selfie\.frames\[\d+\]): se admite JPEG, PNG o WebP$/, (m) => M.imageType(backendImageLabel(m[1] ?? ""))],
  [
    /^dígito de control inválido en la MRZ \((.+)\)$/,
    () => "A check digit in the machine-readable zone (MRZ) of the credential is invalid.",
  ],
  [
    /^prueba de vida fallida: (.*)$/s,
    (m) => `Liveness check failed: ${PROVIDER_REASONS[m[1] ?? ""] ?? m[1]}.`,
  ],
  [/^credencial no válida: (.*)$/s, (m) => `The credential is not valid: ${PROVIDER_REASONS[m[1] ?? ""] ?? m[1]}.`],
  [
    /^similitud facial ([\d.]+) por debajo del mínimo aceptable ([\d.]+)$/,
    (m) => `Face similarity ${m[1]} is below the minimum acceptable ${m[2]}.`,
  ],
  [
    /^similitud facial ([\d.]+) menor que ([\d.]+): requiere revisión humana$/,
    (m) => `Face similarity ${m[1]} is below ${m[2]}: a person will review it.`,
  ],
  [
    /^el expediente \S+ agotó sus (\d+) intentos$/,
    (m) => `You have used all ${m[1]} verification attempts. Contact support to reopen your file.`,
  ],
  [
    /^la identidad del expediente \S+ no cambia en estado (\S+)$/,
    (m) => `Your identity details cannot change while your file is ${(ESTADO_KYC_LABELS[m[1] as EstadoKyc] ?? m[1] ?? "").toLowerCase()}.`,
  ],
];

function translateOne(message: string): string | null {
  const exact = EXACT[message];
  if (exact !== undefined) return exact;
  for (const [re, english] of PATTERNS) {
    const m = re.exec(message);
    if (m) return english(m);
  }
  const exhausted = /^intentos agotados: (.*)$/s.exec(message);
  if (exhausted) {
    const reason = exhausted[1] ?? "";
    return `No verification attempts left. ${reason ? (translateOne(reason) ?? reason) : ""}`.trim();
  }
  return null;
}

/**
 * English version of a Spanish message from the KYC backend (validation
 * errors, `advertencias`, `detalles[].mensaje`, `motivo_rechazo`). Messages
 * the backend joins with "; " are translated part by part. Returns null when
 * nothing is recognized (e.g. an operator's free-text reason).
 */
export function kycMessageInEnglish(message: string): string | null {
  const parts = message.split("; ").map((p) => p.trim()).filter(Boolean);
  const translated = parts.map(translateOne);
  if (translated.every((t) => t === null)) return null;
  return translated.map((t, i) => t ?? parts[i]).join(" ");
}

/** `kycMessageInEnglish`, falling back to the original text. */
export function describeKycMessage(message: string): string {
  return kycMessageInEnglish(message) ?? message;
}
