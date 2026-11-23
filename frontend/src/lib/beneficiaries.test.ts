/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  FULL,
  allocation,
  allocationLabel,
  centsToPercent,
  designationProblem,
  designationRequest,
  draftErrors,
  draftsFrom,
  emptyDraft,
  isDirty,
  normalizeLegalName,
  parseBeneficiaries,
  percentToCents,
  splitEvenly,
} from "./beneficiaries.ts";
import { curpCheckDigit } from "./kyc.ts";
import type { BeneficiaryDraft } from "../types/beneficiaries.ts";

const TODAY = "2026-11-23";
const curp = (first17: string) => `${first17}${curpCheckDigit(first17)}`;
const MARIA = curp("LORM900101MDFPJR0");

function draft(over: Partial<BeneficiaryDraft> = {}): BeneficiaryDraft {
  return {
    ...emptyDraft(),
    fullName: "María López Rojas",
    relationship: "HIJO",
    birthDate: "2005-06-07",
    percent: "100",
    ...over,
  };
}

test("percentages in whole cents", () => {
  assert.equal(percentToCents("33.34"), 3334);
  assert.equal(percentToCents("60"), 6000);
  assert.equal(percentToCents("0.5"), 50);
  assert.equal(percentToCents(" 100 "), 10_000);
  for (const bad of ["", "33.333", "-5", "1e2", "abc", "1,5", "1000.5.5"]) assert.equal(percentToCents(bad), null, bad);
  assert.equal(centsToPercent(3334), "33.34");
  assert.equal(centsToPercent(6000), "60");
  assert.equal(centsToPercent(50), "0.50");
  assert.equal(centsToPercent(FULL), "100");
});

test("the allocation bar adds cents, never floats", () => {
  const three = splitEvenly(3).map((p) => draft({ percent: p }));
  assert.deepEqual(splitEvenly(3), ["33.34", "33.33", "33.33"]);
  assert.deepEqual(splitEvenly(6).length, 6);
  assert.equal(
    splitEvenly(7).reduce((s, p) => s + (percentToCents(p) ?? 0), 0),
    FULL,
  );
  const a = allocation(three);
  assert.equal(a.allocated, FULL);
  assert.equal(a.status, "exact");
  assert.equal(allocationLabel(a), "100% allocated");
  assert.equal(
    allocationLabel(allocation([draft({ percent: "60" }), draft({ percent: "25" })])),
    "85% allocated · 15% left",
  );
  assert.equal(
    allocationLabel(allocation([draft({ percent: "60" }), draft({ percent: "50" })])),
    "110% allocated · 10% over",
  );
  assert.equal(allocation([draft({ percent: "33.333" })]).status, "invalid");
  assert.equal(allocation([]).status, "empty");
  // 0.1 + 0.2 in floating point is not 0.3; in cents it is.
  assert.equal(
    allocation([draft({ percent: "0.1" }), draft({ percent: "0.2" }), draft({ percent: "99.7" })]).status,
    "exact",
  );
});

test("each draft is checked like the server checks it", () => {
  assert.deepEqual(draftErrors(draft(), TODAY), {});
  assert.deepEqual(draftErrors(draft({ curp: MARIA.toLowerCase(), birthDate: "1990-01-01" }), TODAY), {});
  assert.ok(draftErrors(draft({ fullName: "María" }), TODAY).fullName, "needs a surname");
  assert.ok(draftErrors(draft({ fullName: "María L0pez" }), TODAY).fullName);
  assert.ok(draftErrors(draft({ relationship: "" }), TODAY).relationship);
  assert.ok(draftErrors(draft({ birthDate: "", curp: "" }), TODAY).identity);
  assert.ok(draftErrors(draft({ curp: `${MARIA.slice(0, 17)}${MARIA.endsWith("0") ? "1" : "0"}` }), TODAY).curp);
  assert.match(
    draftErrors(draft({ curp: MARIA, birthDate: "1991-01-01" }), TODAY).birthDate ?? "",
    /CURP says 1990-01-01/,
  );
  assert.ok(draftErrors(draft({ birthDate: "2026-11-24" }), TODAY).birthDate, "future");
  assert.ok(draftErrors(draft({ birthDate: "1899-12-31" }), TODAY).birthDate);
  assert.ok(draftErrors(draft({ curp: MARIA, birthDate: "" }), TODAY, MARIA).curp, "the holder");
  assert.ok(draftErrors(draft({ percent: "0" }), TODAY).percent);
  assert.ok(draftErrors(draft({ percent: "100.5" }), TODAY).percent);
});

test("the whole designation: count, duplicates and exactly 100%", () => {
  assert.equal(designationProblem([], TODAY), null, "an empty designation withdraws it");
  assert.equal(designationProblem([draft({ percent: "60" }), draft({ percent: "40" })], TODAY), null);
  assert.equal(
    designationProblem([draft({ percent: "60" }), draft({ percent: "30" })], TODAY),
    "The percentages must add up to exactly 100%.",
  );
  assert.equal(
    designationProblem(
      Array.from({ length: 6 }, () => draft({ percent: "10" })),
      TODAY,
    ),
    "Name at most 5 beneficiaries.",
  );
  assert.equal(designationProblem([draft({ fullName: "" })], TODAY), "Check beneficiary 1.");
  assert.equal(
    designationProblem(
      [
        draft({ curp: MARIA, birthDate: "", percent: "50" }),
        draft({ curp: MARIA.toLowerCase(), birthDate: "", percent: "50" }),
      ],
      TODAY,
    ),
    "The same CURP appears twice.",
  );
});

test("the request is what the server expects", () => {
  const body = designationRequest(
    [
      draft({
        fullName: "  maría   lópez rojas ",
        curp: MARIA.toLowerCase(),
        birthDate: "1990-01-01",
        percent: "33.40",
      }),
      draft({ curp: "", percent: "66.6" }),
    ],
    3,
  );
  assert.deepEqual(body, {
    beneficiarios: [
      {
        nombre_completo: "MARÍA LÓPEZ ROJAS",
        parentesco: "HIJO",
        curp: MARIA,
        fecha_nacimiento: "1990-01-01",
        porcentaje: "33.40",
      },
      { nombre_completo: "MARÍA LÓPEZ ROJAS", parentesco: "HIJO", fecha_nacimiento: "2005-06-07", porcentaje: "66.60" },
    ],
    version_base: 3,
  });
  assert.equal(normalizeLegalName("jean-luc  o'neil"), "JEAN-LUC O'NEIL");
});

const HASH_A = "a".repeat(64);
const HASH_0 = "0".repeat(64);
const VIEW = {
  vigente: {
    version: 2,
    creada_en: "2026-11-23T12:00:00Z",
    dispositivo: "Chrome on macOS",
    ip: "203.0.113.7",
    hash: HASH_A,
    hash_anterior: HASH_0,
    beneficiarios: [
      {
        posicion: 1,
        nombre_completo: "MARÍA LÓPEZ ROJAS",
        parentesco: "HIJO",
        curp: MARIA,
        fecha_nacimiento: "1990-01-01",
        porcentaje: "60.00",
      },
      {
        posicion: 2,
        nombre_completo: "JOSÉ LÓPEZ ROJAS",
        parentesco: "HIJO",
        curp: null,
        fecha_nacimiento: "2005-06-07",
        porcentaje: "40.00",
      },
    ],
  },
  version: 2,
  historial: [
    {
      version: 2,
      creada_en: "2026-11-23T12:00:00Z",
      num_beneficiarios: 2,
      dispositivo: "Chrome on macOS",
      hash: HASH_A,
    },
    { version: 1, creada_en: "2026-11-22T12:00:00Z", num_beneficiarios: 1, dispositivo: null, hash: HASH_0 },
  ],
  cadena_verificada: true,
  puede_designar: true,
  maximo: 5,
  parentescos: ["CONYUGE"],
};

test("the API's answer, validated whole", () => {
  const view = parseBeneficiaries(VIEW);
  assert.ok(view);
  assert.equal(view.current?.beneficiaries[1]?.percent, "40.00");
  assert.equal(view.history.length, 2);
  assert.equal(view.chainVerified, true);
  assert.deepEqual(parseBeneficiaries({ ...VIEW, vigente: null, version: 0, historial: [] })?.current, null);
  for (const bad of [
    { ...VIEW, version: 3 },
    { ...VIEW, maximo: 6 },
    { ...VIEW, cadena_verificada: "yes" },
    { ...VIEW, vigente: { ...VIEW.vigente, hash: "x" } },
    {
      ...VIEW,
      vigente: { ...VIEW.vigente, beneficiarios: [{ ...VIEW.vigente.beneficiarios[0], parentesco: "VECINO" }] },
    },
    { ...VIEW, vigente: { ...VIEW.vigente, beneficiarios: [{ ...VIEW.vigente.beneficiarios[0], porcentaje: 60 }] } },
    { ...VIEW, historial: [{ ...VIEW.historial[0], num_beneficiarios: 7 }] },
  ]) {
    assert.equal(parseBeneficiaries(bad), null, JSON.stringify(bad).slice(0, 80));
  }
});

test("drafts from what is saved, and whether they changed", () => {
  const view = parseBeneficiaries(VIEW);
  assert.ok(view);
  const drafts = draftsFrom(view.current);
  assert.deepEqual(
    drafts.map((d) => [d.fullName, d.percent, d.curp]),
    [
      ["MARÍA LÓPEZ ROJAS", "60", MARIA],
      ["JOSÉ LÓPEZ ROJAS", "40", ""],
    ],
  );
  assert.equal(isDirty(drafts, view.current), false);
  assert.equal(
    isDirty(
      drafts.map((d, i) => (i === 0 ? { ...d, percent: "60.00" } : d)),
      view.current,
    ),
    false,
    "same value",
  );
  assert.equal(
    isDirty(
      drafts.map((d, i) => (i === 0 ? { ...d, percent: "55" } : d)),
      view.current,
    ),
    true,
  );
  assert.equal(isDirty([], view.current), true);
  assert.equal(isDirty([], null), false);
});
