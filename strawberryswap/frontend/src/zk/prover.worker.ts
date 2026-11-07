/// <reference lib="webworker" />
// The prover worker. Heavy work happens here — downloading ~11 MB of artifacts, WASM witness generation, Groth16
// proving — so the trading UI never freezes. It also owns the shielded wallet (ZkEngine): keys and note openings
// never exist on the UI thread.
import * as snarkjs from "snarkjs";
import type { Hex } from "viem";
import { ZkEngine, type Groth16Prover } from "./engine";
import { proofToHex } from "./format";
import manifestJson from "./manifest.generated.json";
import type { Api, ApiKey, ArtifactEntry, ProgressMessage, WorkerRequest, WorkerResponse, ZkManifest } from "./protocol";

const manifest = manifestJson as ZkManifest;
const CACHE_NAME = "strawberryswap-zk-v1";
const ctx = self as unknown as DedicatedWorkerGlobalScope;

const post = (m: WorkerResponse): void => ctx.postMessage(m);
const progress = (phase: ProgressMessage["phase"], loaded: number, total: number, label: string): void => post({ type: "progress", phase, loaded, total, label });

async function sha256Hex(b: Uint8Array): Promise<string> {
  const d = new Uint8Array(await crypto.subtle.digest("SHA-256", new Uint8Array(b)));
  return [...d].map((x) => x.toString(16).padStart(2, "0")).join("");
}

/**
 * Fetches one artifact, with a streaming progress report, and VERIFIES its SHA-256 against the hash compiled into
 * this bundle (so the site's content hash commits to the exact proving key and witness generator). Verified bytes
 * are kept in the Cache API: later sessions skip the download but are re-verified every time.
 */
async function loadArtifact(e: ArtifactEntry, label: string, base: { done: number; total: number }): Promise<Uint8Array> {
  const url = `${ctx.location.origin}/${e.path}`;
  const key = `${url}?sha256=${e.sha256}`;
  const cache = typeof caches !== "undefined" ? await caches.open(CACHE_NAME) : undefined;

  const hit = await cache?.match(key);
  if (hit) {
    const b = new Uint8Array(await hit.arrayBuffer());
    if ((await sha256Hex(b)) === e.sha256) {
      base.done += b.length;
      progress("download", base.done, base.total, `${label} (cached)`);
      return b;
    }
    await cache?.delete(key); // corrupted or stale entry
  }

  const res = await fetch(url, { cache: "no-store", credentials: "omit", referrerPolicy: "no-referrer" });
  if (!res.ok || !res.body) throw new Error(`could not download ${label} (${res.status})`);
  const chunks: Uint8Array[] = [];
  let got = 0;
  const reader = res.body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    got += value.length;
    progress("download", base.done + got, base.total, label);
  }
  const out = new Uint8Array(got);
  let off = 0;
  for (const c of chunks) {
    out.set(c, off);
    off += c.length;
  }
  progress("verify", base.done + got, base.total, `verifying ${label}`);
  if ((await sha256Hex(out)) !== e.sha256) throw new Error(`${label} does not match the hash pinned in this build — refusing to use it`);
  base.done += got;
  await cache?.put(key, new Response(out.slice()));
  return out;
}

class BrowserProver implements Groth16Prover {
  constructor(private readonly wasm: Uint8Array, private readonly zkey: Uint8Array, private readonly vkey: unknown) {}

  async prove(input: Record<string, unknown>, expected: bigint[]): Promise<Hex> {
    progress("prove", 0, 1, "generating zero-knowledge proof");
    const { proof, publicSignals } = await snarkjs.groth16.fullProve(input, { type: "mem", data: this.wasm }, { type: "mem", data: this.zkey });
    // Refuse to hand back anything the chain would reject: the public signals must be exactly what the engine
    // computed, and the proof must verify against the pinned verification key.
    if (publicSignals.length !== expected.length || publicSignals.some((s, i) => s !== String(expected[i]))) throw new Error("prover returned unexpected public signals");
    if (!(await snarkjs.groth16.verify(this.vkey, publicSignals, proof))) throw new Error("generated proof failed local verification");
    progress("prove", 1, 1, "proof ready");
    return proofToHex(proof);
  }
}

let engine: ZkEngine | undefined;
let initPromise: Promise<Api["init"]["res"]> | undefined;

function init(): Promise<Api["init"]["res"]> {
  initPromise ??= (async () => {
    const a = manifest.artifacts;
    if (!a) throw new Error("proving artifacts are not part of this build (run `npm run prepare:zk` before `npm run build`)");
    const base = { done: 0, total: a.wasm.bytes + a.zkey.bytes + a.vkey.bytes };
    const wasm = await loadArtifact(a.wasm, "witness generator", base);
    const zkey = await loadArtifact(a.zkey, "proving key", base);
    const vkey: unknown = JSON.parse(new TextDecoder().decode(await loadArtifact(a.vkey, "verification key", base)));
    progress("prepare", 1, 1, "ready");
    engine = new ZkEngine(new BrowserProver(wasm, zkey, vkey));
    return { devKeys: manifest.devKeys, zkeyBytes: a.zkey.bytes };
  })().catch((e: unknown) => {
    initPromise = undefined; // allow a retry after a transient network error
    throw e;
  });
  return initPromise;
}

const eng = (): ZkEngine => {
  if (!engine) throw new Error("prover is not initialised");
  return engine;
};

type Handlers = { [K in ApiKey]: (p: Api[K]["req"]) => Promise<Api[K]["res"]> | Api[K]["res"] };
const handlers: Handlers = {
  init: () => init(),
  unlock: (p) => ({ pubKey: eng().unlock(p.signature) }),
  lock: () => (eng().lock(), eng().state()),
  addLeaves: (p) => eng().addLeaves(p.leaves),
  markSpent: (p) => eng().markSpent(p.nullifiers),
  release: (p) => (eng().release(p.commitment), eng().state()),
  state: () => eng().state(),
  pendingOpenings: () => eng().pendingOpenings(),
  proveDeposit: (p) => eng().proveDeposit(p),
  proveIntent: (p) => eng().proveIntent(p),
  proveWithdraw: (p) => eng().proveWithdraw(p),
};

/** Error text sent to the UI: short, and with long digit runs removed so witness values can never ride along. */
const safeMessage = (e: unknown): string => (e instanceof Error ? e.message : String(e)).replace(/\d{12,}/g, "<n>").slice(0, 240);

// Requests are processed strictly one at a time: the engine is stateful and proving is CPU-bound anyway.
let queue: Promise<void> = Promise.resolve();
ctx.onmessage = (ev: MessageEvent<WorkerRequest>): void => {
  const req = ev.data;
  queue = queue.then(async () => {
    try {
      const handler = handlers[req.type] as (p: unknown) => Promise<unknown> | unknown;
      post({ type: "result", id: req.id, ok: true, result: await handler(req.payload) });
    } catch (e) {
      post({ type: "result", id: req.id, ok: false, error: safeMessage(e) });
    }
  });
};
