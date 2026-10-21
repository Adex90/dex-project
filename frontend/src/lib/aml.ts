/**
 * Framework-free logic of the compliance console (/admin/aml) and the
 * client-facing copy that must never tip a client off.
 *
 * - Ordering, merging and filtering of the polled alert feed (the server
 *   orders only for paging; the officer sees severity first).
 * - The actions an officer may take. These mirror the database rules of
 *   migration 0009 (aml.resolver_alerta, aml.decidir_levantamiento), which are
 *   the ones that decide: the console only hides what would be refused.
 * - Form validation with the same limits the backend applies.
 * - Deadlines in Mexico City time (fixed UTC-6, like the backend's hoy_cdmx).
 * - English labels for every backend enum (raw values are never shown).
 *
 * Pure and framework-free so `node --test` can import it (no React, no fetch,
 * no TS-only runtime syntax). "Now" is always an argument with a default.
 */
import type {
  AccionResolucionAml,
  ActorTipoAml,
  AlertaAml,
  CalculoAml,
  CanalAml,
  ContraparteAml,
  Decimal,
  DecisionAlertaAml,
  EntidadAml,
  EstadoAlertaAml,
  EstadoCongelamientoAml,
  EstadoCuentaInternaAml,
  EstadoRetencionAml,
  EstadoSolicitudAml,
  EvaluacionAml,
  IsoDate,
  IsoDateTime,
  NivelRiesgoAml,
  OrigenAlertaAml,
  OrigenCongelamientoAml,
  OrigenEvaluacionAml,
  ParametrosAml,
  PropuestaAml,
  ResolverAlertaInput,
  ResultadoEvaluacionAml,
  Rol,
  SentidoAlertaAml,
  SolicitudAml,
  TipoReporteAml,
  TipoSolicitudAml,
  Uuid,
} from "./api/types.ts";
import { compareAmounts } from "./decimal.ts";

// -----------------------------------------------------------------------------
// Constants
// -----------------------------------------------------------------------------

export const POLL_INTERVAL_MS = 10_000;
/** Overlap of each poll with the previous one: covers rows updated before `servidor_en` but committed after it. */
export const POLL_OVERLAP_MS = 60_000;
export const MAX_BACKOFF_MS = 60_000;
export const DICTAMEN_MIN = 30, DICTAMEN_MAX = 2000, COMENTARIO_MIN = 10, COMENTARIO_MAX = 2000;
/** Rows per feed request (the API maximum). A full poll page triggers an immediate follow-up. */
export const FEED_PAGE_SIZE = 200;

/**
 * The one message a client gets for any operation refused while their account
 * is restricted, whatever the cause (OPERACION_NO_DISPONIBLE). It must not hint
 * at compliance, alerts or freezes: Mexican AML rules forbid tipping off.
 */
export const GENERIC_UNAVAILABLE_MESSAGE = "This operation is not available right now. If you need help, please contact support.";
/** Vault card suffix for `saldo_bloqueado` ("$435,000.00 MXN on hold (still fully backed)"). */
export const HOLD_COPY = "on hold (still fully backed)";
/** Client ledger labels of the hold entries (their concepts are already neutral: "Retención temporal"). */
export const CLIENT_HOLD_LABELS = { RETENCION: "Funds on hold", LIBERACION_RETENCION: "Hold released" } as const;

/** Why an approve button is disabled for the requester or a false-positive proposer. */
export const SECOND_OFFICER_REQUIRED = "Requires a second compliance officer";

const OWN_RECORD = "You cannot act on alerts or restrictions that concern your own client record.";
const READ_ONLY = "Auditors have read-only access.";
const OFFICERS_ONLY = "Only compliance officers can make this decision.";

// -----------------------------------------------------------------------------
// Roles
// -----------------------------------------------------------------------------

/** Roles admitted to the console: CUMPLIMIENTO (full) and AUDITOR (read-only). */
export function isComplianceRole(role: Rol | null): boolean {
  return role === "CUMPLIMIENTO" || role === "AUDITOR";
}

/** Only compliance officers act; auditors see everything and change nothing. */
export function canAct(role: Rol | null): boolean {
  return role === "CUMPLIMIENTO";
}

// -----------------------------------------------------------------------------
// Risk levels and scores
// -----------------------------------------------------------------------------

export const LEVELS: readonly NivelRiesgoAml[] = ["BAJO", "MEDIO", "ALTO", "CRITICO"];
export const LEVEL_ORDER: Record<NivelRiesgoAml, number> = { BAJO: 0, MEDIO: 1, ALTO: 2, CRITICO: 3 };

const LEVEL_TONES: Record<NivelRiesgoAml, "danger" | "warning" | "info" | "neutral"> = {
  BAJO: "neutral",
  MEDIO: "info",
  ALTO: "warning",
  CRITICO: "danger",
};

export function levelTone(n: NivelRiesgoAml): "danger" | "warning" | "info" | "neutral" {
  return LEVEL_TONES[n];
}

export interface Thresholds {
  medio: Decimal;
  alto: Decimal;
  critico: Decimal;
}

/** The seeded parameters (version 1), used until GET /aml/reglas has been read. */
export const DEFAULT_THRESHOLDS: Thresholds = { medio: "30", alto: "60", critico: "85" };

export function thresholdsFromParams(p: Pick<ParametrosAml, "umbral_medio" | "umbral_alto" | "umbral_critico"> | null): Thresholds {
  return p ? { medio: p.umbral_medio, alto: p.umbral_alto, critico: p.umbral_critico } : DEFAULT_THRESHOLDS;
}

/** Mirrors aml.nivel_de_puntaje: < medio BAJO, < alto MEDIO, < critico ALTO, else CRITICO. */
export function levelFromScore(score: Decimal, t: Thresholds = DEFAULT_THRESHOLDS): NivelRiesgoAml {
  if (compareAmounts(score, t.medio) < 0) return "BAJO";
  if (compareAmounts(score, t.alto) < 0) return "MEDIO";
  if (compareAmounts(score, t.critico) < 0) return "ALTO";
  return "CRITICO";
}

/** "65.00" -> "65", "65.50" -> "65.5". */
export function formatScore(score: Decimal): string {
  return score.includes(".") ? score.replace(/\.?0+$/, "") : score;
}

/** A score (or threshold) as a position on a 0-100 meter. */
export function scorePercent(score: Decimal): number {
  const n = Number(score);
  return Number.isFinite(n) ? Math.min(100, Math.max(0, n)) : 0;
}

// -----------------------------------------------------------------------------
// Time
// -----------------------------------------------------------------------------

const HOUR_MS = 3_600_000;
const DAY_MS = 86_400_000;
/** Mexico City has been UTC-6 all year since daylight saving time was abolished in 2022. */
const MEXICO_CITY_OFFSET_MS = -6 * HOUR_MS;

/**
 * Microseconds since the epoch of an ISO-8601 timestamp. The API sends
 * microseconds ("…05.123456+00:00") and Date.parse keeps only milliseconds,
 * so two updates within the same millisecond would otherwise compare equal.
 */
export function instantMicros(iso: IsoDateTime): number {
  const ms = Date.parse(iso);
  if (Number.isNaN(ms)) return Number.NaN;
  const fraction = /T\d{2}:\d{2}:\d{2}\.(\d+)/.exec(iso)?.[1] ?? "";
  return ms * 1000 + Number(fraction.slice(3, 6).padEnd(3, "0"));
}

/** -1, 0 or 1 by instant (unparseable timestamps compare equal). */
export function compareInstants(a: IsoDateTime, b: IsoDateTime): number {
  const d = instantMicros(a) - instantMicros(b);
  return d > 0 ? 1 : d < 0 ? -1 : 0;
}

/** Calendar date in Mexico City ("YYYY-MM-DD") of an instant. */
export function mexicoCityDate(at: IsoDateTime | Date): IsoDate {
  const ms = typeof at === "string" ? Date.parse(at) : at.getTime();
  return new Date(ms + MEXICO_CITY_OFFSET_MS).toISOString().slice(0, 10);
}

/** Whole calendar days in Mexico City (fixed UTC-6, as the backend's hoy_cdmx) from today to the deadline; negative when overdue. */
export function daysToDeadline(fechaLimite: IsoDateTime, now: Date = new Date()): number {
  const deadline = Date.parse(`${mexicoCityDate(fechaLimite)}T00:00:00Z`);
  const today = Date.parse(`${mexicoCityDate(now)}T00:00:00Z`);
  return Math.round((deadline - today) / DAY_MS);
}

/** Whole hours left before the freeze review is due, or whole hours since it became overdue. */
export function slaRemaining(slaVenceEn: IsoDateTime, now: Date = new Date()): { overdue: boolean; hours: number } {
  const ms = Date.parse(slaVenceEn) - now.getTime();
  return { overdue: ms < 0, hours: Math.floor(Math.abs(ms) / HOUR_MS) };
}

const MEXICO_CITY = "America/Mexico_City";
const FORMATS = {
  datetime: new Intl.DateTimeFormat("en-US", {
    timeZone: MEXICO_CITY,
    year: "numeric",
    month: "short",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  }),
  date: new Intl.DateTimeFormat("en-US", { timeZone: MEXICO_CITY, year: "numeric", month: "short", day: "2-digit" }),
  time: new Intl.DateTimeFormat("en-US", {
    timeZone: MEXICO_CITY,
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hourCycle: "h23",
  }),
};

/** An instant in Mexico City time, e.g. "Oct 21, 2026, 09:04" / "Oct 21, 2026" / "09:04:31". */
export function formatMexicoCity(at: IsoDateTime | Date | number, style: keyof typeof FORMATS = "datetime"): string {
  const date = at instanceof Date ? at : new Date(at);
  return Number.isNaN(date.getTime()) ? "—" : FORMATS[style].format(date);
}

/** "just now", "5 min ago", "3 h ago", "2 d ago" (or "in 5 min" for the future). */
export function relativeTime(at: IsoDateTime, now: Date = new Date()): string {
  const ms = now.getTime() - Date.parse(at);
  if (Number.isNaN(ms)) return "—";
  const abs = Math.abs(ms);
  let text: string;
  if (abs < 45_000) return "just now";
  if (abs < HOUR_MS) text = `${Math.max(1, Math.round(abs / 60_000))} min`;
  else if (abs < DAY_MS) text = `${Math.floor(abs / HOUR_MS)} h`;
  else text = `${Math.floor(abs / DAY_MS)} d`;
  return ms >= 0 ? `${text} ago` : `in ${text}`;
}

// -----------------------------------------------------------------------------
// Feed: ordering, merging, filters and polling
// -----------------------------------------------------------------------------

export function isOpen(a: Pick<AlertaAml, "estado">): boolean {
  return a.estado === "PENDIENTE" || a.estado === "EN_REVISION";
}

/** Open (PENDIENTE, EN_REVISION) before closed; level desc; urgent first; fecha_limite_reporte asc; folio asc. */
export function sortAlerts(rows: AlertaAml[]): AlertaAml[] {
  return [...rows].sort(
    (a, b) =>
      Number(isOpen(b)) - Number(isOpen(a)) ||
      LEVEL_ORDER[b.nivel_riesgo] - LEVEL_ORDER[a.nivel_riesgo] ||
      Number(b.urgente) - Number(a.urgente) ||
      compareInstants(a.fecha_limite_reporte, b.fecha_limite_reporte) ||
      a.folio - b.folio,
  );
}

/** De-duplicates by id keeping the greater actualizado_en (Date.parse); reports new and updated ids. */
export function mergeFeed(
  prev: AlertaAml[],
  incoming: AlertaAml[],
): { alertas: AlertaAml[]; nuevas: Uuid[]; actualizadas: Uuid[] } {
  const alertas = prev.slice();
  const index = new Map<Uuid, number>(alertas.map((a, i) => [a.id, i]));
  const known = new Set(index.keys());
  const nuevas: Uuid[] = [];
  const actualizadas = new Set<Uuid>();
  for (const a of incoming) {
    const i = index.get(a.id);
    if (i === undefined) {
      index.set(a.id, alertas.length);
      alertas.push(a);
      nuevas.push(a.id);
    } else if (compareInstants(a.actualizado_en, alertas[i]!.actualizado_en) > 0) {
      alertas[i] = a;
      if (known.has(a.id)) actualizadas.add(a.id);
    }
  }
  return { alertas, nuevas, actualizadas: [...actualizadas] };
}

export const ALERT_STATES: readonly EstadoAlertaAml[] = ["PENDIENTE", "EN_REVISION", "CONFIRMADO", "FALSO_POSITIVO"];
const DEFAULT_STATES: readonly EstadoAlertaAml[] = ["PENDIENTE", "EN_REVISION"];

/** An empty `estados` or `niveles` list means "all". */
export interface AlertFilters { estados: EstadoAlertaAml[]; niveles: NivelRiesgoAml[]; regla: string | null; asignada: "yo" | "nadie" | null }

/** Open alerts of every level, rule and assignee (the API's page-mode default). */
export function defaultFilters(): AlertFilters {
  return { estados: [...DEFAULT_STATES], niveles: [], regla: null, asignada: null };
}

/** Query keys shared by the feed request and the page URL. */
export type AlertFilterQuery = {
  estado: string | undefined;
  nivel: string | undefined;
  regla: string | undefined;
  asignada: "yo" | "nadie" | undefined;
};

function canonical<T extends string>(values: readonly string[], order: readonly T[]): T[] {
  const wanted = new Set(values.map((v) => v.trim().toUpperCase()));
  return order.filter((v) => wanted.has(v));
}

function sameList(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

const RULE_CODE_MAX = 64;

export function matchesFilters(a: AlertaAml, f: AlertFilters, me: Uuid): boolean {
  if (f.estados.length > 0 && !f.estados.includes(a.estado)) return false;
  if (f.niveles.length > 0 && !f.niveles.includes(a.nivel_riesgo)) return false;
  if (f.regla && a.regla.codigo !== f.regla) return false;
  if (f.asignada === "yo" && a.asignada_a?.id !== me) return false;
  if (f.asignada === "nadie" && a.asignada_a !== null) return false;
  return true;
}

/** Feed query and URL parameters. The default states are omitted (the API defaults to them); none means TODOS. */
export function filtersToQuery(f: AlertFilters): AlertFilterQuery {
  const estados = canonical(f.estados, ALERT_STATES);
  const niveles = canonical(f.niveles, LEVELS);
  const regla = f.regla?.trim().slice(0, RULE_CODE_MAX);
  return {
    estado: estados.length === 0 ? "TODOS" : sameList(estados, DEFAULT_STATES) ? undefined : estados.join(","),
    nivel: niveles.length > 0 ? niveles.join(",") : undefined,
    regla: regla ? regla : undefined,
    asignada: f.asignada ?? undefined,
  };
}

/** Inverse of `filtersToQuery`; unknown values are ignored. */
export function filtersFromSearch(p: URLSearchParams): AlertFilters {
  const rawStates = p.get("estado")?.trim() ?? "";
  let estados: EstadoAlertaAml[];
  if (rawStates === "") estados = [...DEFAULT_STATES];
  else if (rawStates.toUpperCase() === "TODOS") estados = [];
  else {
    const parsed = canonical(rawStates.split(","), ALERT_STATES);
    estados = parsed.length > 0 ? parsed : [...DEFAULT_STATES];
  }
  const niveles = canonical((p.get("nivel") ?? "").split(","), LEVELS);
  const regla = p.get("regla")?.trim().slice(0, RULE_CODE_MAX);
  const asignada = p.get("asignada");
  return {
    estados,
    niveles,
    regla: regla ? regla : null,
    asignada: asignada === "yo" || asignada === "nadie" ? asignada : null,
  };
}

function fold(s: string): string {
  return s.normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase();
}

/** Client-side search box: folio ("42" or "#42"), client name (accents ignored) or client id prefix. */
export function matchesSearch(a: AlertaAml, text: string): boolean {
  const q = text.trim();
  if (q === "") return true;
  const folio = /^#?(\d+)$/.exec(q);
  if (folio && Number(folio[1]) === a.folio) return true;
  if (fold(a.cliente.nombre_completo).includes(fold(q))) return true;
  return q.length >= 4 && a.cliente.id.toLowerCase().startsWith(q.toLowerCase());
}

/** Poll cursor: the previous response's `servidor_en` minus POLL_OVERLAP_MS. */
export function nextPollCursor(servidorEn: IsoDateTime): IsoDateTime {
  return new Date(Date.parse(servidorEn) - POLL_OVERLAP_MS).toISOString();
}

/** min(POLL_INTERVAL_MS × 2^failures, MAX_BACKOFF_MS): 10 s, 20 s, 40 s, 60 s… */
export function nextPollDelay(failures: number): number {
  return Math.min(POLL_INTERVAL_MS * 2 ** Math.max(0, failures), MAX_BACKOFF_MS);
}

// -----------------------------------------------------------------------------
// What an officer may do (mirrors §2.9; the database has the final word)
// -----------------------------------------------------------------------------

/**
 * Mirrors §2.9: AUDITOR or own client record → []; PENDIENTE → [TOMAR]; EN_REVISION held by me → [LIBERAR, ESCALAR,
 * CONFIRMAR, FALSO_POSITIVO], or [ESCALAR, CONFIRMAR] while an FP proposal is pending; anything else → [].
 */
export function allowedActions(role: Rol | null, me: Uuid, a: AlertaAml): AccionResolucionAml[] {
  if (!canAct(role) || a.cliente.id === me) return [];
  if (a.estado === "PENDIENTE") return ["TOMAR"];
  if (a.estado === "EN_REVISION" && a.asignada_a?.id === me) {
    return a.resolucion_propuesta === "FALSO_POSITIVO" ? ["ESCALAR", "CONFIRMAR"] : ["LIBERAR", "ESCALAR", "CONFIRMAR", "FALSO_POSITIVO"];
  }
  return [];
}

/** CONFIRMADO, no folio yet, and not about the officer's own client record. */
export function canRecordUifFolio(role: Rol | null, me: Uuid, a: AlertaAml): boolean {
  return canAct(role) && a.estado === "CONFIRMADO" && a.folio_uif === null && a.cliente.id !== me;
}

/** Trims; \n allowed, other control characters rejected (as `char::is_control` in the backend). Length in characters. */
export function validateNarrative(text: string, min: number, max: number): string | null {
  const trimmed = text.trim();
  for (const ch of trimmed) {
    const c = ch.codePointAt(0)!;
    if ((c < 0x20 && c !== 0x0a) || (c >= 0x7f && c <= 0x9f)) {
      return "Remove tabs and other control characters; only line breaks are allowed.";
    }
  }
  const n = [...trimmed].length;
  if (n < min) return `Write at least ${min} characters (${n} so far).`;
  if (n > max) return `Use at most ${max} characters (${n} now).`;
  return null;
}

const NEEDS_DICTAMEN: ReadonlySet<AccionResolucionAml> = new Set(["ESCALAR", "CONFIRMAR", "FALSO_POSITIVO"]);

function provided(value: unknown): boolean {
  return value !== undefined && value !== null;
}

/**
 * English messages; [] when valid. Dictamen 30–2000 for ESCALAR/CONFIRMAR/FALSO_POSITIVO and absent otherwise;
 * tipo_reporte/urgente only with CONFIRMAR; congelar only with ESCALAR/CONFIRMAR;
 * tipo_levantamiento only with FALSO_POSITIVO on an ACTIVO freeze.
 *
 * `clientRestricted` is whether the client has an ACTIVO freeze (the case file's `congelamiento`); it defaults to
 * the alert's own freeze link.
 */
export function validateResolution(
  input: ResolverAlertaInput,
  a: AlertaAml,
  clientRestricted: boolean = a.congelamiento?.estado === "ACTIVO",
): string[] {
  const errors: string[] = [];
  if (NEEDS_DICTAMEN.has(input.accion)) {
    const problem = validateNarrative(input.dictamen ?? "", DICTAMEN_MIN, DICTAMEN_MAX);
    if (problem) errors.push(`Dictamen: ${problem}`);
  } else if (provided(input.dictamen)) {
    errors.push("A dictamen is only recorded when escalating, confirming or clearing an alert.");
  }
  if (provided(input.tipo_reporte) && input.accion !== "CONFIRMAR") {
    errors.push("The report type only applies when confirming suspicious activity.");
  }
  if (provided(input.urgente) && input.accion !== "CONFIRMAR") {
    errors.push("The urgent 24-hour report only applies when confirming suspicious activity.");
  }
  if (provided(input.congelar) && input.accion !== "ESCALAR" && input.accion !== "CONFIRMAR") {
    errors.push("Restricting the client only applies when escalating or confirming.");
  }
  if (provided(input.tipo_levantamiento)) {
    if (input.accion !== "FALSO_POSITIVO") {
      errors.push("What happens to held funds is only chosen when clearing a false positive.");
    } else if (!clientRestricted) {
      errors.push("The client is not restricted, so there are no held funds to decide on.");
    }
  }
  return errors;
}

const UIF_FOLIO = /^[A-Z0-9-]{4,40}$/;

/** Folio of the SITI report (^[A-Z0-9-]{4,40}$) and its date: between the alert's date and today, in Mexico City. */
export function validateUifFolio(folio: string, reportadaEn: IsoDate, a: AlertaAml, now: Date = new Date()): string | null {
  if (!UIF_FOLIO.test(folio.trim())) return "The UIF folio has 4 to 40 characters: capital letters, digits and hyphens.";
  const date = /^(\d{4})-(\d{2})-(\d{2})$/.exec(reportadaEn);
  if (!date || new Date(`${reportadaEn}T00:00:00Z`).toISOString().slice(0, 10) !== reportadaEn) {
    return "Enter the date the report was filed.";
  }
  const from = mexicoCityDate(a.timestamp);
  if (reportadaEn < from) return `The report date cannot be before the alert was raised (${from}).`;
  if (reportadaEn > mexicoCityDate(now)) return "The report date cannot be in the future.";
  return null;
}

/** Not allowed for AUDITOR, for the requester, for the frozen client, or for anyone holding an FP proposal on that client. */
export function canDecideRequest(
  role: Rol | null,
  me: Uuid,
  s: SolicitudAml,
  alertsOfClient: AlertaAml[],
): { allowed: boolean; reason: string | null } {
  if (!canAct(role)) return { allowed: false, reason: role === "AUDITOR" ? READ_ONLY : OFFICERS_ONLY };
  if (s.estado !== "PENDIENTE") return { allowed: false, reason: "This request has already been decided." };
  if (s.cliente.id === me) return { allowed: false, reason: OWN_RECORD };
  if (s.solicitada_por.id === me) return { allowed: false, reason: SECOND_OFFICER_REQUIRED };
  const proposer = alertsOfClient.some(
    (a) => a.cliente.id === s.cliente.id && a.resolucion_propuesta === "FALSO_POSITIVO" && a.propuesta_por?.id === me,
  );
  if (proposer) return { allowed: false, reason: SECOND_OFFICER_REQUIRED };
  return { allowed: true, reason: null };
}

export function canDecideProposal(role: Rol | null, me: Uuid, p: PropuestaAml): { allowed: boolean; reason: string | null } {
  if (!canAct(role)) return { allowed: false, reason: role === "AUDITOR" ? READ_ONLY : OFFICERS_ONLY };
  if (p.estado !== "PENDIENTE") return { allowed: false, reason: "This proposal has already been decided." };
  if (p.propuesta_por.id === me) return { allowed: false, reason: SECOND_OFFICER_REQUIRED };
  return { allowed: true, reason: null };
}

// -----------------------------------------------------------------------------
// Case file helpers
// -----------------------------------------------------------------------------

/** Evaluation details that hold a full score (not `{ error }` nor the `{}` of markers and blocked attempts). */
export function isCalculo(f: EvaluacionAml["factores"]): f is CalculoAml {
  return typeof f === "object" && f !== null && Array.isArray((f as Partial<CalculoAml>).factores);
}

export function isScoringError(f: EvaluacionAml["factores"]): f is { error: string } {
  return typeof f === "object" && f !== null && typeof (f as { error?: unknown }).error === "string";
}

/** The evaluation with the highest score that carries a full breakdown, or null. */
export function strongestEvaluation(evaluaciones: EvaluacionAml[]): (EvaluacionAml & { factores: CalculoAml }) | null {
  let best: (EvaluacionAml & { factores: CalculoAml }) | null = null;
  for (const e of evaluaciones) {
    if (!isCalculo(e.factores)) continue;
    const candidate = e as EvaluacionAml & { factores: CalculoAml };
    if (!best || compareAmounts(candidate.factores.puntaje, best.factores.puntaje) > 0) best = candidate;
  }
  return best;
}

/** One bar per factor, primaries first; width = puntos / max(100, puntaje_bruto), within [0, 1]. */
export function factorRows(c: CalculoAml): { codigo: string; tipo: "PRIMARIA" | "MODIFICADOR"; puntos: Decimal; width: number }[] {
  const scale = Math.max(100, Number(c.puntaje_bruto) || 0);
  return c.factores
    .map((f) => {
      const ratio = Number(f.puntos) / scale;
      return { codigo: f.codigo, tipo: f.tipo, puntos: f.puntos, width: Number.isFinite(ratio) ? Math.min(1, Math.max(0, ratio)) : 0 };
    })
    .sort((a, b) => Number(a.tipo === "MODIFICADOR") - Number(b.tipo === "MODIFICADOR"));
}

function str(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
}

function oneOf<T extends string>(value: unknown, allowed: readonly T[]): T | null {
  return typeof value === "string" && (allowed as readonly string[]).includes(value) ? (value as T) : null;
}

const CHANNELS: readonly CanalAml[] = [
  "SPEI_WEBHOOK",
  "SPEI_OPERADOR",
  "SPEI_DEMO",
  "TRANSFERENCIA_RECIBIDA",
  "TRANSFERENCIA_ENVIADA",
  "SPEI_ENVIADO",
  "INVERSION_MUTUO",
  "DESEMBOLSO_CREDITO",
  "COBRO_CREDITO",
];

/** What `alerta.detalles` says about the alert, by origin. */
export type AlertDetails =
  | {
      kind: "operacion";
      entidad: EntidadAml | null;
      sentido: "ENTRADA" | "SALIDA" | null;
      canal: CanalAml | null;
      monto: Decimal | null;
      transaccion_id: Uuid | null;
      contraparte: ContraparteAml | null;
    }
  | { kind: "motivo"; motivo: string }
  | {
      kind: "legado";
      folio: number | null;
      regla: string | null;
      descripcion: string | null;
      detalle: string | null;
      analista: string | null;
      dictamen: string | null;
    }
  | { kind: "red"; clave: string | null; clientes: string[] }
  | { kind: "ninguno" };

export function describeDetails(detalles: Record<string, unknown>): AlertDetails {
  const op = record(detalles.operacion);
  if (op) {
    const c = record(op.contraparte);
    return {
      kind: "operacion",
      entidad: oneOf(op.entidad, ["IFPE", "SOFOM"] as const),
      sentido: oneOf(op.sentido, ["ENTRADA", "SALIDA"] as const),
      canal: oneOf(op.canal, CHANNELS),
      monto: str(op.monto),
      transaccion_id: str(op.transaccion_id),
      contraparte: c
        ? { nombre: str(c.nombre), cuenta: str(c.cuenta), rfc_curp: str(c.rfc_curp), cliente_id: str(c.cliente_id) }
        : null,
    };
  }
  const legacy = record(detalles.legado);
  if (legacy) {
    return {
      kind: "legado",
      folio: typeof legacy.folio === "number" ? legacy.folio : null,
      regla: str(legacy.regla),
      descripcion: str(legacy.descripcion),
      detalle: typeof legacy.detalle === "string" ? legacy.detalle : legacy.detalle == null ? null : JSON.stringify(legacy.detalle),
      analista: str(legacy.analista),
      dictamen: str(legacy.dictamen),
    };
  }
  const network = record(detalles.red);
  if (network) {
    const clientes = Array.isArray(network.clientes) ? network.clientes.filter((x): x is string => typeof x === "string") : [];
    return { kind: "red", clave: str(network.clave), clientes };
  }
  const motivo = str(detalles.motivo);
  if (motivo) return { kind: "motivo", motivo };
  return { kind: "ninguno" };
}

/** "cuenta:012180001234567899" -> { label: "Account", value: "012180001234567899" }. */
export function describeCounterpartyKey(key: string | null): { label: string; value: string } | null {
  if (!key) return null;
  const i = key.indexOf(":");
  if (i < 0) return { label: "Counterparty", value: key };
  const kind = key.slice(0, i);
  const labels: Record<string, string> = { cuenta: "Account", cliente: "Client", clabe: "CLABE", rfc: "RFC", curp: "CURP" };
  return { label: labels[kind] ?? "Counterparty", value: key.slice(i + 1) };
}

// -----------------------------------------------------------------------------
// English labels (backend enum values are never shown raw)
// -----------------------------------------------------------------------------

export const LEVEL_LABEL: Record<NivelRiesgoAml, string> = { BAJO: "Low", MEDIO: "Medium", ALTO: "High", CRITICO: "Critical" };

export const STATE_LABEL: Record<EstadoAlertaAml, string> = {
  PENDIENTE: "Pending",
  EN_REVISION: "In review",
  CONFIRMADO: "Confirmed",
  FALSO_POSITIVO: "False positive",
};

export const DECISION_LABEL: Record<DecisionAlertaAml, string> = { REGISTRADA: "Recorded", RETENIDA: "Held", BLOQUEADA: "Blocked" };

export const CHANNEL_LABEL: Record<CanalAml, string> = {
  SPEI_WEBHOOK: "SPEI received",
  SPEI_OPERADOR: "SPEI (operator capture)",
  SPEI_DEMO: "Demo deposit",
  TRANSFERENCIA_RECIBIDA: "Transfer received",
  TRANSFERENCIA_ENVIADA: "Transfer sent",
  SPEI_ENVIADO: "SPEI sent",
  INVERSION_MUTUO: "Mutuum investment",
  DESEMBOLSO_CREDITO: "Credit disbursement",
  COBRO_CREDITO: "Credit repayment",
};

export const REQUEST_TYPE_LABEL: Record<TipoSolicitudAml, string> = {
  LEVANTAR_Y_LIBERAR: "Lift and release funds",
  LEVANTAR_CONSERVANDO_RETENCIONES: "Lift and keep funds held",
  LIBERAR_RETENCIONES: "Release kept funds",
};

export const ORIGIN_LABEL: Record<OrigenAlertaAml, string> = {
  EN_LINEA: "Real-time scoring",
  INTENTO_BLOQUEADO: "Blocked attempt",
  LOTE: "Batch monitor",
  DIFERIDA: "Deferred scoring",
  MANUAL: "Manual",
  LISTA_BLOQUEADAS: "Blocked-persons list",
  LEGADO: "Legacy (before this module)",
};

export const FREEZE_ORIGIN_LABEL: Record<OrigenCongelamientoAml, string> = {
  AUTOMATICO: "Automatic",
  LOTE: "Batch monitor",
  MANUAL: "Manual",
  LISTA_BLOQUEADAS: "Blocked-persons list",
};

export const FREEZE_STATE_LABEL: Record<EstadoCongelamientoAml, string> = { ACTIVO: "Active", LEVANTADO: "Lifted" };

export const REPORT_TYPE_LABEL: Record<TipoReporteAml, string> = {
  INUSUAL: "Unusual operation",
  INTERNA_PREOCUPANTE: "Internal concern",
};

export const REQUEST_STATE_LABEL: Record<EstadoSolicitudAml, string> = {
  PENDIENTE: "Pending",
  APROBADA: "Approved",
  RECHAZADA: "Rejected",
  ANULADA: "Withdrawn",
};

export const HOLD_STATE_LABEL: Record<EstadoRetencionAml, string> = { VIGENTE: "Held", LIBERADA: "Released" };

/** Staff view: the AML freeze is visible here (clients see ACTIVA). */
export const ACCOUNT_STATE_LABEL: Record<EstadoCuentaInternaAml, string> = {
  ACTIVA: "Active",
  CONGELADA: "Frozen (legal order)",
  CONGELADO_PREVENTIVO: "Preventive restriction",
  CERRADA: "Closed",
};

export const DIRECTION_LABEL: Record<SentidoAlertaAml, string> = { ENTRADA: "Incoming", SALIDA: "Outgoing", NINGUNO: "Not applicable" };

export const RESULT_LABEL: Record<ResultadoEvaluacionAml, string> = {
  PERMITIDA: "Allowed",
  ALERTADA: "Alerted",
  RETENIDA: "Held",
  BLOQUEADA: "Blocked",
  PENDIENTE_REEVALUACION: "Awaiting re-scoring",
  REEVALUADA: "Re-scored",
};

export const EVALUATION_ORIGIN_LABEL: Record<OrigenEvaluacionAml, string> = {
  EN_LINEA: "Real-time",
  DIFERIDA: "Deferred",
  REEVALUACION: "Re-evaluation",
};

export const BLOCK_REASON_LABEL: Record<"CONGELADO" | "PUNTAJE_CRITICO", string> = {
  CONGELADO: "Client already restricted",
  PUNTAJE_CRITICO: "Critical score",
};

export const ACTOR_TYPE_LABEL: Record<ActorTipoAml, string> = {
  SISTEMA: "System",
  CUMPLIMIENTO: "Compliance officer",
  AUDITOR: "Auditor",
  OPERADOR: "Operator",
  MIGRACION: "Migration",
};

export const KYC_STATUS_LABEL: Record<"PENDIENTE" | "EN_REVISION" | "VERIFICADO" | "RECHAZADO" | "SUSPENDIDO", string> = {
  PENDIENTE: "Pending",
  EN_REVISION: "In review",
  VERIFICADO: "Verified",
  RECHAZADO: "Rejected",
  SUSPENDIDO: "Suspended",
};

export const ENTITY_LABEL: Record<EntidadAml, string> = { IFPE: "Vault (IFPE)", SOFOM: "Credit (SOFOM)" };

/** Audit-log events (aml.bitacora.evento). */
export const EVENT_LABEL: Record<string, string> = {
  ALERTA_CREADA: "Alert raised",
  ALERTA_AGRUPADA: "New occurrence grouped",
  ALERTA_TOMADA: "Taken for review",
  ALERTA_LIBERADA: "Returned to the queue",
  ALERTA_ESCALADA: "Escalated",
  ALERTA_FP_PROPUESTO: "False positive proposed",
  ALERTA_FP_RETIRADO: "False-positive proposal withdrawn",
  ALERTA_CONFIRMADA: "Suspicious activity confirmed",
  ALERTA_FALSO_POSITIVO: "Cleared as false positive",
  REPORTE_UIF_REGISTRADO: "UIF folio recorded",
  CONGELAMIENTO_APLICADO: "Restriction applied",
  CONGELAMIENTO_CONFIRMADO: "Restriction confirmed",
  CONGELAMIENTO_LEVANTADO: "Restriction lifted",
  RETENCION_APLICADA: "Funds held",
  RETENCION_LIBERADA: "Held funds released",
  LEVANTAMIENTO_SOLICITADO: "Unfreeze requested",
  LEVANTAMIENTO_APROBADO: "Unfreeze approved",
  LEVANTAMIENTO_RECHAZADO: "Unfreeze rejected",
  LEVANTAMIENTO_ANULADO: "Unfreeze request withdrawn",
  OPERACION_BLOQUEADA: "Operation blocked",
  CARGO_EN_CUENTA_CONGELADA: "Debit on a restricted account",
  EVALUACION_DIFERIDA: "Scoring deferred",
  EVALUACION_FALLIDA: "Scoring failed",
  CONSULTA_EXPEDIENTE: "Case file viewed",
  RIESGO_CLIENTE_ELEVADO: "Client risk tier raised",
  PROPUESTA_CREADA: "Change proposed",
  PROPUESTA_APROBADA: "Change approved",
  PROPUESTA_RECHAZADA: "Change rejected",
  REGLA_VERSION_NUEVA: "New rule version",
  REGLA_DESACTIVADA: "Rule deactivated",
  PARAMETROS_VERSION_NUEVA: "New parameters version",
  MIGRACION_0009: "Compliance module installed",
};

/** "SOME_CODE" -> "Some code": last resort for a value without a label. */
export function humanize(code: string): string {
  const text = code.replace(/_/g, " ").toLowerCase();
  return text.charAt(0).toUpperCase() + text.slice(1);
}

export function eventLabel(evento: string): string {
  return EVENT_LABEL[evento] ?? humanize(evento);
}

const ANY_STATE_LABEL: Record<string, string> = {
  ...LEVEL_LABEL,
  ...HOLD_STATE_LABEL,
  ...FREEZE_STATE_LABEL,
  ...REQUEST_STATE_LABEL,
  ...ACCOUNT_STATE_LABEL,
  ...STATE_LABEL,
};

/** Label of a state recorded in the audit log (alert, restriction, request, hold, account or tier). */
export function stateLabel(value: string): string {
  return ANY_STATE_LABEL[value] ?? humanize(value);
}

// -----------------------------------------------------------------------------
// No tipping off
// -----------------------------------------------------------------------------

const TIPPING_OFF_WORDS = /\b(aml|pld|uif)\b/i;
const TIPPING_OFF_STEMS = /(compliance|alert|suspicious|report|frozen|freez|cumplimiento|sospech|congel)/i;

/** Client-facing copy guard used by the tests: true if the text mentions aml, pld, uif, compliance, alert, suspicious, report, frozen or freeze. */
export function containsTippingOffTerms(text: string): boolean {
  return TIPPING_OFF_WORDS.test(text) || TIPPING_OFF_STEMS.test(text);
}
