/**
 * The Sovereign Vault's real-time channel, `GET /api/v1/vault/ws`, without
 * React: it connects, hands every frame to the store, asks for a resync when
 * the store finds a gap, and reconnects when the connection drops or the
 * backend deploys. `src/hooks/use-vault-socket.ts` runs it in the dashboard.
 *
 * The protocol (backend/src/boveda/socket.rs, README "Sovereign Vault"):
 * - Authentication: a browser cannot set headers on a WebSocket, so the JWT
 *   travels as a subprotocol entry, `["sc.vault.v1", "bearer.<jwt>"]`. The
 *   server confirms only `sc.vault.v1`; the token never comes back.
 * - Server to client: a `vault.snapshot` first, then deltas with a higher
 *   `seq`. The store enforces the order (`receive`).
 * - Client to server: `{"type":"resync"}` asks for a full snapshot. The
 *   server serves one per second at most and merges the rest.
 * - Liveness: the server pings every 25 s and the browser answers on its
 *   own; nothing to do here.
 * - Close codes: 4001, the session expired or was revoked (sign in again; a
 *   reconnection would be refused); 1001, the server is restarting or
 *   heard nothing for 75 s; anything else, the network.
 *
 * Pure TypeScript with injectable timers and socket, so `node --test` drives
 * it with a fake socket and the real store (hence the relative `.ts` import).
 */

import type { ApplyResult, RealtimeStatus } from "../store/vault.ts";

export const VAULT_PROTOCOL = "sc.vault.v1";
/** Close code: the session expired or was revoked. */
export const CLOSE_SESSION_ENDED = 4001;
/** Close code for leaving on purpose (sign-out, leaving the dashboard). */
const CLOSE_NORMAL = 1000;
/** WebSocket.OPEN. */
const OPEN = 1;
const RESYNC = JSON.stringify({ type: "resync" });

/** The channel's URL for an API base URL: http becomes ws, https becomes wss. */
export function vaultSocketUrl(apiUrl: string): string {
  const url = new URL(`${apiUrl.replace(/\/+$/, "")}/api/v1/vault/ws`);
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  return url.toString();
}

/**
 * How long to wait before reconnection attempt `attempt` (0 for the first):
 * doubling from 1 s up to 30 s, each scaled by a random 50 to 100 %, so the
 * sockets a deploy closes all at once do not all come back at once.
 */
export function reconnectDelay(attempt: number, random: () => number = Math.random): number {
  const base = Math.min(30_000, 1_000 * 2 ** Math.max(attempt, 0));
  return Math.round(base * (0.5 + random() / 2));
}

/** What the channel uses of a WebSocket: the browser's, or a test double. */
export interface SocketLike {
  readonly readyState: number;
  onopen: ((event: unknown) => void) | null;
  onmessage: ((event: { data: unknown }) => void) | null;
  onclose: ((event: { code: number; reason: string }) => void) | null;
  onerror: ((event: unknown) => void) | null;
  send(data: string): void;
  close(code?: number, reason?: string): void;
}

export interface VaultSocketOptions {
  /** `vaultSocketUrl(API_URL)`. */
  url: string;
  /** The session's JWT. */
  token: string;
  /** Applies a frame (`useVaultStore.getState().receive`). */
  receive: (data: unknown) => ApplyResult;
  /** The store's current version (`lastSeq`). */
  currentSeq: () => number | null;
  /** `useVaultStore.getState().setRealtimeStatus`. */
  setStatus: (status: RealtimeStatus) => void;
  /** The server ended the session (close 4001), or the token expired before a reconnection. */
  onSessionEnded: () => void;
  /**
   * The vault moved to a newer version than this channel had seen: a
   * deposit, a refund, an investment, or changes made while it was offline.
   * The dashboard reloads what the stream does not carry (ledger, summary).
   */
  onAdvance?: () => void;
  /** Whether the session can no longer be used (checked before every reconnection). */
  isTokenExpired?: () => boolean;
  createSocket?: (url: string, protocols: string[]) => SocketLike;
  /** Runs `callback` after `ms`; returns a cancel function. */
  schedule?: (callback: () => void, ms: number) => () => void;
  random?: () => number;
}

function browserSocket(url: string, protocols: string[]): SocketLike {
  return new WebSocket(url, protocols) as unknown as SocketLike;
}

function browserSchedule(callback: () => void, ms: number): () => void {
  const id = setTimeout(callback, ms);
  return () => clearTimeout(id);
}

export class VaultSocket {
  readonly #options: VaultSocketOptions;
  #socket: SocketLike | null = null;
  #stopped = true;
  /** Failed attempts since the stream last worked: the backoff exponent. */
  #attempt = 0;
  #cancelRetry: (() => void) | null = null;
  /** The newest version this channel has seen, across reconnections. */
  #lastSeen: number | null = null;

  constructor(options: VaultSocketOptions) {
    this.#options = options;
  }

  /** Opens the channel. A second call does nothing. */
  start(): void {
    if (!this.#stopped) return;
    this.#stopped = false;
    this.#connect("connecting");
  }

  /** Closes the channel for good (sign-out, leaving the dashboard): no reconnection. */
  stop(): void {
    this.#stopped = true;
    this.#cancelRetry?.();
    this.#cancelRetry = null;
    const socket = this.#socket;
    this.#socket = null;
    if (socket) {
      socket.onopen = socket.onmessage = socket.onerror = socket.onclose = null;
      socket.close(CLOSE_NORMAL, "client leaving");
    }
    this.#options.setStatus("offline");
  }

  /**
   * Reconnects now if a reconnection is waiting (the network came back, the
   * tab became visible again); otherwise does nothing.
   */
  wake(): void {
    if (this.#stopped || this.#socket || !this.#cancelRetry) return;
    this.#cancelRetry();
    this.#cancelRetry = null;
    this.#attempt = 0;
    this.#connect("reconnecting");
  }

  #connect(status: "connecting" | "reconnecting"): void {
    if (this.#options.isTokenExpired?.()) {
      this.#endSession();
      return;
    }
    this.#options.setStatus(status);
    let socket: SocketLike;
    try {
      socket = (this.#options.createSocket ?? browserSocket)(this.#options.url, [
        VAULT_PROTOCOL,
        `bearer.${this.#options.token}`,
      ]);
    } catch {
      // The constructor refused (a URL or protocol the browser rejects): try again later.
      this.#retry();
      return;
    }
    this.#socket = socket;
    socket.onopen = () => this.#options.setStatus("live");
    socket.onmessage = (event) => this.#onMessage(event.data);
    // An error is always followed by a close, which decides what happens next.
    socket.onerror = () => undefined;
    socket.onclose = (event) => this.#onClose(socket, event.code);
  }

  #onMessage(data: unknown): void {
    const result = this.#options.receive(data);
    if (result === "gap" || result === "no-baseline") this.#send(RESYNC);
    if (result !== "applied" && result !== "gap") return;
    // The stream works: the next drop starts the backoff from the beginning.
    this.#attempt = 0;
    const seq = this.#options.currentSeq();
    if (seq === null) return;
    if (this.#lastSeen !== null && seq > this.#lastSeen) this.#options.onAdvance?.();
    this.#lastSeen = this.#lastSeen === null ? seq : Math.max(this.#lastSeen, seq);
  }

  #onClose(socket: SocketLike, code: number): void {
    if (socket !== this.#socket) return;
    this.#socket = null;
    if (this.#stopped) return;
    if (code === CLOSE_SESSION_ENDED) {
      this.#endSession();
      return;
    }
    // Drops the stream's baseline: REST may fill in until the stream is back.
    this.#options.setStatus("reconnecting");
    this.#retry();
  }

  #retry(): void {
    const delay = reconnectDelay(this.#attempt, this.#options.random);
    this.#attempt += 1;
    this.#cancelRetry = (this.#options.schedule ?? browserSchedule)(() => {
      this.#cancelRetry = null;
      if (!this.#stopped) this.#connect("reconnecting");
    }, delay);
  }

  #endSession(): void {
    this.#stopped = true;
    this.#options.setStatus("offline");
    this.#options.onSessionEnded();
  }

  #send(message: string): void {
    if (this.#socket?.readyState === OPEN) this.#socket.send(message);
  }
}
