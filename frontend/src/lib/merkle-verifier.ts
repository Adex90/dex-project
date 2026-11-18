/**
 * Client-side verification of Proof-of-Reserve inclusion proofs.
 *
 * Re-implements, independently of the bank, the Merkle sum trees defined in
 * backend/src/crypto/merkle.rs ("SHA-256 merkle-sum v1"):
 *
 *   cash leaf     = SHA256("SC-POR-v1:hoja" 0x00 || account_id[16] || balance_u128_be[16] || salt[32])
 *   position leaf = SHA256("SC-POR-v1:posicion" 0x00 || ticker || 0x00 || account_id[16]
 *                          || quantity_u128_be[16] || salt[32])
 *   node          = SHA256("SC-POR-v1:nodo" 0x01 || L.hash[32] || L.sum_be[16] || R.hash[32] || R.sum_be[16])
 *
 * Amounts are integers in units of 10^-8 (pesos for cash, the asset's unit for
 * a position). Each node commits to the sums of BOTH children, so the bank
 * cannot hide liabilities by under-reporting a sibling's sum: any change breaks
 * the hash chain up to the published root. Cash and each asset have their own
 * tree (migration 0018); a position leaf carries its ticker, so it cannot be
 * moved to another asset's tree or pass for a cash leaf.
 *
 * Deterministic and dependency-free: hashing uses the browser's WebCrypto when
 * available and falls back to a pure-JS SHA-256 (WebCrypto is only exposed in
 * secure contexts: HTTPS or localhost). Both give the same bytes, and the
 * tests check them against vectors computed separately from the spec.
 */

import { fromUnits, toUnits } from "./decimal.ts";

export type Side = "IZQUIERDA" | "DERECHA";
export type Engine = "webcrypto" | "js";

export interface ProofLeaf {
  cuenta_id: string;
  /** Balance at the cut, 8-decimal string. */
  saldo: string;
  /** 32-byte salt, hex. */
  sal: string;
  hash?: string | null;
}

export interface ProofPositionLeaf {
  ticker: string;
  cuenta_id: string;
  /** Quantity of the asset at the cut, 8-decimal string. */
  cantidad: string;
  /** 32-byte salt, hex. */
  sal: string;
  hash?: string | null;
}

export interface ProofStep {
  /** Side on which the SIBLING sits relative to the node being climbed. */
  lado: Side;
  hash: string;
  suma: string;
}

export interface VerifiedStep {
  level: number;
  side: Side;
  siblingHash: string;
  siblingSum: string;
  /** The two children, in hashing order. */
  leftHash: string;
  leftSum: string;
  rightHash: string;
  rightSum: string;
  /** Their parent: SHA-256 of the node preimage, and the sum of both. */
  nodeHash: string;
  nodeSum: string;
}

/** Where the leaf sits: its index among `leafCount` leaves. */
export interface PathShape {
  index: number;
  leafCount: number;
}

export interface VerificationResult {
  leafHash: string;
  leafSum: string;
  /** The leaf hash we computed matches the one the bank sent (null if it sent none). */
  leafHashMatches: boolean | null;
  /** The path turns where the leaf's position says it must (null without a shape). */
  shapeMatches: boolean | null;
  steps: VerifiedStep[];
  rootHash: string;
  rootSum: string;
  rootMatches: boolean;
  totalMatches: boolean;
  valid: boolean;
  engine: Engine;
}

export interface VerifyOptions {
  engine?: Engine;
  shape?: PathShape;
}

const MAX_PATH = 64;
const U128_LIMIT = 1n << 128n;
const encoder = new TextEncoder();
const LEAF_DOMAIN = concat(encoder.encode("SC-POR-v1:hoja"), new Uint8Array([0x00]));
const POSITION_DOMAIN = concat(encoder.encode("SC-POR-v1:posicion"), new Uint8Array([0x00]));
const NODE_DOMAIN = concat(encoder.encode("SC-POR-v1:nodo"), new Uint8Array([0x01]));
const TICKER_RE = /^[A-Z][A-Z0-9]{0,11}$/;

export function concat(...parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let offset = 0;
  for (const p of parts) {
    out.set(p, offset);
    offset += p.length;
  }
  return out;
}

export function hexToBytes(hex: string, expectedLength?: number): Uint8Array {
  const clean = hex.trim().toLowerCase();
  if (!/^(?:[0-9a-f]{2})*$/.test(clean)) throw new Error("invalid hex string");
  const bytes = new Uint8Array(clean.length / 2);
  for (let i = 0; i < bytes.length; i++) bytes[i] = parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  if (expectedLength !== undefined && bytes.length !== expectedLength) {
    throw new Error(`expected ${expectedLength} bytes, got ${bytes.length}`);
  }
  return bytes;
}

export function bytesToHex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

export function uuidToBytes(uuid: string): Uint8Array {
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(uuid.trim())) {
    throw new Error("invalid account id");
  }
  return hexToBytes(uuid.replace(/-/g, ""), 16);
}

export function u128ToBytes(value: bigint): Uint8Array {
  if (value < 0n || value >= U128_LIMIT) throw new Error("value out of u128 range");
  const out = new Uint8Array(16);
  let v = value;
  for (let i = 15; i >= 0; i--) {
    out[i] = Number(v & 0xffn);
    v >>= 8n;
  }
  return out;
}

/** A catalog ticker: the position-leaf encoding relies on it having no NUL byte. */
export function isTicker(ticker: string): boolean {
  return TICKER_RE.test(ticker);
}

/** An amount of the proof as 10^-8 units: at most 8 decimals, never negative. */
function proofUnits(value: string, what: string): bigint {
  const units = toUnits(value);
  if (units < 0n) throw new Error(`negative ${what}`);
  if (units >= U128_LIMIT) throw new Error(`${what} out of range`);
  return units;
}

// ---------------------------------------------------------------------------
// SHA-256
// ---------------------------------------------------------------------------

function subtle(): SubtleCrypto | null {
  const c = (globalThis as { crypto?: Crypto }).crypto;
  return c?.subtle ?? null;
}

/** The engine this browser offers: WebCrypto in a secure context, the pure-JS fallback otherwise. */
export function defaultEngine(): Engine {
  return subtle() ? "webcrypto" : "js";
}

export async function sha256(data: Uint8Array, engine: Engine = defaultEngine()): Promise<Uint8Array> {
  const s = subtle();
  if (engine === "webcrypto" && s) {
    return new Uint8Array(await s.digest("SHA-256", data as BufferSource));
  }
  return sha256Js(data);
}

const K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
  0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
  0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
  0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
  0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
  0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
  0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
  0xc67178f2,
]);

/** Minimal FIPS 180-4 SHA-256, used only when WebCrypto is unavailable. */
export function sha256Js(data: Uint8Array): Uint8Array {
  const h = new Uint32Array([
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
  ]);
  const bitLength = BigInt(data.length) * 8n;
  const padded = new Uint8Array(Math.ceil((data.length + 9) / 64) * 64);
  padded.set(data);
  padded[data.length] = 0x80;
  const view = new DataView(padded.buffer);
  view.setBigUint64(padded.length - 8, bitLength);
  const w = new Uint32Array(64);
  const rotr = (x: number, n: number) => (x >>> n) | (x << (32 - n));
  for (let offset = 0; offset < padded.length; offset += 64) {
    for (let i = 0; i < 16; i++) w[i] = view.getUint32(offset + i * 4);
    for (let i = 16; i < 64; i++) {
      const w15 = w[i - 15]!;
      const w2 = w[i - 2]!;
      const s0 = rotr(w15, 7) ^ rotr(w15, 18) ^ (w15 >>> 3);
      const s1 = rotr(w2, 17) ^ rotr(w2, 19) ^ (w2 >>> 10);
      w[i] = (w[i - 16]! + s0 + w[i - 7]! + s1) >>> 0;
    }
    let [a, b, c, d, e, f, g, hh] = [h[0]!, h[1]!, h[2]!, h[3]!, h[4]!, h[5]!, h[6]!, h[7]!];
    for (let i = 0; i < 64; i++) {
      const S1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
      const ch = (e & f) ^ (~e & g);
      const t1 = (hh + S1 + ch + K[i]! + w[i]!) >>> 0;
      const S0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const t2 = (S0 + maj) >>> 0;
      hh = g;
      g = f;
      f = e;
      e = (d + t1) >>> 0;
      d = c;
      c = b;
      b = a;
      a = (t1 + t2) >>> 0;
    }
    h[0] = (h[0]! + a) >>> 0;
    h[1] = (h[1]! + b) >>> 0;
    h[2] = (h[2]! + c) >>> 0;
    h[3] = (h[3]! + d) >>> 0;
    h[4] = (h[4]! + e) >>> 0;
    h[5] = (h[5]! + f) >>> 0;
    h[6] = (h[6]! + g) >>> 0;
    h[7] = (h[7]! + hh) >>> 0;
  }
  const out = new Uint8Array(32);
  const outView = new DataView(out.buffer);
  h.forEach((word, i) => outView.setUint32(i * 4, word));
  return out;
}

// ---------------------------------------------------------------------------
// Merkle sum tree
// ---------------------------------------------------------------------------

export interface SumNode {
  hash: Uint8Array;
  sum: bigint;
}

export async function hashLeaf(leaf: ProofLeaf, engine?: Engine): Promise<SumNode> {
  const sum = proofUnits(leaf.saldo, "balance");
  const preimage = concat(LEAF_DOMAIN, uuidToBytes(leaf.cuenta_id), u128ToBytes(sum), hexToBytes(leaf.sal, 32));
  return { hash: await sha256(preimage, engine), sum };
}

export async function hashPositionLeaf(leaf: ProofPositionLeaf, engine?: Engine): Promise<SumNode> {
  if (!isTicker(leaf.ticker)) throw new Error(`invalid ticker: ${leaf.ticker}`);
  const sum = proofUnits(leaf.cantidad, "quantity");
  const preimage = concat(
    POSITION_DOMAIN,
    encoder.encode(leaf.ticker),
    new Uint8Array([0x00]),
    uuidToBytes(leaf.cuenta_id),
    u128ToBytes(sum),
    hexToBytes(leaf.sal, 32),
  );
  return { hash: await sha256(preimage, engine), sum };
}

export async function combine(left: SumNode, right: SumNode, engine?: Engine): Promise<SumNode> {
  const sum = left.sum + right.sum;
  if (sum >= U128_LIMIT) throw new Error("sum overflows u128");
  const preimage = concat(NODE_DOMAIN, left.hash, u128ToBytes(left.sum), right.hash, u128ToBytes(right.sum));
  return { hash: await sha256(preimage, engine), sum };
}

/**
 * The side of each sibling on the path from leaf `index` in a tree of
 * `leafCount` leaves, built like the backend's: pairs left to right, and an
 * odd last node moves up unchanged (no step for it at that level).
 */
export function expectedPathSides(index: number, leafCount: number): Side[] {
  if (!Number.isSafeInteger(index) || !Number.isSafeInteger(leafCount) || index < 0 || index >= leafCount) {
    throw new Error("leaf index out of range");
  }
  const sides: Side[] = [];
  let i = index;
  let n = leafCount;
  while (n > 1) {
    const sibling = i % 2 === 0 ? i + 1 : i - 1;
    if (sibling < n) sides.push(sibling < i ? "IZQUIERDA" : "DERECHA");
    i = Math.floor(i / 2);
    n = Math.ceil(n / 2);
  }
  return sides;
}

async function climb(
  leaf: SumNode,
  sentLeafHash: string | null | undefined,
  path: ProofStep[],
  publishedRoot: string,
  publishedTotal: string,
  { engine = defaultEngine(), shape }: VerifyOptions,
): Promise<VerificationResult> {
  if (path.length > MAX_PATH) throw new Error("path too long");
  const leafHash = bytesToHex(leaf.hash);
  let node = leaf;
  const steps: VerifiedStep[] = [];
  for (const [level, step] of path.entries()) {
    if (step.lado !== "IZQUIERDA" && step.lado !== "DERECHA") throw new Error("invalid side");
    const sibling: SumNode = { hash: hexToBytes(step.hash, 32), sum: proofUnits(step.suma, "sibling sum") };
    const [left, right] = step.lado === "IZQUIERDA" ? [sibling, node] : [node, sibling];
    node = await combine(left, right, engine);
    steps.push({
      level: level + 1,
      side: step.lado,
      siblingHash: bytesToHex(sibling.hash),
      siblingSum: fromUnits(sibling.sum),
      leftHash: bytesToHex(left.hash),
      leftSum: fromUnits(left.sum),
      rightHash: bytesToHex(right.hash),
      rightSum: fromUnits(right.sum),
      nodeHash: bytesToHex(node.hash),
      nodeSum: fromUnits(node.sum),
    });
  }
  const shapeMatches = shape
    ? (() => {
        try {
          const expected = expectedPathSides(shape.index, shape.leafCount);
          return expected.length === path.length && expected.every((side, i) => side === path[i]!.lado);
        } catch {
          return false;
        }
      })()
    : null;
  const rootHash = bytesToHex(node.hash);
  const rootMatches = rootHash === publishedRoot.trim().toLowerCase();
  const totalMatches = node.sum === toUnits(publishedTotal);
  return {
    leafHash,
    leafSum: fromUnits(leaf.sum),
    leafHashMatches: sentLeafHash ? sentLeafHash.trim().toLowerCase() === leafHash : null,
    shapeMatches,
    steps,
    rootHash,
    rootSum: fromUnits(node.sum),
    rootMatches,
    totalMatches,
    valid: rootMatches && totalMatches && shapeMatches !== false,
    engine,
  };
}

/**
 * Recomputes the cash root from the leaf and its path, step by step, and
 * compares it with the root and the total liabilities the bank published.
 * With `shape`, the path's turns must also be those of the leaf's position.
 */
export async function verifyInclusion(
  leaf: ProofLeaf,
  path: ProofStep[],
  publishedRoot: string,
  publishedTotal: string,
  options: VerifyOptions = {},
): Promise<VerificationResult> {
  const engine = options.engine ?? defaultEngine();
  return climb(await hashLeaf(leaf, engine), leaf.hash, path, publishedRoot, publishedTotal, { ...options, engine });
}

/** The same for a position, against the published root and total of its asset's tree. */
export async function verifyPositionInclusion(
  leaf: ProofPositionLeaf,
  path: ProofStep[],
  publishedRoot: string,
  publishedTotal: string,
  options: VerifyOptions = {},
): Promise<VerificationResult> {
  const engine = options.engine ?? defaultEngine();
  const node = await hashPositionLeaf(leaf, engine);
  return climb(node, leaf.hash, path, publishedRoot, publishedTotal, { ...options, engine });
}
