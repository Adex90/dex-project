import { GENERIC_UNAVAILABLE_MESSAGE } from "../aml";
import { API_URL } from "../config";
import { kycMessageInEnglish } from "../kyc";
import type { ApiErrorBody } from "./types";

/** Error returned by the backend ({ error: { codigo, mensaje } }) or by the network. */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
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
    if (isErrorBody(data)) throw new ApiError(response.status, data.error.codigo, data.error.mensaje);
    throw new ApiError(response.status, `HTTP_${response.status}`, response.statusText || "Unexpected error");
  }
  return data as T;
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
  PROHIBIDO: "You are not allowed to perform this operation.",
  EMAIL_REGISTRADO: "That email is already registered.",
  IDENTIDAD_REGISTRADA: "That CURP/RFC is already registered.",
  SALDO_INSUFICIENTE: "Insufficient available balance in your vault.",
  KYC_NO_VERIFICADO: "Your identity (KYC) has not been verified yet, so money movements are disabled.",
  DESTINO_NO_ACREDITABLE: "The destination account cannot receive this transfer.",
  KYC_PROVEEDOR_NO_DISPONIBLE: "Identity verification is not available right now. Your file was not changed; please try again later.",
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
  METODO_NO_PERMITIDO: "That operation is not supported here.",
  PRUEBA_PENDIENTE: "Your account will be included in the next reserve snapshot. Please try again in a minute.",
  CORTE_EN_CURSO: "A reserve snapshot is being generated right now. Please try again in a moment.",
  SPEI_DUPLICADO: "That SPEI transfer was already credited.",
  LIQUIDEZ_MUTUOS: "The credit division cannot complete this right now without additional capital.",
  FECHA_VALOR: "This operation must be dated today.",
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
export function describeError(error: unknown): string {
  if (error instanceof ApiError) {
    return MESSAGES[error.code] ?? kycMessageInEnglish(error.message) ?? error.message;
  }
  if (error instanceof Error) return error.message;
  return "Unexpected error.";
}
