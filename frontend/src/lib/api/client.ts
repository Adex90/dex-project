import { GENERIC_UNAVAILABLE_MESSAGE } from "../aml";
import { API_URL } from "../config";
import { kycMessageInEnglish } from "../kyc";
import type { ApiErrorBody } from "./types";

/** Error returned by the backend ({ error: { codigo, mensaje } }) or by the network. */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  /** A refused credential (step-up): the tries left before the account locks. Null otherwise. */
  readonly remainingAttempts: number | null;

  constructor(status: number, code: string, message: string, remainingAttempts: number | null = null) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.remainingAttempts = remainingAttempts;
  }

  /**
   * The session can no longer be used: expired or revoked (401), or a staff
   * session without verified two-step verification (MFA_REQUERIDO). Either way
   * the person signs in again, and signing in asks for the authenticator code.
   * A wrong password or authenticator code is also a 401, but the person just
   * tries again: those are not a lost session.
   */
  get isUnauthorized(): boolean {
    if (this.code === "MFA_REQUERIDO") return true;
    return this.status === 401 && !RETRYABLE_401.has(this.code);
  }
}

/** 401 answers to a wrong secret (password, authenticator code): retry, keep the sign-in. */
const RETRYABLE_401 = new Set(["CREDENCIALES_INVALIDAS", "CODIGO_MFA_INVALIDO"]);

export interface RequestOptions {
  method?: "GET" | "POST";
  body?: unknown;
  token?: string | null;
  signal?: AbortSignal;
  query?: Record<string, string | number | undefined>;
}

function isErrorBody(value: unknown): value is ApiErrorBody {
  return (
    typeof value === "object" &&
    value !== null &&
    "error" in value &&
    typeof (value as ApiErrorBody).error?.codigo === "string"
  );
}

function apiError(status: number, body: ApiErrorBody): ApiError {
  const left = body.error.intentos_restantes;
  const remaining = typeof left === "number" && Number.isSafeInteger(left) && left >= 0 ? left : null;
  return new ApiError(status, body.error.codigo, body.error.mensaje, remaining);
}

/** Typed fetch wrapper for the Rust API. Never caches: balances must be live. */
export async function request<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const { method = "GET", body, token, signal, query } = options;
  const url = new URL(`${API_URL}${path}`);
  for (const [key, value] of Object.entries(query ?? {})) {
    if (value !== undefined) url.searchParams.set(key, String(value));
  }
  const headers: Record<string, string> = { Accept: "application/json" };
  if (body !== undefined) headers["Content-Type"] = "application/json";
  if (token) headers.Authorization = `Bearer ${token}`;

  let response: Response;
  try {
    response = await fetch(url, {
      method,
      headers,
      body: body === undefined ? undefined : JSON.stringify(body),
      signal,
      cache: "no-store",
    });
  } catch (cause) {
    if (cause instanceof DOMException && cause.name === "AbortError") throw cause;
    throw new ApiError(0, "RED", `Cannot reach the Synthetic Capital API at ${API_URL}.`);
  }

  const text = await response.text();
  let data: unknown = null;
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = null;
    }
  }
  if (!response.ok) {
    if (isErrorBody(data)) throw apiError(response.status, data);
    throw new ApiError(response.status, `HTTP_${response.status}`, response.statusText || "Unexpected error");
  }
  return data as T;
}

/**
 * A text download (the accounting CSV): the body as text once the API answers
 * 2xx, otherwise the API's error, like `request`. Never cached.
 */
export async function requestText(
  path: string,
  options: Pick<RequestOptions, "token" | "signal" | "query"> = {},
): Promise<{ text: string; contentType: string }> {
  const { token, signal, query } = options;
  const url = new URL(`${API_URL}${path}`);
  for (const [key, value] of Object.entries(query ?? {})) {
    if (value !== undefined) url.searchParams.set(key, String(value));
  }
  const headers: Record<string, string> = { Accept: "text/csv, application/json" };
  if (token) headers.Authorization = `Bearer ${token}`;
  let response: Response;
  try {
    response = await fetch(url, { headers, signal, cache: "no-store" });
  } catch (cause) {
    if (cause instanceof DOMException && cause.name === "AbortError") throw cause;
    throw new ApiError(0, "RED", `Cannot reach the Synthetic Capital API at ${API_URL}.`);
  }
  const text = await response.text();
  if (!response.ok) {
    let data: unknown = null;
    try {
      data = JSON.parse(text);
    } catch {
      data = null;
    }
    if (isErrorBody(data)) throw apiError(response.status, data);
    throw new ApiError(response.status, `HTTP_${response.status}`, response.statusText || "Unexpected error");
  }
  return { text, contentType: response.headers.get("content-type") ?? "" };
}

/**
 * English copy for the backend's stable error codes; falls back to the server
 * message. KYC_PROVEEDOR_ERROR and KYC_IDENTIDAD_BLOQUEADA are deliberately
 * absent: their wording depends on the case (e.g. whether the attempt was
 * refunded), so they are translated from the server message.
 */
const MESSAGES: Record<string, string> = {
  RED: "Cannot reach the bank's servers. Check your connection and try again.",
  NO_AUTENTICADO: "Your session has expired. Please sign in again.",
  CREDENCIALES_INVALIDAS: "Incorrect email or password.",
  BLOQUEADO: "Too many failed attempts. Your access is temporarily locked; try again in 15 minutes.",
  MFA_REQUERIDO: "Your role requires two-step verification. Please sign in again.",
  CODIGO_MFA_INVALIDO: "That code is not valid. Enter the current 6-digit code from your authenticator app.",
  DESAFIO_MFA_VENCIDO: "Your sign-in attempt expired. Please sign in again.",
  MFA_YA_ACTIVO: "Two-step verification is already turned on for this account.",
  MFA_SIN_CONFIGURAR: "Set up your authenticator app first.",
  TOKEN_RESTABLECIMIENTO_INVALIDO: "This reset link is invalid or has expired. Please request a new one.",
  // Security Center: changing the password from a signed-in session.
  CONTRASENA_ACTUAL_INCORRECTA: "Your current password is not correct.",
  CONTRASENA_REPETIDA: "Choose a new password that is different from your current one.",
  CODIGO_MFA_REQUERIDO: "Enter the 6-digit code from your authenticator app.",
  CONTRASENA_CAMBIADA_EN_PARALELO: "Your password or code was used somewhere else at the same time. Please try again.",
  // Step-up (migration 0022): withdrawals, transfers and beneficiaries ask to confirm it's you.
  STEP_UP_REQUERIDO: "For your security, confirm it's you before this action. Nothing was done.",
  CONTRASENA_REQUERIDA: "Enter your account password.",
  CODIGO_MFA_USADO: "That code was already used. Wait for the next one in your authenticator app.",
  SESION_BLOQUEADA:
    "Too many failed attempts. For your security, your account is locked for 15 minutes and this session has ended.",
  PROHIBIDO: "You are not allowed to perform this operation.",
  EMAIL_REGISTRADO: "That email is already registered.",
  IDENTIDAD_REGISTRADA: "That CURP/RFC is already registered.",
  SALDO_INSUFICIENTE: "Insufficient available balance in your vault.",
  KYC_NO_VERIFICADO: "Your identity (KYC) has not been verified yet, so money movements are disabled.",
  DESTINO_NO_ACREDITABLE: "The destination account cannot receive this transfer.",
  KYC_PROVEEDOR_NO_DISPONIBLE:
    "Identity verification is not available right now. Your file was not changed; please try again later.",
  CLIENTE_EN_LISTA_BLOQUEADAS: "Operations are blocked on this account for compliance reasons.",
  CUENTA_CONGELADA: "Your vault account is frozen.",
  CUENTA_CERRADA: "Your vault account is closed.",
  MUTUO_NO_VENCIDO: "This contract has not matured yet. Mutuum contracts cannot be withdrawn early.",
  MUTUO_CON_CARTERA_VIGENTE: "This contract still funds loans that have not been repaid; it will settle once they are.",
  LIQUIDEZ_SOFOM_INSUFICIENTE: "The credit division cannot settle this contract right now. Please try again later.",
  OPERACION_DUPLICADA: "This operation was already submitted.",
  CLAVE_IDEMPOTENCIA_REUTILIZADA: "This request conflicts with a previous operation. Please start again.",
  REINTENTAR: "The system is busy. Please try again.",
  INTEGRIDAD: "An integrity check failed and the operation was reverted. Please contact support.",
  TIEMPO_AGOTADO: "The request took too long. Please try again.",
  TIPO_NO_SOPORTADO: "The request was not sent as JSON.",
  CUERPO_DEMASIADO_GRANDE: "The request is too large.",
  RUTA_INEXISTENTE: "That page of the API does not exist.",
  NO_ENCONTRADO: "That was not found: it may no longer exist.",
  METODO_NO_PERMITIDO: "That operation is not supported here.",
  PRUEBA_PENDIENTE: "Your account will be included in the next reserve snapshot. Please try again in a minute.",
  CORTE_EN_CURSO: "A reserve snapshot is being generated right now. Please try again in a moment.",
  SPEI_DUPLICADO: "That SPEI transfer was already credited.",
  LIQUIDEZ_MUTUOS: "The credit division cannot complete this right now without additional capital.",
  FECHA_VALOR: "This operation must be dated today.",
  // Markets (POST /api/v1/market/order, GET /api/v1/market/quote).
  MERCADO_NO_DISPONIBLE: "The market opens once live prices are connected. No order was placed.",
  MERCADO_CERRADO: "This asset is not trading right now. No order was placed.",
  MONTO_INSUFICIENTE: "That amount is too small to buy the smallest unit of this asset.",
  POSICION_INSUFICIENTE:
    "That is more than the units you can sell are worth. Units held by open orders cannot be sold.",
  // Limit and stop orders (migration 0015).
  PRECIO_STOP_INVALIDO:
    "The price has already reached that stop. A buy stop goes above the current price, a sell stop below it.",
  LIMITE_ORDENES_PENDIENTES: "You have the maximum of 50 open orders. Cancel one to place another.",
  ORDEN_NO_PENDIENTE: "This order is no longer open: it has already filled or was rejected.",
  TIPO_ORDEN_NO_DISPONIBLE: "This order type is not available yet. Nothing was placed.",
  // Savings plans (migration 0023).
  LIMITE_PLANES: "You have the maximum of 20 savings plans. Cancel one to create another.",
  PLAN_CANCELADO: "This savings plan was already cancelled: it cannot be paused or resumed.",
  // Watchlist and price targets (migration 0024). Never the singular "alert": a client screen must not show it.
  LIMITE_SEGUIMIENTO: "You already watch 50 assets. Remove one from your watchlist to add another.",
  AVISO_YA_ALCANZADO: "The price is already there. Choose a target above or below the current price.",
  LIMITE_AVISOS_PRECIO: "You have the maximum of 50 active price targets. Cancel one to set another.",
  AVISO_DISPARADO: "That price target was already reached: there is nothing to cancel.",
  // Withdrawals (migration 0016): only to a bank account linked in the client's own name.
  CUENTA_NO_VINCULADA: "Withdrawals go only to a bank account linked in your name. Nothing was sent.",
  TRANSFERENCIAS_TERCEROS_NO_DISPONIBLES:
    "Payments to other people's accounts are not available. Withdrawals go to a bank account in your name.",
  // SPEI clarifications (migration 0025): one open per transfer, within 90 days, 10 open at most.
  ACLARACION_ACTIVA: "This transfer already has an open clarification. Follow it in Clarifications.",
  ACLARACION_VENCIDA: "Clarifications can be opened within 90 days of the transfer.",
  LIMITE_ACLARACIONES: "You have 10 open clarifications. Once one is closed you can open another.",
  // Legal beneficiaries (migration 0020): a save is all or nothing.
  BENEFICIARIOS_PORCENTAJE: "The percentages must add up to exactly 100%. Nothing was saved.",
  BENEFICIARIOS_MAXIMO: "Name at most 5 beneficiaries. Nothing was saved.",
  BENEFICIARIO_TITULAR: "You cannot name yourself as a beneficiary. Nothing was saved.",
  BENEFICIARIO_DUPLICADO: "The same person appears twice. Nothing was saved.",
  BENEFICIARIO_INVALIDO: "Check each beneficiary's details. Nothing was saved.",
  DESIGNACION_DESACTUALIZADA:
    "Your beneficiaries were changed from another session. The latest version is shown; review it and save again.",
  // The W-8BEN (migration 0021): nothing is saved unless every check passes.
  W8BEN_FIRMA: "Sign with your full legal name, exactly as it appears in your file.",
  W8BEN_CERTIFICACIONES: "Confirm all three statements to certify.",
  W8BEN_PERSONA_EUA: "U.S. citizens and residents certify their status on Form W-9 instead.",
  W8BEN_PERSONA_MORAL: "Business accounts certify on Form W-8BEN-E, which is not available here yet.",
  W8BEN_RESIDENCIA: "Here you can certify as a resident of Mexico under the U.S.–Mexico tax treaty.",
  W8BEN_DATOS_INCOMPLETOS: "Your file needs your RFC, date of birth and address before you can certify.",
  W8BEN_CONCURRENTE: "Your W-8BEN was certified from another session at the same time. Reload and check it.",
  // A client whose account is restricted gets this one neutral message whatever
  // the cause (no tipping off): it must not mention compliance, alerts or freezes.
  OPERACION_NO_DISPONIBLE: GENERIC_UNAVAILABLE_MESSAGE,
  // Staff only (operators and the compliance console).
  RETENCION_PROTEGIDA: "Part of this balance can only be released by the compliance team.",
  AML_ALERTA_CERRADA: "This alert is already closed.",
  AML_ASIGNADA_A_OTRO: "Another compliance officer is reviewing this alert.",
  AML_TRANSICION_INVALIDA: "This action is not available in the alert's current state. Refresh and try again.",
  AML_AUTORESOLUCION: "You cannot act on alerts or restrictions that concern your own client record.",
  AML_CUATRO_OJOS: "A second compliance officer must make this decision.",
  AML_ALERTAS_ABIERTAS:
    "Resolve or propose a resolution for this client's other high-risk alerts before lifting the restriction.",
  AML_SOLICITUD_PENDIENTE: "There is already a pending request for this restriction.",
  AML_SOLICITUD_NO_PENDIENTE: "This request has already been decided.",
  AML_CONGELAMIENTO_NO_ACTIVO: "This restriction is no longer active.",
  AML_SIN_RETENCIONES: "There are no held funds to release.",
  AML_PROPUESTA_NO_PENDIENTE: "This proposal has already been decided.",
  AML_CAMBIO_INVALIDO: "The proposed change is not valid for this rule or parameter.",
  AML_DICTAMEN_INVALIDO: "Check the length and content of the text you entered.",
  AML_FOLIO_REGISTRADO: "A UIF folio is already recorded for this alert.",
};

/**
 * English message for an error. Codes without a fixed message (VALIDACION and
 * most KYC_* codes, whose `mensaje` depends on the case) are translated from
 * the backend's Spanish when recognized (see `kycMessageInEnglish`).
 */
/** The English message for an API error code, if there is one ("MONTO_INSUFICIENTE"). */
export function messageForCode(code: string): string | undefined {
  return MESSAGES[code];
}

export function describeError(error: unknown): string {
  if (error instanceof ApiError) {
    return MESSAGES[error.code] ?? kycMessageInEnglish(error.message) ?? error.message;
  }
  if (error instanceof Error) return error.message;
  return "Unexpected error.";
}
