import { request } from "./client";
import type {
  AmlAlertsQuery,
  Cliente,
  CongelamientoManualAml,
  CongelarClienteInput,
  CorteReserva,
  DatosGuardados,
  DatosPersonalesInput,
  DecisionInput,
  DecisionLevantamientoAml,
  DecisionPropuestaAml,
  DetalleAlertaAml,
  EnvioVerificacion,
  EstadoReservaVista,
  IsoDateTime,
  ListaAlertasAml,
  ListaCongelamientosAml,
  ListaNotificacionesAml,
  ListaPropuestasAml,
  ListaRelevantesAml,
  ListaSolicitudesAml,
  Movimiento,
  PlazoTasa,
  PropuestaCreadaAml,
  PropuestaInput,
  PruebaReserva,
  ReglasAml,
  RegistroInput,
  RegistroRespuesta,
  ReporteUifAml,
  ReporteUifInput,
  ResolucionAlertaAml,
  ResolverAlertaInput,
  ResultadoLiquidacion,
  ResultadoMutuo,
  ResultadoVerificacionPrueba,
  ResumenAml,
  RevisionClienteAml,
  Sesion,
  SolicitudCreadaAml,
  SolicitudLevantamientoInput,
  SolicitudVerificacion,
  Tablero,
  TransaccionIfpe,
  Uuid,
  VerificacionBitacoraAml,
  VistaExpediente,
} from "./types";

const AML = "/api/v1/aml";

/** One function per backend route used by the client app (see backend/src/routes). */
export const api = {
  register: (input: RegistroInput) => request<RegistroRespuesta>("/api/v1/auth/registro", { method: "POST", body: input }),

  login: (email: string, password: string) =>
    request<Sesion>("/api/v1/auth/login", { method: "POST", body: { email, password } }),

  dashboard: (token: string, signal?: AbortSignal) => request<Tablero>("/api/v1/tablero", { token, signal }),

  movements: (token: string, params: { limit?: number; before?: number } = {}, signal?: AbortSignal) =>
    request<TransaccionIfpe[]>("/api/v1/ifpe/movimientos", {
      token,
      signal,
      query: { limite: params.limit, antes_de: params.before },
    }),

  simulatedDeposit: (token: string, monto: string, clave_idempotencia: string) =>
    request<Movimiento>("/api/v1/ifpe/depositos/simulados", { method: "POST", token, body: { monto, clave_idempotencia } }),

  rates: () => request<PlazoTasa[]>("/api/v1/sofom/tasas"),

  /** IFPE -> SOFOM bridge: locks vault funds into a fixed-term mutuum contract. */
  invest: (token: string, body: { monto: string; plazo_dias: number; clave_idempotencia: string }) =>
    request<ResultadoMutuo>("/api/v1/sofom/mutuos", { method: "POST", token, body }),

  settle: (token: string, contractId: string) =>
    request<ResultadoLiquidacion>(`/api/v1/sofom/mutuos/${encodeURIComponent(contractId)}/liquidar`, {
      method: "POST",
      token,
    }),

  reserveStatus: (signal?: AbortSignal) => request<EstadoReservaVista>("/api/v1/reserva/estado", { signal }),

  reserveProof: (token: string) => request<PruebaReserva>("/api/v1/reserva/prueba", { token }),

  /** Public copy of a published cut, fetched without credentials. */
  publicCut: (id: string) => request<CorteReserva>(`/api/v1/reserva/cortes/${encodeURIComponent(id)}`),

  verifyProof: (proof: PruebaReserva) =>
    request<ResultadoVerificacionPrueba>("/api/v1/reserva/verificar", {
      method: "POST",
      body: { corte_id: proof.corte.id, hoja: proof.hoja, ruta: proof.ruta },
    }),

  /** The signed-in client (its `estatus_kyc` tells a SUSPENDIDO client from a rejected one). */
  me: (token: string, signal?: AbortSignal) => request<Cliente>("/api/v1/yo", { token, signal }),

  /** The customer's KYC file: state, attempts left and the last biometric result. */
  kycFile: (token: string, signal?: AbortSignal) => request<VistaExpediente>("/api/v1/kyc/expediente", { token, signal }),

  /** Step 1: personal data and address (only while the file is PENDIENTE). */
  kycSaveProfile: (token: string, body: DatosPersonalesInput) =>
    request<DatosGuardados>("/api/v1/kyc/perfil", { method: "POST", token, body }),

  /**
   * Steps 2-3: credential + selfie frames. Resolves on 200 (`resultado` set)
   * and on 202 (`resultado: null`: the provider answers later, poll kycFile).
   */
  kycVerify: (token: string, body: SolicitudVerificacion, signal?: AbortSignal) =>
    request<EnvioVerificacion>("/api/v1/kyc/verificar", { method: "POST", token, body, signal }),

  // ---------------------------------------------------------------------------
  // Compliance console (/admin/aml): CUMPLIMIENTO and AUDITOR only; every POST
  // is CUMPLIMIENTO only. The API enforces the role on every route.
  // ---------------------------------------------------------------------------

  /** Counters for the summary bar. */
  amlSummary: (token: string, signal?: AbortSignal) => request<ResumenAml>(`${AML}/resumen`, { token, signal }),

  /** Alert feed: page mode (folio cursor) or, with `actualizadas_desde`, poll mode. */
  amlAlerts: (token: string, q: AmlAlertsQuery, signal?: AbortSignal) =>
    request<ListaAlertasAml>(`${AML}/alertas`, {
      token,
      signal,
      query: {
        estado: q.estado,
        nivel: q.nivel,
        regla: q.regla,
        cliente_id: q.cliente_id,
        asignada: q.asignada,
        antes_de_folio: q.antes_de_folio,
        actualizadas_desde: q.actualizadas_desde,
        limite: q.limite,
      },
    }),

  /** Case file of an alert. Each read is logged in the audit trail (CONSULTA_EXPEDIENTE). */
  amlAlert: (token: string, id: Uuid, signal?: AbortSignal) =>
    request<DetalleAlertaAml>(`${AML}/alertas/${encodeURIComponent(id)}`, { token, signal }),

  /** TOMAR, LIBERAR, ESCALAR, CONFIRMAR (suspicious-activity report) or FALSO_POSITIVO. */
  amlResolve: (token: string, id: Uuid, body: ResolverAlertaInput) =>
    request<ResolucionAlertaAml>(`${AML}/alertas/${encodeURIComponent(id)}/resolver`, { method: "POST", token, body }),

  /** Records the UIF (SITI) folio of a CONFIRMADO alert, once. */
  amlRecordUifReport: (token: string, id: Uuid, body: ReporteUifInput) =>
    request<ReporteUifAml>(`${AML}/alertas/${encodeURIComponent(id)}/reporte-uif`, { method: "POST", token, body }),

  /** Client review (KYC, accounts, aggregates, orders, alerts, freezes). Logged like the case file. */
  amlClient: (token: string, id: Uuid, signal?: AbortSignal) =>
    request<RevisionClienteAml>(`${AML}/clientes/${encodeURIComponent(id)}`, { token, signal }),

  /** Manual freeze: one officer may tighten. An existing ACTIVO freeze comes back with `ya_congelado`. */
  amlFreezeClient: (token: string, clientId: Uuid, body: CongelarClienteInput) =>
    request<CongelamientoManualAml>(`${AML}/clientes/${encodeURIComponent(clientId)}/congelar`, {
      method: "POST",
      token,
      body,
    }),

  /** Freezes, ACTIVO by default (`estado` = ACTIVO | LEVANTADO | TODOS). */
  amlFreezes: (
    token: string,
    q: { estado?: string; cliente_id?: Uuid; antes_de_folio?: number; limite?: number },
    signal?: AbortSignal,
  ) =>
    request<ListaCongelamientosAml>(`${AML}/congelamientos`, {
      token,
      signal,
      query: { estado: q.estado, cliente_id: q.cliente_id, antes_de_folio: q.antes_de_folio, limite: q.limite },
    }),

  /** Four-eyes request: lift the freeze (releasing or keeping the holds) or release kept holds. */
  amlRequestUnfreeze: (token: string, freezeId: Uuid, body: SolicitudLevantamientoInput) =>
    request<SolicitudCreadaAml>(`${AML}/congelamientos/${encodeURIComponent(freezeId)}/solicitudes`, {
      method: "POST",
      token,
      body,
    }),

  /** Four-eyes queue, PENDIENTE by default. */
  amlRequests: (token: string, q: { estado?: string; limite?: number }, signal?: AbortSignal) =>
    request<ListaSolicitudesAml>(`${AML}/solicitudes`, { token, signal, query: { estado: q.estado, limite: q.limite } }),

  /** Second officer's decision on an unfreeze or release request. */
  amlDecideRequest: (token: string, requestId: Uuid, body: DecisionInput) =>
    request<DecisionLevantamientoAml>(`${AML}/solicitudes/${encodeURIComponent(requestId)}/decision`, {
      method: "POST",
      token,
      body,
    }),

  /** Outbox feed after an id (exclusive). */
  amlNotifications: (token: string, q: { despues_de?: number; limite?: number }, signal?: AbortSignal) =>
    request<ListaNotificacionesAml>(`${AML}/notificaciones`, {
      token,
      signal,
      query: { despues_de: q.despues_de, limite: q.limite },
    }),

  /** Current parameters (thresholds, observation mode), rules and pending proposals. */
  amlRules: (token: string, q: { incluir_inactivas?: boolean }, signal?: AbortSignal) =>
    request<ReglasAml>(`${AML}/reglas`, {
      token,
      signal,
      query: { incluir_inactivas: q.incluir_inactivas === undefined ? undefined : String(q.incluir_inactivas) },
    }),

  /** Rule and parameter change proposals, PENDIENTE by default. */
  amlProposals: (token: string, q: { estado?: string; limite?: number }, signal?: AbortSignal) =>
    request<ListaPropuestasAml>(`${AML}/propuestas`, { token, signal, query: { estado: q.estado, limite: q.limite } }),

  amlPropose: (token: string, body: PropuestaInput) =>
    request<PropuestaCreadaAml>(`${AML}/propuestas`, { method: "POST", token, body }),

  amlDecideProposal: (token: string, id: Uuid, body: DecisionInput) =>
    request<DecisionPropuestaAml>(`${AML}/propuestas/${encodeURIComponent(id)}/decision`, { method: "POST", token, body }),

  /** Quarterly register of relevant operations. */
  amlRelevant: (token: string, q: { trimestre?: string; antes_de?: IsoDateTime; limite?: number }, signal?: AbortSignal) =>
    request<ListaRelevantesAml>(`${AML}/relevantes`, {
      token,
      signal,
      query: { trimestre: q.trimestre, antes_de: q.antes_de, limite: q.limite },
    }),

  /** Recomputes the audit-log hashes and seals. */
  amlVerifyAuditLog: (token: string, signal?: AbortSignal) =>
    request<VerificacionBitacoraAml>(`${AML}/bitacora/verificacion`, { token, signal }),
};

/** A fresh idempotency key: one per logical operation, reused on retries. */
export function newIdempotencyKey(): string {
  if (typeof crypto.randomUUID === "function") return crypto.randomUUID();
  // randomUUID only exists in secure contexts; getRandomValues works everywhere.
  const b = crypto.getRandomValues(new Uint8Array(16));
  b[6] = (b[6]! & 0x0f) | 0x40;
  b[8] = (b[8]! & 0x3f) | 0x80;
  const h = Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}
