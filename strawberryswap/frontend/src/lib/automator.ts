import { BATCH_PUBKEY } from "../zk/field";
import type { IntentOpening } from "../zk/notes";

/**
 * Transport to the Automator's intake API (bots/automator). Rules enforced HERE, in code:
 *  - https, or http ONLY for onion services (Tor already authenticates + encrypts them) and loopback (development);
 *  - no credentials, no referrer, no cache, no redirects, no custom headers: nothing that identifies the user beyond
 *    what the network layer itself reveals (use Tor Browser for .onion relays);
 *  - the body is only ever the intent OPENING — which contains no key — and is never logged;
 *  - responses are size-capped and strictly validated; a relay advertising a different batch key is refused.
 */
export interface AutomatorInfo {
  batchSize: number;
  minSwaps: number;
  epochSeconds: number;
  batchPubKey: string;
}

export type AutomatorIntentState = "queued" | "inflight" | "settled" | "refunded" | "dropped";

export class NoteNotYetKnown extends Error {
  constructor() {
    super("the Automator has not seen this note on-chain yet");
  }
}

const LOOPBACK = new Set(["localhost", "127.0.0.1", "[::1]"]);

export function validateAutomatorUrl(raw: string): string {
  let u: URL;
  try {
    u = new URL(raw);
  } catch {
    throw new Error("invalid Automator URL");
  }
  if (u.username || u.password) throw new Error("Automator URL must not contain credentials");
  const host = u.hostname.toLowerCase();
  const secure = u.protocol === "https:";
  const allowedPlain = u.protocol === "http:" && (host.endsWith(".onion") || LOOPBACK.has(host));
  if (!secure && !allowedPlain) throw new Error("Automator URL must be https, or http for an .onion / loopback address");
  return `${u.protocol}//${u.host}`;
}

const MAX_BODY = 4096;

async function request(base: string, path: string, init: { method: "GET" | "POST"; body?: unknown }, timeoutMs = 30_000): Promise<{ status: number; json: unknown }> {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), timeoutMs);
  try {
    const res = await fetch(`${validateAutomatorUrl(base)}${path}`, {
      method: init.method,
      headers: init.body === undefined ? undefined : { "Content-Type": "application/json" },
      body: init.body === undefined ? undefined : JSON.stringify(init.body),
      credentials: "omit", referrerPolicy: "no-referrer", cache: "no-store", redirect: "error", mode: "cors", signal: ctl.signal,
    });
    const text = await res.text();
    if (text.length > MAX_BODY) throw new Error("Automator response too large");
    let json: unknown = undefined;
    try {
      json = text ? JSON.parse(text) : undefined;
    } catch {
      throw new Error("Automator returned malformed JSON");
    }
    return { status: res.status, json };
  } finally {
    clearTimeout(timer);
  }
}

const isRecord = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null;

export async function fetchInfo(base: string): Promise<AutomatorInfo> {
  const { status, json } = await request(base, "/v1/info", { method: "GET" });
  if (status !== 200 || !isRecord(json)) throw new Error(`Automator info failed (${status})`);
  const { batchSize, minSwaps, epochSeconds, batchPubKey } = json;
  if (typeof batchSize !== "number" || typeof minSwaps !== "number" || typeof epochSeconds !== "number" || typeof batchPubKey !== "string") throw new Error("Automator info malformed");
  // The batch key defines which notes are intent notes. A relay with another key would be asking us to lock funds
  // under rules this app's circuits do not implement — refuse before anything is spent.
  if (batchPubKey !== BATCH_PUBKEY.toString()) throw new Error("this Automator uses a different batch key — refusing");
  return { batchSize, minSwaps, epochSeconds, batchPubKey };
}

/** POST the intent opening. Throws NoteNotYetKnown on 404 so the caller can retry while the relay syncs the chain. */
export async function submitIntent(base: string, opening: IntentOpening): Promise<string> {
  const { status, json } = await request(base, "/v1/intents", { method: "POST", body: opening });
  if (status === 404) throw new NoteNotYetKnown();
  if (status !== 202 || !isRecord(json) || typeof json.receipt !== "string" || !/^[0-9a-f]{32}$/.test(json.receipt)) throw new Error(`Automator rejected the intent (${status})`);
  return json.receipt;
}

export async function getIntentStatus(base: string, receipt: string): Promise<{ state: AutomatorIntentState; txHash?: string }> {
  if (!/^[0-9a-f]{32}$/.test(receipt)) throw new Error("bad receipt");
  const { status, json } = await request(base, `/v1/intents/${receipt}`, { method: "GET" });
  if (status === 404) return { state: "dropped" };
  if (status !== 200 || !isRecord(json) || typeof json.state !== "string") throw new Error(`status failed (${status})`);
  const state = json.state as AutomatorIntentState;
  if (!["queued", "inflight", "settled", "refunded", "dropped"].includes(state)) throw new Error("unknown state");
  return typeof json.txHash === "string" ? { state, txHash: json.txHash } : { state };
}
