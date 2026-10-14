/**
 * Mexican identity rules of the onboarding forms. The vectors are the ones of
 * the backend unit tests (backend/src/kyc/curp.rs, rfc.rs, ine.rs, texto.rs,
 * expediente.rs), so both sides give the customer the same verdict.
 *
 * Run with: npm test   (Node >= 22.18 strips TypeScript types natively)
 */
import assert from "node:assert/strict";
import { test } from "node:test";

import type { DatosPersonalesInput } from "./api/types.ts";
import {
  ENTIDADES,
  ESTADOS,
  KYC_MESSAGES,
  SAMPLE_IDENTITY_SPACE,
  ageOn,
  buildClaveElector,
  buildCurp,
  buildRfcFromCurp,
  checkProfile,
  checkVerificationImages,
  cleanIdInput,
  curpCheckDigit,
  curpNameWarnings,
  deriveCurpLetters,
  describeKycMessage,
  entidadName,
  inegiCode,
  inspectImage,
  kycMessageInEnglish,
  nameWords,
  normalizeName,
  parseClaveElector,
  parseCurp,
  rfcCheckDigit,
  rfcCurpCheck,
  sameName,
  sampleIdentity,
  shortText,
  todayInMexicoCity,
  trimWs,
  validateCic,
  validateCitizenId,
  validateCredential,
  validatePostalCode,
  validateRfc,
  type CredentialInput,
  type ParsedCurp,
} from "./kyc.ts";

/** "Today" of the backend tests (NaiveDate 2026-10-13). */
const TODAY = "2026-10-13";
const REAL_CURPS = ["LOOA531113HTCPBN07", "HEGG560427MVZRRL04", "MAHJ280603MSPRRV09"];

function curp(value: string): ParsedCurp {
  const r = parseCurp(value);
  assert.ok(r.ok, `${value} should parse: ${r.ok ? "" : r.error}`);
  return r.value;
}

/** A CURP with the right check digit for its first 17 characters. */
function withCheckDigit(first17: string): string {
  const digit = curpCheckDigit(first17);
  assert.ok(digit !== null, first17);
  return first17 + digit;
}

function curpError(value: string): string {
  const r = parseCurp(value);
  assert.ok(!r.ok, `${value} should be rejected`);
  return r.code;
}

// -----------------------------------------------------------------------------
// Catalog and text
// -----------------------------------------------------------------------------

test("RENAPO catalog: 32 states plus NE, with their INEGI codes", () => {
  assert.equal(ENTIDADES.length, 33);
  assert.equal(new Set(ENTIDADES.map((e) => e.code)).size, 33);
  assert.equal(ESTADOS.length, 32);
  assert.deepEqual(
    ESTADOS.map((e) => e.inegi),
    Array.from({ length: 32 }, (_, i) => i + 1),
  );
  assert.equal(entidadName("DF"), "Ciudad de México");
  assert.equal(entidadName("NE"), "Born abroad");
  assert.equal(entidadName("ZZ"), null);
  assert.equal(entidadName(null), null);
  assert.equal(inegiCode("TC"), 27);
  assert.equal(inegiCode("NE"), null);
  assert.equal(inegiCode("ZZ"), null);
});

test("names are normalized like texto.rs", () => {
  assert.equal(normalizeName("  José-María  de la O'Neill "), "JOSE MARIA DE LA O NEILL");
  assert.equal(normalizeName("Muñoz Güemes"), "MUÑOZ GUEMES");
  assert.deepEqual(nameWords("Ma. Guadalupe"), ["MA", "GUADALUPE"]);
  assert.ok(sameName("Peña", "PENA", false));
  assert.ok(!sameName("Peña", "PENA", true));
  // A decomposed accent (NFD) reads like the precomposed one.
  assert.equal(normalizeName("López"), "LOPEZ");
});

test("short text fields are trimmed, bounded and free of control characters", () => {
  assert.deepEqual(shortText("Street", "  Av.  Reforma ", 20), { ok: true, value: "Av. Reforma" });
  assert.deepEqual(shortText("Street", "   ", 10), { ok: false, code: "REQUIRED", error: "Street is required." });
  assert.equal(shortText("Street", "abc\u0000", 10).ok, false);
  assert.equal(shortText("Street", "a\tb", 10).ok, false);
  assert.deepEqual(shortText("Street", "abcdefghijk", 10), {
    ok: false,
    code: "TOO_LONG",
    error: "Street can have at most 10 characters.",
  });
  // Length counts characters, not UTF-16 units.
  assert.equal(shortText("Street", "ñ".repeat(10), 10).ok, true);
});

test("whitespace is Unicode White_Space, like Rust's trim (not JavaScript's)", () => {
  // U+0085 (NEL) is whitespace for Rust; U+FEFF (BOM) is not.
  assert.equal(trimWs("\u0085 \u3000LOOA\u00a0\n"), "LOOA");
  assert.equal(trimWs("\ufeffLOOA"), "\ufeffLOOA");
  assert.ok(parseCurp("\u0085LOOA531113HTCPBN07\u2003").ok);
  assert.equal(curpError("\ufeffLOOA531113HTCPBN07"), "LENGTH");
  assert.ok(validateRfc("\u0085GODE561231GR8").ok);
  assert.deepEqual(shortText("Street", "\u0085Av.\u3000 Reforma\u0085", 20), { ok: true, value: "Av. Reforma" });
  // Inside the text NEL is still a control character (Rust rejects it too).
  assert.equal(shortText("Street", "Av.\u0085Reforma", 20).ok, false);
  assert.deepEqual(shortText("Street", "\u0085", 20), { ok: false, code: "REQUIRED", error: "Street is required." });
  assert.equal(validatePostalCode("\u008506600"), null);
  const image = new Uint8Array(2048).fill(7);
  image.set([0xff, 0xd8, 0xff, 0xe0]);
  const b64 = Buffer.from(image).toString("base64");
  assert.ok(inspectImage("x", b64).ok);
  assert.ok(inspectImage("x", `${b64.slice(0, 100)}\u0085${b64.slice(100)}`).ok);
  assert.equal(inspectImage("x", `${b64.slice(0, 100)}\ufeff${b64.slice(100)}`).ok, false);
});

test("ID inputs are upper-cased without spaces", () => {
  assert.equal(cleanIdInput(" looa 531113 htcpbn07 "), "LOOA531113HTCPBN07");
});

// -----------------------------------------------------------------------------
// CURP
// -----------------------------------------------------------------------------

test("CURP check digit of real CURPs", () => {
  for (const c of REAL_CURPS) {
    assert.equal(curpCheckDigit(c.slice(0, 17)), c.slice(17), c);
    assert.ok(parseCurp(c).ok, c);
  }
  assert.equal(curpCheckDigit("LOOA531113HTCPBN"), null);
  assert.equal(curpCheckDigit("looa531113htcpbn0"), null);
});

test("CURP: structure, date, state and check digit errors", () => {
  assert.equal(curpError("LOOA531113HTCPBN08"), "CHECK_DIGIT");
  assert.equal(curpError("LOOA531113HTCPBN0"), "LENGTH");
  assert.equal(curpError("LOOA531113ZTCPBN07"), "STRUCTURE");
  assert.equal(curpError("LOOA531313HTCPBN07"), "DATE");
  assert.equal(curpError("LOOA531113HZZPBN07"), "ENTITY");
  assert.equal(curpError("LOOA531113HTCPBN07X"), "LENGTH");
  assert.equal(curpError("LOÑA531113HTCPBN07"), "LENGTH");
  assert.equal(curpError(""), "LENGTH");
  assert.equal(curpError("1OOA531113HTCPBN07"), "STRUCTURE");
  // February 29, 1900 does not exist (1900 was not a leap year)...
  assert.equal(curpError(withCheckDigit("AAAA000229HDFAAA0")), "DATE");
  // ...but February 29, 2000 does (letter in position 17: born since 2000).
  assert.equal(curp(withCheckDigit("AAAA000229HDFAAAA")).fechaNacimiento, "2000-02-29");
  // Structure and check digit fine, month 00.
  assert.equal(curpError(withCheckDigit("AAAA530013HDFAAA0")), "DATE");
});

test("CURP errors read in English", () => {
  const r = parseCurp("LOOA531113HZZPBN07");
  assert.ok(!r.ok);
  assert.equal(r.error, "Birth state ZZ in the CURP does not exist.");
  const d = parseCurp("LOOA531113HTCPBN08");
  assert.ok(!d.ok);
  assert.equal(d.error, KYC_MESSAGES.curpCheckDigit);
});

test("CURP gives birth date, sex and birth state", () => {
  assert.deepEqual(curp(" looa531113htcpbn07 "), {
    curp: "LOOA531113HTCPBN07",
    fechaNacimiento: "1953-11-13",
    sexo: "H",
    entidad: "TC",
  });
  assert.deepEqual(
    { ...curp("HEGG560427MVZRRL04") },
    { curp: "HEGG560427MVZRRL04", fechaNacimiento: "1956-04-27", sexo: "M", entidad: "VZ" },
  );
  assert.equal(curp("MAHJ280603MSPRRV09").fechaNacimiento, "1928-06-03");
  // Letter homoclave: born since 2000.
  assert.equal(curp(withCheckDigit("GAXP050101MDFRXRA")).fechaNacimiento, "2005-01-01");
  assert.equal(curp(withCheckDigit("GAXP050101XNERXRA")).entidad, "NE");
});

test("CURP letters derived from the name (RENAPO rules)", () => {
  assert.equal(deriveCurpLetters("Andrés Manuel", "López", "Obrador"), "LOOAPBN");
  assert.equal(deriveCurpLetters("Gloria", "Hernández", "García"), "HEGGRRL");
  // MARÍA is skipped in compound given names; particles are dropped.
  assert.equal(deriveCurpLetters("María Guadalupe", "Pérez", "de la Cruz"), "PECGRRD");
  // No maternal surname: X.
  assert.equal(deriveCurpLetters("Ana", "Ruiz", null), "RUXAZXN");
  assert.equal(deriveCurpLetters("Ana", "Ruiz"), "RUXAZXN");
  assert.equal(deriveCurpLetters("Ana", "Ruiz", ""), "RUXAZXN");
  // Ñ counts as X.
  assert.equal(deriveCurpLetters("Íñigo", "Muñoz", "Ñuñez"), "MUXIXXX");
  // Offensive word: BUEY -> BXEY.
  assert.equal(deriveCurpLetters("Yolanda", "Buendía", "Esparza").slice(0, 4), "BXEY");
  // JOSÉ alone is kept; JOSÉ + another name uses the second one.
  assert.equal(deriveCurpLetters("José", "Ruiz", null).slice(3, 4), "J");
  assert.equal(deriveCurpLetters("José Luis", "Ruiz", null).slice(3, 4), "L");
  assert.equal(deriveCurpLetters("Ludwig", "Von Mises", null), "MIXLSXD");
});

test("the real CURPs match their holders' names", () => {
  assert.deepEqual(curpNameWarnings("LOOA531113HTCPBN07", "Andrés Manuel", "López", "Obrador"), []);
  assert.deepEqual(curpNameWarnings(curp("HEGG560427MVZRRL04"), "Gloria", "Hernández", "García"), []);
});

test("CURP letters that do not match the typed name are a warning", () => {
  const c = buildCurp({
    nombres: "Ludwig",
    apellidoPaterno: "Von Mises",
    fechaNacimiento: "1990-07-15",
    sexo: "H",
    entidad: "DF",
    homoclave: 3,
  });
  const parsed = curp(c);
  assert.equal(parsed.fechaNacimiento, "1990-07-15");
  assert.deepEqual(curpNameWarnings(parsed, "Ludwig", "Von Mises", null), []);
  const warnings = curpNameWarnings(parsed, "Murray", "Rothbard", null);
  assert.equal(warnings.length, 1);
  assert.equal(
    warnings[0],
    "The CURP letters (MIXL…SXD) do not match the name you entered (expected ROXM…TXR).",
  );
});

test("built CURPs are valid, before and since 2000", () => {
  const before = buildCurp({
    nombres: "Andrés Manuel",
    apellidoPaterno: "López",
    apellidoMaterno: "Obrador",
    fechaNacimiento: "1953-11-13",
    sexo: "H",
    entidad: "TC",
    homoclave: 0,
  });
  // Same as the real one, except for RENAPO's homoclave.
  assert.equal(before.slice(0, 16), "LOOA531113HTCPBN");
  assert.equal(before.charAt(16), "0");
  assert.equal(before, "LOOA531113HTCPBN07");
  const since = buildCurp({
    nombres: "Ana",
    apellidoPaterno: "Ruiz",
    fechaNacimiento: "2010-01-01",
    sexo: "M",
    entidad: "DF",
    homoclave: 27,
  });
  assert.equal(since.charAt(16), "B");
  assert.deepEqual(curp(since), { curp: since, fechaNacimiento: "2010-01-01", sexo: "M", entidad: "DF" });
  assert.throws(() => buildCurp({ nombres: "A", apellidoPaterno: "B", fechaNacimiento: "1990-02-30", sexo: "H", entidad: "DF", homoclave: 1 }), RangeError);
  assert.throws(() => buildCurp({ nombres: "A", apellidoPaterno: "B", fechaNacimiento: "1990-02-01", sexo: "H", entidad: "XX", homoclave: 1 }), RangeError);
  assert.throws(() => buildCurp({ nombres: "A", apellidoPaterno: "B", fechaNacimiento: "1890-02-01", sexo: "H", entidad: "DF", homoclave: 1 }), RangeError);
  assert.throws(() => buildCurp({ nombres: "A", apellidoPaterno: "B", fechaNacimiento: "1990-02-01", sexo: "H", entidad: "DF", homoclave: -1 }), RangeError);
});

// -----------------------------------------------------------------------------
// RFC
// -----------------------------------------------------------------------------

test("RFC check digit of real RFCs (companies and individuals)", () => {
  for (const rfc of ["SAT970701NN3", "BBA830831LJ2", "CFE370814QI0", "PME380607P35", "TME840315KT6", "GODE561231GR8"]) {
    assert.equal(rfcCheckDigit(rfc.slice(0, -1)), rfc.slice(-1), rfc);
  }
  assert.equal(rfcCheckDigit("GODE561231"), null);
  assert.equal(rfcCheckDigit("GODE561231G$"), null);
});

test("RFC of an individual", () => {
  assert.deepEqual(validateRfc(" gode561231gr8 "), { ok: true, value: "GODE561231GR8" });
  const code = (rfc: string) => {
    const r = validateRfc(rfc);
    return r.ok ? "OK" : r.code;
  };
  assert.equal(code("GODE561231GR9"), "CHECK_DIGIT");
  assert.equal(code("SAT970701NN3"), "LENGTH");
  assert.equal(code("GODE561331GR8"), "DATE");
  assert.equal(code("GOD1561231GR8"), "STRUCTURE");
  assert.equal(code("XEXX010101000"), "GENERIC");
  assert.equal(code("XAXX010101000"), "GENERIC");
  assert.equal(code("GODE561231GRB"), "STRUCTURE");
  // February 29 exists in 2000 (the RFC does not tell the century).
  const leap = buildRfcFromCurp("AAAA000229", "AB");
  assert.equal(code(leap), "OK");
  const r = validateRfc("GODE561231GR9");
  assert.ok(!r.ok);
  assert.equal(r.error, "The RFC check digit does not match.");
});

test("RFC cross-checked with the CURP", () => {
  const c = curp("LOOA531113HTCPBN07");
  const rfc = buildRfcFromCurp(c.curp, "XY");
  assert.equal(rfc.slice(0, 12), "LOOA531113XY");
  assert.ok(validateRfc(rfc).ok);
  assert.deepEqual(rfcCurpCheck(rfc, c), { errors: [], warnings: [] });
  assert.deepEqual(rfcCurpCheck(rfc, "LOOA531113HTCPBN07"), { errors: [], warnings: [] });
  const otherDate = buildRfcFromCurp("LOOA531114HTCPBN07", "XY");
  assert.deepEqual(rfcCurpCheck(otherDate, c), {
    errors: ["The birth date in the RFC does not match the one in the CURP."],
    warnings: [],
  });
  // SAT has its own list of offensive words: different letters only warn.
  const otherLetters = buildRfcFromCurp("LXOA531113HTCPBN07", "XY");
  assert.deepEqual(rfcCurpCheck(otherLetters, c), {
    errors: [],
    warnings: ["The RFC letters do not match the CURP's."],
  });
  assert.deepEqual(rfcCurpCheck("LOOA531113", c).errors, ["Invalid RFC."]);
  assert.throws(() => buildRfcFromCurp("LOOA531113HTCPBN07", "X"), RangeError);
  assert.throws(() => buildRfcFromCurp("LOOA53", "XY"), RangeError);
});

// -----------------------------------------------------------------------------
// Voting credential (INE)
// -----------------------------------------------------------------------------

const AMLO = { nombres: "Andrés Manuel", apellidoPaterno: "López", apellidoMaterno: "Obrador" };

function credential(overrides: Partial<CredentialInput> = {}): CredentialInput {
  return {
    clave_elector: buildClaveElector({ ...AMLO, fechaNacimiento: "1953-11-13", entidadInegi: 27, sexo: "H", homoclave: 100 }),
    cic: "123456789",
    identificador_ciudadano: "987654321",
    emision: 2020,
    vigencia: 2030,
    ...overrides,
  };
}

test("clave de elector", () => {
  const clave = buildClaveElector({ ...AMLO, fechaNacimiento: "1953-11-13", entidadInegi: 27, sexo: "H", homoclave: 100 });
  assert.equal(clave, "LPOBAN53111327H100");
  assert.deepEqual(parseClaveElector(` ${clave.toLowerCase()} `), {
    ok: true,
    value: { claveElector: clave, aammdd: "531113", entidad: 27, sexo: "H" },
  });
  const code = (v: string) => {
    const r = parseClaveElector(v);
    return r.ok ? "OK" : r.code;
  };
  assert.equal(code("LPOBAN531113"), "FORMAT");
  assert.equal(code("LPOBAN53131327H100"), "DATE");
  // State 00 does not exist.
  assert.equal(code("LPOBAN53111300H100"), "FORMAT");
  assert.equal(code("LPOBAN53111327Z100"), "FORMAT");
  assert.equal(code("LPOBA153111327H100"), "FORMAT");
  // Without a maternal surname, and with Ñ (as X).
  assert.equal(
    buildClaveElector({ nombres: "Íñigo", apellidoPaterno: "Muñoz", fechaNacimiento: "2001-02-03", entidadInegi: 9, sexo: "H", homoclave: 7 }),
    "MXXXIX01020309H007",
  );
  assert.throws(() => buildClaveElector({ ...AMLO, fechaNacimiento: "1953-11-13", entidadInegi: 0, sexo: "H", homoclave: 1 }), RangeError);
});

test("CIC, citizen identifier and postal code", () => {
  assert.equal(validateCic("123456789"), null);
  assert.equal(validateCic("12345678"), "The credential's CIC has 9 digits.");
  assert.equal(validateCic("12345678A"), KYC_MESSAGES.cic);
  assert.equal(validateCitizenId("987654321"), null);
  assert.equal(validateCitizenId("9876543210123"), null);
  assert.equal(validateCitizenId("98765432101"), KYC_MESSAGES.citizenId);
  assert.equal(validatePostalCode("86035"), null);
  assert.equal(validatePostalCode(" 06600 "), null);
  assert.equal(validatePostalCode("8603"), "The postal code has 5 digits.");
  assert.equal(validatePostalCode("8603A"), KYC_MESSAGES.postalCode);
});

test("credential against the CURP (ine::validar_credencial)", () => {
  const c = curp("LOOA531113HTCPBN07");
  assert.deepEqual(validateCredential(credential(), c, TODAY), { errors: [], warnings: [], fieldErrors: {} });
  // Expired (and the span of 10 years is fine): exactly one error.
  const expired = validateCredential(credential({ emision: 2014, vigencia: 2024 }), c, TODAY);
  assert.deepEqual(expired.errors, ["The credential expired in 2024."]);
  assert.deepEqual(expired.fieldErrors, { vigencia: "The credential expired in 2024." });
  // Someone else's credential (other birth date).
  const other = buildClaveElector({ nombres: "A", apellidoPaterno: "B", fechaNacimiento: "1953-11-14", entidadInegi: 27, sexo: "H", homoclave: 1 });
  assert.deepEqual(validateCredential(credential({ clave_elector: other }), c, TODAY).errors, [
    "The birth date in the voter key does not match the one in the CURP.",
  ]);
  // Another birth state only warns.
  const otherState = buildClaveElector({ nombres: "A", apellidoPaterno: "B", fechaNacimiento: "1953-11-13", entidadInegi: 9, sexo: "H", homoclave: 1 });
  assert.deepEqual(validateCredential(credential({ clave_elector: otherState }), c, TODAY), {
    errors: [],
    warnings: ["The birth state in the voter key does not match the one in the CURP."],
    fieldErrors: {},
  });
  // Another sex is an error; X on either side is not.
  const female = buildClaveElector({ ...AMLO, fechaNacimiento: "1953-11-13", entidadInegi: 27, sexo: "M", homoclave: 1 });
  assert.deepEqual(validateCredential(credential({ clave_elector: female }), c, TODAY).errors, [
    "The sex in the voter key does not match the one in the CURP.",
  ]);
  const nonBinary = buildClaveElector({ ...AMLO, fechaNacimiento: "1953-11-13", entidadInegi: 27, sexo: "X", homoclave: 1 });
  assert.deepEqual(validateCredential(credential({ clave_elector: nonBinary }), c, TODAY).errors, []);
});

test("credential years, CIC and identifier", () => {
  const c = curp("LOOA531113HTCPBN07");
  const errors = (overrides: Partial<CredentialInput>, today = TODAY) => validateCredential(credential(overrides), c, today).errors;
  // Valid through December 31 of the expiry year.
  assert.deepEqual(errors({ emision: 2016, vigencia: 2026 }, "2026-12-31"), []);
  assert.deepEqual(errors({ emision: 2016, vigencia: 2026 }, "2027-01-01"), ["The credential expired in 2026."]);
  assert.deepEqual(errors({ emision: 2019, vigencia: 2030 }), ["A voting credential is valid for up to 10 years from its issue year."]);
  assert.deepEqual(errors({ emision: 2027, vigencia: 2030 }), ["Issue year 2027 is not valid."]);
  assert.deepEqual(errors({ emision: 1989, vigencia: 1999 }), ["Issue year 1989 is not valid.", "The credential expired in 1999."]);
  assert.deepEqual(errors({ emision: 2026, vigencia: 2025 }), [
    "A voting credential is valid for up to 10 years from its issue year.",
    "The credential expired in 2025.",
  ]);
  assert.deepEqual(errors({ cic: " 123456789 " }), []);
  assert.deepEqual(errors({ cic: "1234" }), ["The credential's CIC has 9 digits."]);
  assert.deepEqual(errors({ identificador_ciudadano: "" }), []);
  assert.deepEqual(errors({ identificador_ciudadano: null }), []);
  assert.deepEqual(errors({ identificador_ciudadano: "12" }), ["The citizen identifier has 9 or 13 digits."]);
  const check = validateCredential(credential({ clave_elector: "LPOBAN531113", cic: "1" }), c, TODAY);
  assert.deepEqual(check.fieldErrors, {
    clave_elector: KYC_MESSAGES.claveFormat,
    cic: KYC_MESSAGES.cic,
  });
});

// -----------------------------------------------------------------------------
// Step 1 (expediente.rs validar_datos)
// -----------------------------------------------------------------------------

function profile(overrides: Partial<DatosPersonalesInput> = {}, domicilio: Partial<DatosPersonalesInput["domicilio"]> = {}): DatosPersonalesInput {
  return {
    nombres: " Andrés  Manuel ",
    apellido_paterno: "López",
    apellido_materno: "Obrador",
    curp: "looa531113htcpbn07",
    rfc: buildRfcFromCurp("LOOA531113HTCPBN07", "AB"),
    domicilio: {
      calle: "Av. Paseo Tabasco",
      numero_exterior: "1401",
      numero_interior: " ",
      colonia: "Tabasco 2000",
      municipio: "Centro",
      estado: "Tabasco",
      codigo_postal: "86035",
      ...domicilio,
    },
    ...overrides,
  };
}

test("step 1 validates and normalizes the personal data", () => {
  const check = checkProfile(profile(), TODAY);
  assert.deepEqual(check.errors, {});
  assert.deepEqual(check.warnings, []);
  assert.equal(check.curp?.fechaNacimiento, "1953-11-13");
  assert.deepEqual(check.body, {
    nombres: "Andrés Manuel",
    apellido_paterno: "López",
    apellido_materno: "Obrador",
    curp: "LOOA531113HTCPBN07",
    rfc: buildRfcFromCurp("LOOA531113HTCPBN07", "AB"),
    domicilio: {
      calle: "Av. Paseo Tabasco",
      numero_exterior: "1401",
      numero_interior: null,
      colonia: "Tabasco 2000",
      municipio: "Centro",
      estado: "Tabasco",
      codigo_postal: "86035",
    },
  });
  // An empty maternal surname is sent as null.
  assert.equal(checkProfile(profile({ apellido_materno: "  " }), TODAY).body?.apellido_materno, null);
  // Decomposed accents are sent precomposed (the backend maps those only).
  assert.equal(checkProfile(profile({ apellido_paterno: "López" }), TODAY).body?.apellido_paterno, "López");
});

test("step 1 rejects inconsistent data", () => {
  assert.equal(checkProfile(profile({ curp: "LOOA531113HTCPBN08" }), TODAY).errors.curp, KYC_MESSAGES.curpCheckDigit);
  const otherDate = checkProfile(profile({ rfc: buildRfcFromCurp("LOOA531114HTCPBN07", "AB") }), TODAY);
  assert.equal(otherDate.errors.rfc, KYC_MESSAGES.rfcDateMismatch);
  assert.equal(otherDate.body, null);
  assert.equal(checkProfile(profile({}, { codigo_postal: "8603" }), TODAY).errors.codigo_postal, KYC_MESSAGES.postalCode);
  // A name that does not match the CURP is accepted with a warning.
  const other = checkProfile(profile({ nombres: "Beatriz" }), TODAY);
  assert.deepEqual(other.errors, {});
  assert.equal(other.warnings.length, 1);
  assert.ok(other.body);
  // Under 18.
  const minorCurp = buildCurp({ nombres: "Ana", apellidoPaterno: "Ruiz", fechaNacimiento: "2010-01-01", sexo: "M", entidad: "DF", homoclave: 1 });
  const minor = checkProfile(profile({ curp: minorCurp, rfc: buildRfcFromCurp(minorCurp, "AB") }), TODAY);
  assert.equal(minor.errors.curp, KYC_MESSAGES.underage);
  assert.equal(minor.body, null);
  // Every field is reported at once (the backend stops at the first).
  const empty = checkProfile(
    profile({ nombres: "", apellido_paterno: "123", curp: "", rfc: "" }, { calle: "", numero_exterior: "x".repeat(21), estado: "" }),
    TODAY,
  );
  assert.deepEqual(Object.keys(empty.errors).sort(), ["apellido_paterno", "calle", "curp", "estado", "nombres", "numero_exterior", "rfc"]);
  assert.equal(empty.errors.nombres, "Given names is required.");
  assert.equal(empty.errors.apellido_paterno, "Paternal surname must contain letters.");
  assert.equal(empty.errors.numero_exterior, "Exterior number can have at most 20 characters.");
});

test("age in whole years", () => {
  assert.equal(ageOn("2008-10-14", "2026-10-13"), 17);
  assert.equal(ageOn("2008-10-13", "2026-10-13"), 18);
  assert.equal(ageOn("2008-02-29", "2026-02-28"), 17);
  assert.equal(ageOn("2008-02-29", "2026-03-01"), 18);
  assert.throws(() => ageOn("2008-02-30", TODAY), RangeError);
});

test("today in Mexico City (UTC-6, like spei::hoy_cdmx)", () => {
  assert.equal(todayInMexicoCity(new Date("2026-10-14T05:59:59Z")), "2026-10-13");
  assert.equal(todayInMexicoCity(new Date("2026-10-14T06:00:00Z")), "2026-10-14");
});

// -----------------------------------------------------------------------------
// Images (expediente.rs decodificar_imagen)
// -----------------------------------------------------------------------------

function jpeg(size: number, fill: number): Uint8Array {
  const bytes = new Uint8Array(size).fill(fill);
  bytes.set([0xff, 0xd8, 0xff, 0xe0]);
  return bytes;
}

const b64 = (bytes: Uint8Array) => Buffer.from(bytes).toString("base64");
const dataUrl = (bytes: Uint8Array, type = "image/jpeg") => `data:${type};base64,${b64(bytes)}`;

test("images are checked like the backend decodes them", () => {
  const ok = inspectImage("INE front", dataUrl(jpeg(2048, 7)));
  assert.ok(ok.ok);
  assert.equal(ok.value.type, "image/jpeg");
  assert.equal(ok.value.bytes, 2048);
  // Plain base64 (no data URL) and line breaks are accepted.
  assert.ok(inspectImage("x", b64(jpeg(2048, 7)).replace(/(.{76})/g, "$1\n")).ok);
  const png = new Uint8Array(4096).fill(1);
  png.set([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  assert.equal(inspectImage("x", dataUrl(png, "image/png")).ok && "png", "png");
  const webp = new Uint8Array(4096).fill(2);
  webp.set([...Buffer.from("RIFF"), 0, 0, 0, 0, ...Buffer.from("WEBP")]);
  const w = inspectImage("x", dataUrl(webp, "image/webp"));
  assert.ok(w.ok && w.value.type === "image/webp");

  const code = (input: string) => {
    const r = inspectImage("Selfie frame 1", input);
    return r.ok ? "OK" : r.code;
  };
  assert.equal(code("no-es-base64!"), "BASE64");
  assert.equal(code(b64(new Uint8Array(2048).fill(0x61))), "TYPE");
  assert.equal(code(b64(jpeg(2048, 7).slice(0, 100))), "SIZE");
  assert.equal(code(b64(jpeg(1023, 7))), "SIZE");
  assert.equal(code(b64(jpeg(1024, 7))), "OK");
  assert.equal(code(b64(jpeg(3 * 1024 * 1024 + 1, 7))), "SIZE");
  // Non-canonical base64 (missing padding, non-zero trailing bits).
  assert.equal(code(b64(jpeg(2048, 7)).replace(/=+$/, "")), "BASE64");
  const twoPads = b64(jpeg(2050, 7));
  assert.ok(twoPads.endsWith("=="));
  assert.equal(code(twoPads), "OK");
  assert.equal(code(`${twoPads.slice(0, -3)}B==`), "BASE64");
  const onePad = b64(jpeg(2048, 7));
  assert.ok(onePad.endsWith("=") && !onePad.endsWith("=="));
  assert.equal(code(`${onePad.slice(0, -2)}B=`), "BASE64");
  const r = inspectImage("Selfie frame 1", "no-es-base64!");
  assert.ok(!r.ok);
  assert.equal(r.error, "Selfie frame 1: the image must be base64-encoded.");
});

test("the verification needs 3 to 8 distinct frames and distinct images", () => {
  const img = (n: number) => dataUrl(jpeg(2048, n));
  const frente = img(1);
  const reverso = img(2);
  assert.deepEqual(checkVerificationImages({ frente, reverso, frames: [img(3), img(4), img(5)] }), []);
  assert.deepEqual(checkVerificationImages({ frente, reverso, frames: [img(3), img(4)] }), [KYC_MESSAGES.frameCount]);
  assert.deepEqual(
    checkVerificationImages({ frente, reverso, frames: Array.from({ length: 9 }, (_, i) => img(10 + i)) }),
    [KYC_MESSAGES.frameCount],
  );
  // The same bytes in another encoding (plain base64 vs data URL) are still the same image.
  assert.deepEqual(checkVerificationImages({ frente, reverso, frames: [img(3), b64(jpeg(2048, 3)), img(5)] }), [
    KYC_MESSAGES.distinctImages,
  ]);
  assert.deepEqual(checkVerificationImages({ frente, reverso: frente, frames: [img(3), img(4), img(5)] }), [
    KYC_MESSAGES.distinctImages,
  ]);
  assert.deepEqual(checkVerificationImages({ frente: "x", reverso, frames: [img(3), img(4), img(5)] }), [
    "INE front: the image must be base64-encoded.",
  ]);
  // Over the 12 MB body limit even though each image is under 3 MB.
  const big = (n: number) => dataUrl(jpeg(2_500_000, n));
  assert.deepEqual(checkVerificationImages({ frente: big(1), reverso: big(2), frames: [big(3), big(4), big(5)] }), [
    KYC_MESSAGES.bodyTooLarge,
  ]);
});

// -----------------------------------------------------------------------------
// Sample identities (the e2e test generates unique valid identities with them)
// -----------------------------------------------------------------------------

test("sample identities pass every check without warnings", () => {
  const seeds = [0, 1, 99, 100, 999, 1000, 123_456, 9_999_999, SAMPLE_IDENTITY_SPACE - 1, Date.UTC(2026, 9, 13)];
  for (let s = 0; s < 3000; s += 7) seeds.push(s * 6089);
  for (const seed of seeds) {
    const id = sampleIdentity(seed, TODAY);
    const step1 = checkProfile(id.profile, TODAY);
    assert.deepEqual(step1.errors, {}, `seed ${seed}`);
    assert.deepEqual(step1.warnings, [], `seed ${seed}`);
    assert.deepEqual(step1.body, id.profile, `seed ${seed}: the profile is already normalized`);
    const parsed = curp(id.curp);
    assert.equal(parsed.fechaNacimiento, id.fechaNacimiento);
    assert.equal(parsed.sexo, id.sexo);
    assert.equal(parsed.entidad, id.entidad);
    assert.deepEqual(validateCredential(id.credential, parsed, TODAY), { errors: [], warnings: [], fieldErrors: {} }, `seed ${seed}`);
    assert.ok(ageOn(id.fechaNacimiento, TODAY) >= 18);
  }
});

test("distinct seeds give distinct CURPs, RFCs and claves de elector", () => {
  const curps = new Set<string>();
  const rfcs = new Set<string>();
  const claves = new Set<string>();
  // Every name/homoclave combination of two birth dates, plus a spread of others.
  const seeds: number[] = [];
  for (let s = 0; s < 2000; s++) seeds.push(s);
  for (let s = 0; s < 3000; s++) seeds.push(2000 + s * 6007);
  for (const seed of seeds) {
    const id = sampleIdentity(seed, TODAY);
    curps.add(id.curp);
    rfcs.add(id.rfc);
    claves.add(id.credential.clave_elector);
  }
  assert.equal(curps.size, seeds.length);
  assert.equal(rfcs.size, seeds.length);
  assert.equal(claves.size, seeds.length);
  // Seeds wrap around the identity space.
  assert.equal(sampleIdentity(SAMPLE_IDENTITY_SPACE + 5, TODAY).curp, sampleIdentity(5, TODAY).curp);
});

// -----------------------------------------------------------------------------
// Backend messages in English
// -----------------------------------------------------------------------------

test("backend messages are translated to English", () => {
  assert.equal(kycMessageInEnglish("el dígito verificador de la CURP no corresponde"), KYC_MESSAGES.curpCheckDigit);
  assert.equal(kycMessageInEnglish("la entidad de nacimiento ZZ no existe"), "Birth state ZZ in the CURP does not exist.");
  assert.equal(
    kycMessageInEnglish("las letras de la CURP (LOOA…PBN) no corresponden al nombre capturado (se esperaban BXXA…TXX)"),
    "The CURP letters (LOOA…PBN) do not match the name you entered (expected BXXA…TXX).",
  );
  // validar_credencial errors joined with "; ".
  assert.equal(
    kycMessageInEnglish("el CIC de la credencial tiene 9 dígitos; la credencial venció en 2024"),
    "The credential's CIC has 9 digits. The credential expired in 2024.",
  );
  assert.equal(kycMessageInEnglish("calle admite como máximo 120 caracteres"), "Street can have at most 120 characters.");
  assert.equal(kycMessageInEnglish("selfie.frames[2]: se admite JPEG, PNG o WebP"), "Selfie frame 3: only JPEG, PNG or WebP images are accepted.");
  assert.equal(
    kycMessageInEnglish("prueba de vida fallida: los cuadros de la cámara son idénticos (posible foto o repetición)"),
    "Liveness check failed: the camera frames are identical (possible photo or replay).",
  );
  assert.equal(
    kycMessageInEnglish("intentos agotados: credencial no válida: anverso y reverso son la misma imagen"),
    "No verification attempts left. The credential is not valid: front and back are the same image.",
  );
  assert.equal(
    kycMessageInEnglish("el expediente 3f2a0c1e-0000-4000-8000-000000000000 agotó sus 3 intentos"),
    "You have used all 3 verification attempts. Contact support to reopen your file.",
  );
  assert.equal(
    kycMessageInEnglish("la identidad del expediente 3f2a0c1e-0000-4000-8000-000000000000 no cambia en estado EN_REVISION"),
    "Your identity details cannot change while your file is in review.",
  );
  assert.equal(kycMessageInEnglish("similitud facial 0.6500 por debajo del mínimo aceptable 0.70"), "Face similarity 0.6500 is below the minimum acceptable 0.70.");
  // Free text (an operator's reason) is not recognized.
  assert.equal(kycMessageInEnglish("documento ilegible"), null);
  assert.equal(describeKycMessage("documento ilegible"), "documento ilegible");
});
