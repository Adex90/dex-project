import { request } from "./client";
import type {
  EstadoReservaVista,
  Movimiento,
  PlazoTasa,
  PruebaReserva,
  RegistroInput,
  RegistroRespuesta,
  ResultadoLiquidacion,
  ResultadoMutuo,
  ResultadoVerificacionPrueba,
  Sesion,
  Tablero,
  TransaccionIfpe,
} from "./types";

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

  verifyProof: (proof: PruebaReserva) =>
    request<ResultadoVerificacionPrueba>("/api/v1/reserva/verificar", {
      method: "POST",
      body: { corte_id: proof.corte.id, hoja: proof.hoja, ruta: proof.ruta },
    }),
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
