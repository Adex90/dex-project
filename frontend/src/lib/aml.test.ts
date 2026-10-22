/**
 * Logic of the compliance console (src/lib/aml.ts). The action and four-eyes
 * rules mirror the database functions of migration 0009 (aml.resolver_alerta,
 * aml.decidir_levantamiento), which have the final word.
 *
 * Run with: npm test   (Node >= 22.18 strips TypeScript types natively)
 */
import assert from "node:assert/strict";
import { test } from "node:test";

import type { AlertaAml, CalculoAml, EvaluacionAml, PropuestaAml, ResolverAlertaInput, SolicitudAml } from "./api/types.ts";
import {
  CLIENT_HOLD_LABELS,
  DEFAULT_THRESHOLDS,
  GENERIC_UNAVAILABLE_MESSAGE,
  HOLD_COPY,
  LEVELS,
  SECOND_OFFICER_REQUIRED,
  allowedActions,
  canAct,
  canDecideProposal,
  canDecideRequest,
  canRecordUifFolio,
  compareInstants,
  containsTippingOffTerms,
  daysToDeadline,
  defaultFilters,
  describeCounterpartyKey,
  describeDetails,
  factorRows,
  filtersFromSearch,
  filtersToQuery,
  formatScore,
  isComplianceRole,
  levelFromScore,
  levelTone,
  matchesFilters,
  matchesSearch,
  mergeFeed,
  nextPollCursor,
  nextPollDelay,
  slaRemaining,
  sortAlerts,
  strongestEvaluation,
  validateNarrative,
  validateResolution,
  validateUifFolio,
  type AlertFilters,
} from "./aml.ts";

const ME = "00000000-0000-4000-8000-0000000000a1";
const OTHER_OFFICER = "00000000-0000-4000-8000-0000000000a2";
const CLIENT = "00000000-0000-4000-8000-0000000000c1";

let seq = 0;
function alert(overrides: Partial<AlertaAml> = {}): AlertaAml {
  seq += 1;
  return {
    id: `a0000000-0000-4000-8000-${String(seq).padStart(12, "0")}`,
    folio: seq,
    estado: "PENDIENTE",
    nivel_riesgo: "ALTO",
    puntaje: "65.00",
    tipo_reporte: "INUSUAL",
    origen: "EN_LINEA",
    decision: "REGISTRADA",
    regla: { id: "r0000000-0000-4000-8000-000000000002", codigo: "R02_FRACCIONAMIENTO", version: 1, nombre: "Fraccionamiento" },
    cliente: { id: CLIENT, nombre_completo: "María José Pérez López", nivel_riesgo_pld: "MEDIO", es_pep: false, es_interno: false },
    entidad: "IFPE",
    sentido: "ENTRADA",
    transaccion_id: null,
    monto: "145000.00000000",
    monto_acumulado: "290000.00000000",
    ocurrencias: 1,
    timestamp: "2026-10-21T15:00:00.000000+00:00",
    ultima_ocurrencia_en: "2026-10-21T15:00:00.000000+00:00",
    actualizado_en: "2026-10-21T15:00:00.000000+00:00",
    fecha_limite_reporte: "2026-12-20T15:00:00.000000+00:00",
    urgente: false,
    congelamiento: null,
    asignada_a: null,
    tomada_en: null,
    escalada: false,
    resolucion_propuesta: null,
    propuesta_por: null,
    dictamen: null,
    resuelta_por: null,
    resuelta_en: null,
    aprobada_por: null,
    folio_uif: null,
    reportada_uif_en: null,
    ...overrides,
  };
}

const person = (id: string, nombre = "Officer") => ({ id, nombre_completo: nombre });
const inReviewByMe = (overrides: Partial<AlertaAml> = {}) =>
  alert({ estado: "EN_REVISION", asignada_a: person(ME), tomada_en: "2026-10-21T16:00:00+00:00", ...overrides });
const activeFreeze = { id: "f0000000-0000-4000-8000-000000000001", folio: 1, estado: "ACTIVO" as const, confirmado: false, sla_vence_en: "2026-10-24T15:00:00+00:00" };

test("levelFromScore mirrors aml.nivel_de_puntaje at every boundary", () => {
  const cases: [string, string][] = [
    ["0", "BAJO"],
    ["29.99", "BAJO"],
    ["30", "MEDIO"],
    ["30.00", "MEDIO"],
    ["59.99", "MEDIO"],
    ["60", "ALTO"],
    ["84.99", "ALTO"],
    ["85", "CRITICO"],
    ["100.00", "CRITICO"],
  ];
  for (const [score, level] of cases) assert.equal(levelFromScore(score), level, score);
  const custom = { medio: "20", alto: "50.5", critico: "90" };
  assert.equal(levelFromScore("19.99", custom), "BAJO");
  assert.equal(levelFromScore("20", custom), "MEDIO");
  assert.equal(levelFromScore("50.49", custom), "MEDIO");
  assert.equal(levelFromScore("50.50", custom), "ALTO");
  assert.equal(levelFromScore("89.99", custom), "ALTO");
  assert.equal(levelFromScore("90", custom), "CRITICO");
  assert.deepEqual(DEFAULT_THRESHOLDS, { medio: "30", alto: "60", critico: "85" });
});

test("levelTone for the four levels", () => {
  assert.deepEqual(
    LEVELS.map((l) => levelTone(l)),
    ["neutral", "info", "warning", "danger"],
  );
  assert.equal(formatScore("65.00"), "65");
  assert.equal(formatScore("65.50"), "65.5");
  assert.equal(formatScore("100"), "100");
});

test("sortAlerts: open before closed, then level, urgent, deadline and folio", () => {
  const closedCritical = alert({ folio: 1, estado: "CONFIRMADO", nivel_riesgo: "CRITICO" });
  const lowOpen = alert({ folio: 2, nivel_riesgo: "BAJO" });
  const highLate = alert({ folio: 3, fecha_limite_reporte: "2026-12-30T00:00:00+00:00" });
  const highEarly = alert({ folio: 4, fecha_limite_reporte: "2026-12-01T00:00:00+00:00" });
  const highUrgent = alert({ folio: 5, urgente: true, fecha_limite_reporte: "2026-12-31T00:00:00+00:00" });
  const highTieA = alert({ folio: 7, fecha_limite_reporte: "2026-12-10T00:00:00+00:00" });
  const highTieB = alert({ folio: 6, estado: "EN_REVISION", fecha_limite_reporte: "2026-12-10T00:00:00+00:00" });
  const criticalOpen = alert({ folio: 8, nivel_riesgo: "CRITICO" });
  const fp = alert({ folio: 9, estado: "FALSO_POSITIVO", nivel_riesgo: "MEDIO" });
  const input = [closedCritical, lowOpen, highLate, highEarly, highUrgent, highTieA, highTieB, criticalOpen, fp];
  const sorted = sortAlerts(input).map((a) => a.folio);
  assert.deepEqual(sorted, [8, 5, 4, 6, 7, 3, 2, 1, 9]);
  // A copy: the input order is untouched.
  assert.equal(input[0], closedCritical);
});

test("mergeFeed keeps the newest copy of each alert and reports what changed", () => {
  const a = alert({ actualizado_en: "2026-10-21T15:00:00.100000+00:00" });
  const b = alert({ actualizado_en: "2026-10-21T15:00:00.000000+00:00" });
  const prev = [a, b];

  const aNewer = { ...a, estado: "EN_REVISION" as const, actualizado_en: "2026-10-21T15:00:00.100001+00:00" };
  const bOlder = { ...b, estado: "CONFIRMADO" as const, actualizado_en: "2026-10-21T14:59:59+00:00" };
  const bSame = { ...b, nivel_riesgo: "CRITICO" as const };
  const c = alert();
  const cAgain = { ...c, ocurrencias: 2, actualizado_en: "2026-10-21T15:01:00+00:00" };

  const merged = mergeFeed(prev, [aNewer, bOlder, bSame, c, cAgain]);
  assert.deepEqual(merged.alertas.map((x) => x.id), [a.id, b.id, c.id]);
  assert.equal(merged.alertas[0]!.estado, "EN_REVISION", "a microsecond newer copy wins");
  assert.equal(merged.alertas[1], b, "older and identical copies are ignored");
  assert.equal(merged.alertas[2]!.ocurrencias, 2, "a duplicate inside the poll keeps the newer copy");
  assert.deepEqual(merged.nuevas, [c.id]);
  assert.deepEqual(merged.actualizadas, [a.id]);
  // The previous list is not mutated.
  assert.equal(prev[0], a);
  assert.deepEqual(mergeFeed([], []), { alertas: [], nuevas: [], actualizadas: [] });
  assert.equal(compareInstants("2026-10-21T15:00:00.1+00:00", "2026-10-21T15:00:00.099999+00:00"), 1);
});

test("matchesFilters by state, level, rule and assignee", () => {
  const f = defaultFilters();
  const pending = alert();
  const mine = inReviewByMe({ nivel_riesgo: "CRITICO", regla: { ...pending.regla, codigo: "R05_ESTRUCTURACION" } });
  const confirmed = alert({ estado: "CONFIRMADO" });
  assert.equal(matchesFilters(pending, f, ME), true);
  assert.equal(matchesFilters(mine, f, ME), true);
  assert.equal(matchesFilters(confirmed, f, ME), false);
  assert.equal(matchesFilters(confirmed, { ...f, estados: [] }, ME), true, "no state selected means all");
  assert.equal(matchesFilters(pending, { ...f, niveles: ["CRITICO"] }, ME), false);
  assert.equal(matchesFilters(mine, { ...f, niveles: ["CRITICO"] }, ME), true);
  assert.equal(matchesFilters(mine, { ...f, regla: "R05_ESTRUCTURACION" }, ME), true);
  assert.equal(matchesFilters(pending, { ...f, regla: "R05_ESTRUCTURACION" }, ME), false);
  assert.equal(matchesFilters(mine, { ...f, asignada: "yo" }, ME), true);
  assert.equal(matchesFilters(mine, { ...f, asignada: "yo" }, OTHER_OFFICER), false);
  assert.equal(matchesFilters(pending, { ...f, asignada: "nadie" }, ME), true);
  assert.equal(matchesFilters(mine, { ...f, asignada: "nadie" }, ME), false);

  assert.equal(matchesSearch(pending, ""), true);
  assert.equal(matchesSearch(pending, `#${pending.folio}`), true);
  assert.equal(matchesSearch(pending, "jose perez"), true, "accents and case are ignored");
  assert.equal(matchesSearch(pending, CLIENT.slice(0, 8)), true);
  assert.equal(matchesSearch(pending, "nobody"), false);
});

test("filtersToQuery and filtersFromSearch round-trip through the URL", () => {
  const roundTrip = (f: AlertFilters) => {
    const query = filtersToQuery(f);
    const params = new URLSearchParams();
    for (const [key, value] of Object.entries(query)) if (value !== undefined) params.set(key, value);
    return { query, back: filtersFromSearch(new URLSearchParams(params.toString())) };
  };
  const cases: AlertFilters[] = [
    defaultFilters(),
    { estados: [], niveles: [], regla: null, asignada: null },
    { estados: ["CONFIRMADO", "FALSO_POSITIVO"], niveles: ["ALTO", "CRITICO"], regla: "R05_ESTRUCTURACION", asignada: "yo" },
    { estados: ["PENDIENTE"], niveles: ["BAJO"], regla: null, asignada: "nadie" },
  ];
  for (const f of cases) assert.deepEqual(roundTrip(f).back, f);

  assert.deepEqual(roundTrip(defaultFilters()).query, { estado: undefined, nivel: undefined, regla: undefined, asignada: undefined });
  assert.equal(roundTrip(cases[1]!).query.estado, "TODOS");
  assert.equal(roundTrip(cases[2]!).query.estado, "CONFIRMADO,FALSO_POSITIVO");
  assert.equal(roundTrip(cases[2]!).query.nivel, "ALTO,CRITICO");
  // Order is canonical and unknown values are dropped.
  assert.deepEqual(filtersFromSearch(new URLSearchParams("estado=en_revision,BOGUS,PENDIENTE&nivel=CRITICO,BAJO,x&asignada=all")), {
    estados: ["PENDIENTE", "EN_REVISION"],
    niveles: ["BAJO", "CRITICO"],
    regla: null,
    asignada: null,
  });
  assert.deepEqual(filtersFromSearch(new URLSearchParams("estado=BOGUS")).estados, ["PENDIENTE", "EN_REVISION"]);
});

test("nextPollCursor overlaps the previous poll by 60 s; nextPollDelay backs off to 60 s", () => {
  assert.equal(nextPollCursor("2026-10-21T15:04:05.123456+00:00"), "2026-10-21T15:03:05.123Z");
  assert.equal(nextPollCursor("2026-10-21T00:00:30Z"), "2026-10-20T23:59:30.000Z");
  assert.deepEqual(
    [0, 1, 2, 3, 4].map((n) => nextPollDelay(n)),
    [10_000, 20_000, 40_000, 60_000, 60_000],
  );
});

test("allowedActions mirrors the database's resolution rules", () => {
  assert.deepEqual(allowedActions("AUDITOR", ME, alert()), []);
  assert.deepEqual(allowedActions("OPERADOR", ME, alert()), []);
  assert.deepEqual(allowedActions(null, ME, alert()), []);
  assert.deepEqual(allowedActions("CUMPLIMIENTO", CLIENT, alert()), [], "own client record");
  assert.deepEqual(allowedActions("CUMPLIMIENTO", ME, alert()), ["TOMAR"]);
  assert.deepEqual(allowedActions("CUMPLIMIENTO", ME, inReviewByMe()), ["LIBERAR", "ESCALAR", "CONFIRMAR", "FALSO_POSITIVO"]);
  assert.deepEqual(allowedActions("CUMPLIMIENTO", OTHER_OFFICER, inReviewByMe()), [], "held by another officer");
  assert.deepEqual(
    allowedActions(
      "CUMPLIMIENTO",
      ME,
      inReviewByMe({ resolucion_propuesta: "FALSO_POSITIVO", propuesta_por: person(ME), congelamiento: activeFreeze }),
    ),
    ["ESCALAR", "CONFIRMAR"],
    "pending false-positive proposal",
  );
  for (const estado of ["CONFIRMADO", "FALSO_POSITIVO"] as const) {
    assert.deepEqual(allowedActions("CUMPLIMIENTO", ME, alert({ estado, asignada_a: person(ME) })), [], estado);
  }

  const confirmed = alert({ estado: "CONFIRMADO" });
  assert.equal(canRecordUifFolio("CUMPLIMIENTO", ME, confirmed), true);
  assert.equal(canRecordUifFolio("AUDITOR", ME, confirmed), false);
  assert.equal(canRecordUifFolio("CUMPLIMIENTO", CLIENT, confirmed), false);
  assert.equal(canRecordUifFolio("CUMPLIMIENTO", ME, { ...confirmed, folio_uif: "SITI-2026-0001", reportada_uif_en: "2026-10-22" }), false);
  assert.equal(canRecordUifFolio("CUMPLIMIENTO", ME, inReviewByMe()), false);

  assert.equal(isComplianceRole("CUMPLIMIENTO"), true);
  assert.equal(isComplianceRole("AUDITOR"), true);
  assert.equal(isComplianceRole("OPERADOR"), false);
  assert.equal(isComplianceRole("CLIENTE"), false);
  assert.equal(isComplianceRole(null), false);
  assert.equal(canAct("CUMPLIMIENTO"), true);
  assert.equal(canAct("AUDITOR"), false);
});

const KEY = "c1a2b3c4-d5e6-4f70-8a91-b2c3d4e5f607";
const DICTAMEN = "Depósitos fraccionados sin soporte documental.";

test("validateNarrative trims, counts characters and allows only line breaks", () => {
  assert.match(validateNarrative("x".repeat(29), 30, 2000) ?? "", /at least 30 characters \(29 so far\)/);
  assert.equal(validateNarrative(`   ${"x".repeat(30)}\n  `, 30, 2000), null);
  assert.match(validateNarrative(`  ${"x".repeat(29)}  `, 30, 2000) ?? "", /29 so far/);
  assert.match(validateNarrative("x".repeat(2001), 30, 2000) ?? "", /at most 2000/);
  assert.equal(validateNarrative("x".repeat(2000), 30, 2000), null);
  assert.equal(validateNarrative(`${"línea uno ".repeat(3)}\nlínea dos`, 30, 2000), null, "\\n accepted");
  assert.match(validateNarrative(`${"x".repeat(30)}\tmore`, 30, 2000) ?? "", /control characters/);
  assert.match(validateNarrative(`${"x".repeat(30)}\u0000`.padEnd(40, "y"), 30, 2000) ?? "", /control characters/);
  assert.match(validateNarrative(`${"x".repeat(30)}\u0085y`, 30, 2000) ?? "", /control characters/);
  assert.equal(validateNarrative("ñ".repeat(30), 30, 2000), null, "characters, not bytes");
});

test("validateResolution enforces the dictamen and rejects misplaced fields", () => {
  const a = inReviewByMe();
  const frozen = inReviewByMe({ congelamiento: activeFreeze });
  const ok = (input: Omit<ResolverAlertaInput, "clave_idempotencia">, target = a) =>
    validateResolution({ ...input, clave_idempotencia: KEY }, target);

  assert.deepEqual(ok({ accion: "TOMAR" }), []);
  assert.deepEqual(ok({ accion: "LIBERAR", dictamen: null }), []);
  assert.equal(ok({ accion: "TOMAR", dictamen: DICTAMEN }).length, 1, "dictamen with TOMAR");
  assert.equal(ok({ accion: "ESCALAR" }).length, 1, "ESCALAR needs a dictamen");
  assert.equal(ok({ accion: "CONFIRMAR", dictamen: "x".repeat(29) }).length, 1);
  assert.deepEqual(ok({ accion: "CONFIRMAR", dictamen: ` ${"x".repeat(30)} ` }), []);
  assert.equal(ok({ accion: "FALSO_POSITIVO", dictamen: "x".repeat(2001) }).length, 1);
  assert.equal(ok({ accion: "FALSO_POSITIVO", dictamen: `${"x".repeat(30)}\u0007` }).length, 1);
  assert.deepEqual(ok({ accion: "FALSO_POSITIVO", dictamen: `${DICTAMEN}\nSegunda línea.` }), []);

  // Misplaced fields.
  assert.equal(ok({ accion: "TOMAR", urgente: true }).length, 1, "urgente with TOMAR");
  assert.equal(ok({ accion: "ESCALAR", dictamen: DICTAMEN, urgente: false }).length, 1, "urgente with ESCALAR");
  assert.deepEqual(ok({ accion: "CONFIRMAR", dictamen: DICTAMEN, urgente: true, tipo_reporte: "INTERNA_PREOCUPANTE", congelar: true }), []);
  assert.equal(ok({ accion: "LIBERAR", congelar: true }).length, 1, "congelar with LIBERAR");
  assert.equal(ok({ accion: "FALSO_POSITIVO", dictamen: DICTAMEN, congelar: false }).length, 1, "congelar with FALSO_POSITIVO");
  assert.deepEqual(ok({ accion: "ESCALAR", dictamen: DICTAMEN, congelar: true }), []);
  assert.equal(ok({ accion: "ESCALAR", dictamen: DICTAMEN, tipo_reporte: "INUSUAL" }).length, 1, "tipo_reporte with ESCALAR");
  assert.deepEqual(ok({ accion: "ESCALAR", dictamen: DICTAMEN, tipo_reporte: null }), [], "null is absent");
  assert.equal(
    ok({ accion: "CONFIRMAR", dictamen: DICTAMEN, tipo_levantamiento: "LEVANTAR_Y_LIBERAR" }, frozen).length,
    1,
    "tipo_levantamiento with CONFIRMAR",
  );

  // tipo_levantamiento only with an ACTIVO freeze.
  assert.equal(ok({ accion: "FALSO_POSITIVO", dictamen: DICTAMEN, tipo_levantamiento: "LEVANTAR_Y_LIBERAR" }).length, 1);
  assert.deepEqual(ok({ accion: "FALSO_POSITIVO", dictamen: DICTAMEN, tipo_levantamiento: "LEVANTAR_CONSERVANDO_RETENCIONES" }, frozen), []);
  assert.equal(
    ok({ accion: "FALSO_POSITIVO", dictamen: DICTAMEN, tipo_levantamiento: "LEVANTAR_Y_LIBERAR" }, { ...frozen, congelamiento: { ...activeFreeze, estado: "LEVANTADO" } }).length,
    1,
  );
  // The case file's view of the client's freeze wins over the alert's own link.
  assert.deepEqual(
    validateResolution({ accion: "FALSO_POSITIVO", dictamen: DICTAMEN, tipo_levantamiento: "LEVANTAR_Y_LIBERAR", clave_idempotencia: KEY }, a, true),
    [],
  );
});

function request(overrides: Partial<SolicitudAml> = {}): SolicitudAml {
  return {
    id: "50000000-0000-4000-8000-000000000001",
    folio: 1,
    congelamiento_id: activeFreeze.id,
    cliente: person(CLIENT, "María José Pérez López"),
    tipo: "LEVANTAR_Y_LIBERAR",
    justificacion: "Operaciones explicadas por nómina documentada del cliente.",
    solicitada_por: person(OTHER_OFFICER),
    solicitada_en: "2026-10-21T17:00:00+00:00",
    estado: "PENDIENTE",
    decidida_por: null,
    decidida_en: null,
    comentario: null,
    ...overrides,
  };
}

test("canDecideRequest enforces four eyes", () => {
  const s = request();
  assert.deepEqual(canDecideRequest("CUMPLIMIENTO", ME, s, []), { allowed: true, reason: null });
  assert.deepEqual(canDecideRequest("CUMPLIMIENTO", OTHER_OFFICER, s, []), { allowed: false, reason: SECOND_OFFICER_REQUIRED });
  const proposal = inReviewByMe({ resolucion_propuesta: "FALSO_POSITIVO", propuesta_por: person(ME), congelamiento: activeFreeze });
  assert.deepEqual(canDecideRequest("CUMPLIMIENTO", ME, s, [proposal]), { allowed: false, reason: SECOND_OFFICER_REQUIRED });
  const otherClient = { ...proposal, cliente: { ...proposal.cliente, id: "00000000-0000-4000-8000-0000000000c2" } };
  assert.equal(canDecideRequest("CUMPLIMIENTO", ME, s, [otherClient]).allowed, true, "a proposal on another client does not count");
  assert.equal(canDecideRequest("CUMPLIMIENTO", CLIENT, s, []).allowed, false, "the frozen client");
  assert.equal(canDecideRequest("AUDITOR", ME, s, []).allowed, false);
  assert.equal(canDecideRequest("CUMPLIMIENTO", ME, request({ estado: "APROBADA", decidida_por: person(OTHER_OFFICER) }), []).allowed, false);

  const p: PropuestaAml = {
    id: "60000000-0000-4000-8000-000000000001",
    folio: 1,
    tipo: "PARAMETROS",
    regla_codigo: null,
    nivel_cliente: null,
    cambios: { modo_observacion: true },
    justificacion: "Periodo de calibración de las primeras semanas en producción.",
    propuesta_por: person(OTHER_OFFICER),
    propuesta_en: "2026-10-21T17:00:00+00:00",
    estado: "PENDIENTE",
    decidida_por: null,
    decidida_en: null,
    comentario: null,
    resultado_id: null,
  };
  assert.deepEqual(canDecideProposal("CUMPLIMIENTO", ME, p), { allowed: true, reason: null });
  assert.deepEqual(canDecideProposal("CUMPLIMIENTO", OTHER_OFFICER, p), { allowed: false, reason: SECOND_OFFICER_REQUIRED });
  assert.equal(canDecideProposal("AUDITOR", ME, p).allowed, false);
  assert.equal(canDecideProposal("CUMPLIMIENTO", ME, { ...p, estado: "RECHAZADA" }).allowed, false);
});

test("daysToDeadline counts Mexico City calendar days (UTC-6)", () => {
  // 2026-10-21 00:00 in Mexico City is 06:00 UTC.
  const startOfDay = new Date("2026-10-21T06:00:00Z");
  const endOfDay = new Date("2026-10-22T05:59:59Z");
  assert.equal(daysToDeadline("2026-10-22T05:59:00+00:00", startOfDay), 0, "due later today");
  assert.equal(daysToDeadline("2026-10-22T06:00:00+00:00", startOfDay), 1, "due tomorrow at midnight");
  assert.equal(daysToDeadline("2026-10-22T06:00:00+00:00", endOfDay), 1, "one minute before midnight it is still tomorrow");
  assert.equal(daysToDeadline("2026-10-22T05:59:00+00:00", new Date("2026-10-22T06:00:00Z")), -1, "overdue since yesterday");
  assert.equal(daysToDeadline("2026-12-20T15:00:00+00:00", startOfDay), 60);
  assert.equal(daysToDeadline("2026-10-01T15:00:00+00:00", startOfDay), -20);

  assert.deepEqual(slaRemaining("2026-10-24T15:00:00+00:00", new Date("2026-10-21T15:30:00Z")), { overdue: false, hours: 71 });
  assert.deepEqual(slaRemaining("2026-10-24T15:00:00+00:00", new Date("2026-10-24T20:00:00Z")), { overdue: true, hours: 5 });
});

const calculo = (puntajeBruto: string, puntaje: string): CalculoAml => ({
  version: 1,
  parametros_id: 1,
  ahora: "2026-10-21T15:00:00+00:00",
  nivel_cliente: "MEDIO",
  perfil: {
    nivel_riesgo_pld: "MEDIO",
    es_pep: false,
    nacionalidad: "MX",
    pais_residencia: "MX",
    entidad_nacimiento: "DF",
    estado_domicilio: "CIUDAD DE MEXICO",
    ingreso_mensual_estimado: null,
    antiguedad_dias: 3,
    es_interno: false,
  },
  ventana: {
    desde: "2026-10-20T15:00:00+00:00",
    hasta: "2026-10-21T15:00:00+00:00",
    horas: 24,
    entradas_n: 3,
    entradas_monto: "435000.00000000",
    salidas_n: 0,
    salidas_monto: "0.00000000",
    operaciones_n: 3,
    volumen: "435000.00000000",
  },
  factores: [
    { codigo: "M03_NIVEL_RIESGO", regla_id: "r3", version: 1, tipo: "MODIFICADOR", puntos: "5.00", permite_congelar: false, evidencia: null },
    { codigo: "R05_ESTRUCTURACION", regla_id: "r5", version: 1, tipo: "PRIMARIA", puntos: "90.00", permite_congelar: true, evidencia: { n: 3 } },
    { codigo: "M04_CUENTA_NUEVA", regla_id: "r4", version: 1, tipo: "MODIFICADOR", puntos: "10.00", permite_congelar: false, evidencia: null },
  ],
  primarias: ["R05_ESTRUCTURACION"],
  reglas_aplicadas: ["r5", "r3", "r4"],
  puntaje_bruto: puntajeBruto,
  puntaje,
  nivel: "CRITICO",
  dominante: "R05_ESTRUCTURACION",
  dominante_regla_id: "r5",
  congelar: true,
  relevante: false,
  regla_relevante_id: null,
  transacciones_patron: [],
  monto_patron: "435000.00000000",
});

test("factorRows widths stay in [0, 1], primaries first", () => {
  const rows = factorRows(calculo("105.00", "100.00"));
  assert.deepEqual(rows.map((r) => [r.codigo, r.tipo]), [
    ["R05_ESTRUCTURACION", "PRIMARIA"],
    ["M03_NIVEL_RIESGO", "MODIFICADOR"],
    ["M04_CUENTA_NUEVA", "MODIFICADOR"],
  ]);
  assert.ok(Math.abs(rows[0]!.width - 90 / 105) < 1e-9, "scaled by the raw total above 100");
  for (const r of rows) assert.ok(r.width >= 0 && r.width <= 1);
  const small = factorRows({ ...calculo("40.00", "40.00"), factores: [{ ...calculo("0", "0").factores[1]!, puntos: "250.00" }] });
  assert.equal(small[0]!.width, 1, "clamped");
  assert.equal(factorRows({ ...calculo("40.00", "40.00"), factores: [{ ...calculo("0", "0").factores[0]!, puntos: "40.00" }] })[0]!.width, 0.4);

  const evaluation = (puntaje: string | null, factores: EvaluacionAml["factores"]): EvaluacionAml => ({
    id: `e-${puntaje}`,
    creado_en: "2026-10-21T15:00:00+00:00",
    cliente_id: CLIENT,
    entidad: "IFPE",
    sentido: "ENTRADA",
    canal: "SPEI_DEMO",
    monto: "145000.00000000",
    transaccion_id: null,
    clave_operacion: "demo:1",
    contraparte: { nombre: null, cuenta: null, rfc_curp: null, cliente_id: null },
    puntaje,
    nivel: null,
    resultado: "ALERTADA",
    motivo_bloqueo: null,
    origen: "EN_LINEA",
    evaluacion_origen_id: null,
    factores,
    reglas_aplicadas: [],
    parametros_id: 1,
    monto_retener: "0.00000000",
  });
  const best = strongestEvaluation([
    evaluation("65.00", calculo("65.00", "65.00")),
    evaluation(null, { error: "timeout" }),
    evaluation(null, {}),
    evaluation("100.00", calculo("105.00", "100.00")),
  ]);
  assert.equal(best?.id, "e-100.00");
  assert.equal(strongestEvaluation([evaluation(null, {})]), null);
});

test("validateUifFolio checks the folio pattern and the date window", () => {
  const a = alert({ estado: "CONFIRMADO", timestamp: "2026-10-01T05:00:00+00:00" }); // Sep 30 in Mexico City
  const now = new Date("2026-10-21T18:00:00Z");
  assert.equal(validateUifFolio("SITI-2026-000123", "2026-10-21", a, now), null);
  assert.equal(validateUifFolio(" AB12 ", "2026-09-30", a, now), null, "trimmed; the alert's Mexico City date is allowed");
  assert.match(validateUifFolio("ab12", "2026-10-21", a, now) ?? "", /capital letters/);
  assert.match(validateUifFolio("AB1", "2026-10-21", a, now) ?? "", /4 to 40/);
  assert.match(validateUifFolio("A".repeat(41), "2026-10-21", a, now) ?? "", /4 to 40/);
  assert.match(validateUifFolio("SITI_1", "2026-10-21", a, now) ?? "", /hyphens/);
  assert.match(validateUifFolio("SITI-1", "2026-09-29", a, now) ?? "", /before the alert/);
  assert.match(validateUifFolio("SITI-1", "2026-10-22", a, now) ?? "", /future/);
  assert.match(validateUifFolio("SITI-1", "2026-02-30", a, now) ?? "", /date/);
  assert.match(validateUifFolio("SITI-1", "21/10/2026", a, now) ?? "", /date/);
  // 23:30 in Mexico City on Oct 21 is already Oct 22 in UTC: Oct 22 is still the future.
  assert.match(validateUifFolio("SITI-1", "2026-10-22", a, new Date("2026-10-22T05:30:00Z")) ?? "", /future/);
});

test("alert details are read defensively by origin", () => {
  const op = describeDetails({
    operacion: {
      entidad: "IFPE",
      sentido: "ENTRADA",
      canal: "SPEI_WEBHOOK",
      monto: "145000.00000000",
      transaccion_id: "t1",
      contraparte: { nombre: "ACME SA", cuenta: "012180001234567899", rfc_curp: null, cliente_id: null },
    },
  });
  assert.equal(op.kind, "operacion");
  if (op.kind === "operacion") {
    assert.equal(op.canal, "SPEI_WEBHOOK");
    assert.equal(op.contraparte?.cuenta, "012180001234567899");
  }
  assert.deepEqual(describeDetails({ operacion: { canal: "BOGUS", contraparte: null } }), {
    kind: "operacion",
    entidad: null,
    sentido: null,
    canal: null,
    monto: null,
    transaccion_id: null,
    contraparte: null,
  });
  assert.deepEqual(describeDetails({ motivo: "Lista de Personas Bloqueadas" }), { kind: "motivo", motivo: "Lista de Personas Bloqueadas" });
  assert.equal(describeDetails({ legado: { folio: 7, regla: "R02", analista: "ana" } }).kind, "legado");
  assert.deepEqual(describeDetails({ red: { clave: "cuenta:1", clientes: ["c1", 2] } }), { kind: "red", clave: "cuenta:1", clientes: ["c1"] });
  assert.deepEqual(describeDetails({}), { kind: "ninguno" });
  assert.deepEqual(describeCounterpartyKey("cuenta:012180001234567899"), { label: "Account", value: "012180001234567899" });
  assert.deepEqual(describeCounterpartyKey("cliente:abc"), { label: "Client", value: "abc" });
  assert.equal(describeCounterpartyKey(null), null);
});

test("client-facing copy never tips the client off", () => {
  assert.equal(containsTippingOffTerms(GENERIC_UNAVAILABLE_MESSAGE), false);
  assert.equal(containsTippingOffTerms(HOLD_COPY), false);
  assert.equal(containsTippingOffTerms(CLIENT_HOLD_LABELS.RETENCION), false);
  assert.equal(containsTippingOffTerms(CLIENT_HOLD_LABELS.LIBERACION_RETENCION), false);
  assert.equal(containsTippingOffTerms("Retención temporal"), false);
  assert.equal(containsTippingOffTerms("Liberación de retención"), false);
  assert.equal(containsTippingOffTerms("on compliance hold"), true);
  for (const text of ["AML review", "pld", "Reported to the UIF", "Alerts", "suspicious activity", "account frozen", "freeze", "CONGELADO_PREVENTIVO"]) {
    assert.equal(containsTippingOffTerms(text), true, text);
  }
  assert.equal(containsTippingOffTerms("Hamlet"), false, "whole words only for the acronyms");
});
