import { passwordChangeBody } from "@/lib/security";
import { stepUpBody } from "@/lib/step-up";
import type { OrderRequest, PlanAction, PriceAlertRequest } from "@/types/market";
import type { DesignationRequest } from "@/types/beneficiaries";
import type { InactivityMinutes, PasswordChange } from "@/types/security";
import type { StepUpCredential } from "@/types/step-up";
import type { W8benCertification } from "@/types/taxes";
import type { SpeiClarificationRequest } from "@/types/transfers";
import type { WithdrawalRequest } from "@/types/wealth";

import { request, requestText } from "./client";
import type {
  AmlAlertsQuery,
  Cliente,
  ConfiguracionMfa,
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
  ResultadoLogin,
  ResultadoLiquidacion,
  ResultadoMutuo,
  ResultadoVerificacionPrueba,
  ResumenAml,
  RevisionClienteAml,
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
/** Orders per page of the order history (the backend allows 1 to 100). */
export const ORDER_HISTORY_PAGE = 25;

/** One function per backend route used by the client app (see backend/src/routes). */
export const api = {
  register: (input: RegistroInput) =>
    request<RegistroRespuesta>("/api/v1/auth/registro", { method: "POST", body: input }),

  login: (email: string, password: string) =>
    request<ResultadoLogin>("/api/v1/auth/login", { method: "POST", body: { email, password } }),

  verifyMfa: (tokenMfa: string, codigo: string) =>
    request<ResultadoLogin>("/api/v1/auth/verify-mfa", { method: "POST", body: { token_mfa: tokenMfa, codigo } }),

  /** `token`: the enrollment token from sign-in, or a session's own token. */
  mfaSetup: (token: string) => request<ConfiguracionMfa>("/api/v1/auth/mfa/setup", { method: "POST", token }),

  mfaEnable: (token: string, codigo: string) =>
    request<ResultadoLogin>("/api/v1/auth/mfa/enable", { method: "POST", token, body: { codigo } }),

  /** Always answers the same, whether or not the email has an account. */
  forgotPassword: (email: string) =>
    request<{ estado: string; mensaje: string }>("/api/v1/auth/password/forgot", { method: "POST", body: { email } }),

  resetPassword: (token: string, password: string) =>
    request<{ estado: "OK" }>("/api/v1/auth/password/reset", { method: "POST", body: { token, password } }),

  // Security Center (migration 0019): every session is a row on the server.

  /** Ends this session on the server (204): its token stops working at once. */
  logout: (token: string) => request<null>("/api/v1/auth/logout", { method: "POST", token }),

  /** The signed-in devices; validate with `parseDeviceSessions`. */
  sessions: (token: string, signal?: AbortSignal) => request<unknown>("/api/v1/auth/sessions", { token, signal }),

  /** Signs one device out (204); 404 when it had already ended. */
  revokeSession: (token: string, sessionId: string) =>
    request<null>(`/api/v1/auth/sessions/${encodeURIComponent(sessionId)}/revoke`, { method: "POST", token }),

  /** Signs every other device out; validate with `parseRevokedCount`. */
  revokeOtherSessions: (token: string) =>
    request<unknown>("/api/v1/auth/sessions/revoke-others", { method: "POST", token }),

  /** Current password (and authenticator code, with two-step verification on); signs other devices out. */
  changePassword: (token: string, change: PasswordChange) =>
    request<unknown>("/api/v1/auth/password/change", { method: "POST", token, body: passwordChangeBody(change) }),

  /** How this session confirms a sensitive action, and its open window; validate with `parseStepUpStatus`. */
  stepUpStatus: (token: string, signal?: AbortSignal) => request<unknown>("/api/v1/auth/step-up", { token, signal }),

  /**
   * Confirms the authenticator code (or the password, without two-step
   * verification) and opens a 5-minute window on this session; validate with
   * `parseStepUpWindow`. Counts toward the sign-in lockout.
   */
  stepUp: (token: string, credential: StepUpCredential) =>
    request<unknown>("/api/v1/auth/step-up", { method: "POST", token, body: stepUpBody(credential) }),

  /** The security log, newest first; validate with `parseSecurityEvents`. */
  securityEvents: (token: string, signal?: AbortSignal) =>
    request<unknown>("/api/v1/auth/security/events", { token, signal }),

  /** Inactivity timeout and two-step status; validate with `parseSecurityPreferences`. */
  securityPreferences: (token: string, signal?: AbortSignal) =>
    request<unknown>("/api/v1/auth/security/preferences", { token, signal }),

  saveSecurityPreferences: (token: string, inactivityMinutes: InactivityMinutes) =>
    request<unknown>("/api/v1/auth/security/preferences", {
      method: "POST",
      token,
      body: { inactividad_minutos: inactivityMinutes },
    }),

  dashboard: (token: string, signal?: AbortSignal) => request<Tablero>("/api/v1/tablero", { token, signal }),

  /**
   * The Sovereign Vault snapshot (src/types/vault.ts). Untyped on purpose:
   * `useVaultStore.load` validates the body before anything renders it.
   */
  vault: (token: string, signal?: AbortSignal) => request<unknown>("/api/v1/vault", { token, signal }),

  /**
   * The market catalog: every asset that can be quoted and traded now.
   * Untyped on purpose: `parseMarketCatalog` validates it before it is stored.
   */
  marketAssets: (token: string, signal?: AbortSignal) => request<unknown>("/api/v1/market/assets", { token, signal }),

  /**
   * A market quote: the asset and its price history (src/types/market.ts).
   * Untyped on purpose: `parseMarketQuote` validates the body before it renders.
   */
  marketQuote: (token: string, ticker: string, signal?: AbortSignal) =>
    request<unknown>(`/api/v1/market/quote/${encodeURIComponent(ticker)}`, { token, signal }),

  /**
   * Places an order. A market order is sent without `order_type` (what the
   * endpoint reads as market) and answers an `OrderReceipt`, filled at the
   * current price; a limit or stop order carries its type and price and
   * answers a `PendingOrder`, waiting with its cash or units held (or filled
   * at once, when a limit is already reachable); a savings plan carries its
   * frequency and answers the `SavingsPlan`, its first purchase made (or
   * skipped). 201 for a new order, 200 when the idempotency key repeats the
   * same one. Untyped on purpose: `parseOrderReceipt`, `parsePendingOrder`
   * and `parseSavingsPlan` validate the answer.
   */
  placeOrder: (token: string, order: OrderRequest) =>
    request<unknown>("/api/v1/market/order", { method: "POST", token, body: orderBody(order) }),

  /**
   * The client's limit and stop orders still waiting for their price, newest
   * first. Untyped on purpose: `parsePendingOrders` validates the list.
   */
  pendingOrders: (token: string, signal?: AbortSignal) =>
    request<unknown>("/api/v1/market/orders/pending", { token, signal }),

  /**
   * Cancels a pending limit or stop order: its cash or units are released.
   * Repeating it answers the same cancelled order; one that already filled or
   * was rejected is 409 `ORDEN_NO_PENDIENTE`. Untyped on purpose:
   * `parsePendingOrder` validates the answer.
   */
  cancelOrder: (token: string, orderId: string) =>
    request<unknown>(`/api/v1/market/orders/${encodeURIComponent(orderId)}/cancel`, { method: "POST", token }),

  /**
   * The client's active and paused savings plans, newest first, with their
   * next dates and what they bought. Untyped on purpose: `parseSavingsPlans`
   * validates the list.
   */
  savingsPlans: (token: string, signal?: AbortSignal) =>
    request<unknown>("/api/v1/market/savings-plans", { token, signal }),

  /**
   * Pauses, resumes or cancels a savings plan and answers it as it stands.
   * Repeating an action answers the same plan; a cancelled plan is 409
   * `PLAN_CANCELADO` to pause or resume. Untyped on purpose:
   * `parseSavingsPlan` validates the answer.
   */
  changeSavingsPlan: (token: string, planId: string, action: PlanAction) =>
    request<unknown>(`/api/v1/market/savings-plans/${encodeURIComponent(planId)}/${action}`, {
      method: "POST",
      token,
    }),

  /**
   * One page of the client's order history, newest first: every filled order
   * (market, limit, stop and savings plan purchases) and every limit or stop
   * order cancelled or rejected. `cursor` is the previous page's
   * `next_cursor`. Untyped on purpose: `parseOrderHistory` validates it.
   */
  orderHistory: (token: string, cursor: string | null, signal?: AbortSignal) => {
    const query = new URLSearchParams({ limit: String(ORDER_HISTORY_PAGE) });
    if (cursor !== null) query.set("cursor", cursor);
    return request<unknown>(`/api/v1/market/orders/history?${query}`, { token, signal });
  },

  /**
   * The assets the client follows, newest first, each with its catalog
   * listing. Untyped on purpose: `parseWatchlist` validates the list.
   */
  watchlist: (token: string, signal?: AbortSignal) => request<unknown>("/api/v1/market/watchlist", { token, signal }),

  /**
   * Follows or unfollows an asset. The body asks for the state, so repeating
   * it changes nothing (a retry is safe). Untyped on purpose:
   * `parseWatchState` validates the answer.
   */
  setWatched: (token: string, ticker: string, watched: boolean) =>
    request<unknown>(`/api/v1/market/watchlist/${encodeURIComponent(ticker)}/toggle`, {
      method: "POST",
      token,
      body: { watched },
    }),

  /**
   * The client's price alerts: the active ones and those reached in the last
   * 30 days. Untyped on purpose: `parsePriceAlerts` validates the list.
   */
  priceAlerts: (token: string, signal?: AbortSignal) => request<unknown>("/api/v1/market/alerts", { token, signal }),

  /**
   * Sets a price target. 201 for a new one, 200 when the idempotency key
   * repeats the same one; a target the price is already at is 422
   * `AVISO_YA_ALCANZADO`. Untyped on purpose: `parsePriceAlert` validates it.
   */
  createPriceAlert: (token: string, body: PriceAlertRequest) =>
    request<unknown>("/api/v1/market/alerts", { method: "POST", token, body }),

  /**
   * Cancels an active price alert. Repeating it answers the same one; one
   * already reached is 409 `AVISO_DISPARADO`.
   */
  cancelPriceAlert: (token: string, alertId: string) =>
    request<unknown>(`/api/v1/market/alerts/${encodeURIComponent(alertId)}/cancel`, { method: "POST", token }),

  /**
   * The client's bank accounts linked for withdrawals, all in their own name.
   * In demo mode the first call links one (BBVA México, ending in 1234).
   * Untyped on purpose: `parseLinkedAccounts` validates the answer.
   */
  linkedAccounts: (token: string, signal?: AbortSignal) =>
    request<unknown>("/api/v1/wealth/linked-accounts", { token, signal }),

  /**
   * Withdraws available cash (never cash held by open orders) to a linked
   * account by SPEI: it leaves the available cash at once and stays
   * "processing" until the network settles it. 201 for a new withdrawal, 200
   * when the idempotency key repeats one. Untyped on purpose:
   * `parseWithdrawalReceipt` validates the answer.
   */
  withdraw: (token: string, body: WithdrawalRequest) =>
    request<unknown>("/api/v1/wealth/withdraw", { method: "POST", token, body }),

  /**
   * The client's SPEI transfers, in and out, newest first, a page at a time
   * (`cursor` from the previous page; null for the first). Untyped on
   * purpose: `parseSpeiTransfers` validates each page.
   */
  speiTransfers: (token: string, cursor: string | null, signal?: AbortSignal) =>
    request<unknown>("/api/v1/wealth/spei/transfers", {
      token,
      signal,
      query: { limit: SPEI_TRANSFERS_PAGE, cursor: cursor ?? undefined },
    }),

  /**
   * One SPEI transfer, by its id or by the id of a ledger movement it made
   * (an order's charge or refund, a deposit's credit); 404 when it is not the
   * client's. Validate with `parseTrackedTransfer`.
   */
  speiTransfer: (token: string, id: string, signal?: AbortSignal) =>
    request<unknown>(`/api/v1/wealth/spei/transfers/${encodeURIComponent(id)}`, { token, signal }),

  /** The client's SPEI clarifications, newest first, with their history. Validate with `parseSpeiClarifications`. */
  speiClarifications: (token: string, signal?: AbortSignal) =>
    request<unknown>("/api/v1/wealth/spei/disputes", { token, signal }),

  /**
   * Opens a clarification on one of the client's SPEI transfers: 201 for a
   * new one, 200 when the idempotency key repeats it. 409
   * `ACLARACION_ACTIVA` when the transfer already has one open, 422
   * `ACLARACION_VENCIDA` past 90 days and `LIMITE_ACLARACIONES` with 10 open.
   * Untyped on purpose: `parseClarification` validates the answer.
   */
  openSpeiClarification: (token: string, body: SpeiClarificationRequest) =>
    request<unknown>("/api/v1/wealth/spei/disputes", { method: "POST", token, body }),

  /**
   * The Document Inbox: monthly account statements and a trade confirmation
   * for every executed order, derived from the hash-chained ledgers, newest
   * first. Untyped on purpose: `parseDocumentList` validates the answer.
   */
  documents: (token: string, signal?: AbortSignal) => request<unknown>("/api/v1/wealth/documentos", { token, signal }),

  /** One document ("co_<order id>" or "ec_<YYYY-MM>"); 404 when it is not the client's. Validate with `parseDocument`. */
  document: (token: string, id: string, signal?: AbortSignal) =>
    request<unknown>(`/api/v1/wealth/documentos/${encodeURIComponent(id)}`, { token, signal }),

  /**
   * The Tax Center for a fiscal year (the current one by default): the fiscal
   * profile, the W-8BEN and the year's summary. Validate with `parseTaxCenter`.
   */
  taxCenter: (token: string, year?: number, signal?: AbortSignal) =>
    request<unknown>("/api/v1/wealth/impuestos", { token, signal, query: { year } }),

  /** Certifies or renews the W-8BEN (201); validate the answer with `parseW8benState`. */
  certifyW8ben: (token: string, body: W8benCertification) =>
    request<unknown>("/api/v1/wealth/impuestos/w8ben", { method: "POST", token, body }),

  /** The year's accounting export (RFC 4180 CSV); reconcile it with `verifyExport` before saving. */
  taxExport: (token: string, year: number, signal?: AbortSignal) =>
    requestText("/api/v1/wealth/impuestos/export.csv", { token, signal, query: { year } }),

  movements: (token: string, params: { limit?: number; before?: number } = {}, signal?: AbortSignal) =>
    request<TransaccionIfpe[]>("/api/v1/ifpe/movimientos", {
      token,
      signal,
      query: { limite: params.limit, antes_de: params.before },
    }),

  simulatedDeposit: (token: string, monto: string, clave_idempotencia: string) =>
    request<Movimiento>("/api/v1/ifpe/depositos/simulados", {
      method: "POST",
      token,
      body: { monto, clave_idempotencia },
    }),

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

  /** The latest published cut (public): its roots, totals and anchors. */
  latestCut: (signal?: AbortSignal) => request<CorteReserva>("/api/v1/reserva/cortes/ultimo", { signal }),

  verifyProof: (proof: PruebaReserva) =>
    request<ResultadoVerificacionPrueba>("/api/v1/reserva/verificar", {
      method: "POST",
      body: {
        corte_id: proof.corte.id,
        hoja: proof.hoja,
        ruta: proof.ruta,
        // Only with positions: a backend from before migration 0018 refuses unknown fields.
        ...(proof.posiciones?.length
          ? { posiciones: proof.posiciones.map((p) => ({ hoja: p.hoja, ruta: p.ruta })) }
          : {}),
      },
    }),

  /** The signed-in client (its `estatus_kyc` tells a SUSPENDIDO client from a rejected one). */
  me: (token: string, signal?: AbortSignal) => request<Cliente>("/api/v1/yo", { token, signal }),

  /** The customer's KYC file: state, attempts left and the last biometric result. */
  kycFile: (token: string, signal?: AbortSignal) =>
    request<VistaExpediente>("/api/v1/kyc/expediente", { token, signal }),

  /** Step 1: personal data and address (only while the file is PENDIENTE). */
  kycSaveProfile: (token: string, body: DatosPersonalesInput) =>
    request<DatosGuardados>("/api/v1/kyc/perfil", { method: "POST", token, body }),

  /**
   * Steps 2-3: credential + selfie frames. Resolves on 200 (`resultado` set)
   * and on 202 (`resultado: null`: the provider answers later, poll kycFile).
   */
  kycVerify: (token: string, body: SolicitudVerificacion, signal?: AbortSignal) =>
    request<EnvioVerificacion>("/api/v1/kyc/verificar", { method: "POST", token, body, signal }),

  /** Legal beneficiaries: the current designation and its history. Validate with `parseBeneficiaries`. */
  beneficiaries: (token: string, signal?: AbortSignal) =>
    request<unknown>("/api/v1/kyc/beneficiarios", { token, signal }),

  /**
   * Saves the whole designation as a new version on top of `version_base`
   * (201); an empty list withdraws it. 409 DESIGNACION_DESACTUALIZADA when
   * another session saved first. Validate the answer with `parseBeneficiaries`.
   */
  saveBeneficiaries: (token: string, body: DesignationRequest) =>
    request<unknown>("/api/v1/kyc/beneficiarios", { method: "POST", token, body }),

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
    request<ListaSolicitudesAml>(`${AML}/solicitudes`, {
      token,
      signal,
      query: { estado: q.estado, limite: q.limite },
    }),

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
    request<DecisionPropuestaAml>(`${AML}/propuestas/${encodeURIComponent(id)}/decision`, {
      method: "POST",
      token,
      body,
    }),

  /** Quarterly register of relevant operations. */
  amlRelevant: (
    token: string,
    q: { trimestre?: string; antes_de?: IsoDateTime; limite?: number },
    signal?: AbortSignal,
  ) =>
    request<ListaRelevantesAml>(`${AML}/relevantes`, {
      token,
      signal,
      query: { trimestre: q.trimestre, antes_de: q.antes_de, limite: q.limite },
    }),

  /** Recomputes the audit-log hashes and seals. */
  amlVerifyAuditLog: (token: string, signal?: AbortSignal) =>
    request<VerificacionBitacoraAml>(`${AML}/bitacora/verificacion`, { token, signal }),
};

/** SPEI transfers per page on the Transfers page (the backend allows up to 100). */
export const SPEI_TRANSFERS_PAGE = 25;

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

/** The body of `POST /api/v1/market/order`: exactly the fields the backend reads for each type. */
export function orderBody(order: OrderRequest): Record<string, string> {
  const base = { ticker: order.ticker, side: order.side, amount: order.amount, idempotency_key: order.idempotency_key };
  switch (order.order_type) {
    case "market":
      return base;
    case "limit":
      return { ...base, order_type: "limit", limit_price: order.limit_price };
    case "stop":
      return { ...base, order_type: "stop", stop_price: order.stop_price };
    case "savings_plan":
      return { ...base, order_type: "savings_plan", frequency: order.frequency };
  }
}
