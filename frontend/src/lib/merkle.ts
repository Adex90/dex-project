/**
 * Client-side verification of Proof-of-Reserve inclusion proofs.
 *
 * Re-implements, independently of the bank, the Merkle sum tree defined in
 * backend/src/crypto/merkle.rs ("SHA-256 merkle-sum v1"):
 *
 *   leaf  = SHA256("SC-POR-v1:hoja" 0x00 || account_id[16] || balance_u128_be[16] || salt[32])
 *   node  = SHA256("SC-POR-v1:nodo" 0x01 || L.hash[32] || L.sum_be[16] || R.hash[32] || R.sum_be[16])
 *
 * Balances are integers in units of 10^-8 MXN. Each node commits to the sums
 * of BOTH children, so the bank cannot hide liabilities by under-reporting a
 * sibling's sum: any change breaks the hash chain up to the published root.
 *
 * Hashing uses WebCrypto when available and falls back to a pure-JS SHA-256
 * (WebCrypto is only exposed in secure contexts: HTTPS or localhost).
 */

import { fromUnits, toUnits } from "./decimal.ts";

export type Side = "IZQUIERDA" | "DERECHA";

export interface ProofLeaf {
  cuenta_id: string;
  /** Balance at the cut, 8-decimal string. */
  saldo: string;
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
  nodeHash: string;
  nodeSum: string;
}

export interface VerificationResult {
  leafHash: string;
  /** The leaf hash we computed matches the one the bank sent. */
  leafHashMatches: boolean | null;
  steps: VerifiedStep[];
  rootHash: string;
  rootSum: string;
  rootMatches: boolean;
  totalMatches: boolean;
  valid: boolean;
  engine: "webcrypto" | "js";
}

const encoder = new TextEncoder();
const LEAF_DOMAIN = concat(encoder.encode("SC-POR-v1:hoja"), new Uint8Array([0x00]));
const NODE_DOMAIN = concat(encoder.encode("SC-POR-v1:nodo"), new Uint8Array([0x01]));

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
  return hexToBytes(uuid.replace(/-/g, ""), 16);
}

export function u128ToBytes(value: bigint): Uint8Array {
  if (value < 0n || value >= 1n << 128n) throw new Error("value out of u128 range");
  const out = new Uint8Array(16);
  let v = value;
  for (let i = 15; i >= 0; i--) {
    out[i] = Number(v & 0xffn);
    v >>= 8n;
  }
  return out;
}

// ---------------------------------------------------------------------------
// SHA-256
// ---------------------------------------------------------------------------

type Engine = "webcrypto" | "js";

function subtle(): SubtleCrypto | null {
  const c = (globalThis as { crypto?: Crypto }).crypto;
  return c?.subtle ?? null;
}

export async function sha256(data: Uint8Array, engine: Engine = subtle() ? "webcrypto" : "js"): Promise<Uint8Array> {
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
  const sum = toUnits(leaf.saldo);
  const preimage = concat(LEAF_DOMAIN, uuidToBytes(leaf.cuenta_id), u128ToBytes(sum), hexToBytes(leaf.sal, 32));
  return { hash: await sha256(preimage, engine), sum };
}

export async function combine(left: SumNode, right: SumNode, engine?: Engine): Promise<SumNode> {
  const preimage = concat(NODE_DOMAIN, left.hash, u128ToBytes(left.sum), right.hash, u128ToBytes(right.sum));
  return { hash: await sha256(preimage, engine), sum: left.sum + right.sum };
}

/**
 * Recomputes the root from the leaf and its path, and compares it with the
 * root and total liabilities the bank published for the cut.
 */
export async function verifyInclusion(
  leaf: ProofLeaf,
  path: ProofStep[],
  publishedRoot: string,
  publishedTotal: string,
  engine: Engine = subtle() ? "webcrypto" : "js",
): Promise<VerificationResult> {
  if (path.length > 64) throw new Error("path too long");
  let node = await hashLeaf(leaf, engine);
  const leafHash = bytesToHex(node.hash);
  const steps: VerifiedStep[] = [];
  for (const [level, step] of path.entries()) {
    const sibling: SumNode = { hash: hexToBytes(step.hash, 32), sum: toUnits(step.suma) };
    if (sibling.sum < 0n) throw new Error("negative sibling sum");
    node = step.lado === "IZQUIERDA" ? await combine(sibling, node, engine) : await combine(node, sibling, engine);
    steps.push({
      level: level + 1,
      side: step.lado,
      siblingHash: step.hash.toLowerCase(),
      siblingSum: fromUnits(sibling.sum),
      nodeHash: bytesToHex(node.hash),
      nodeSum: fromUnits(node.sum),
    });
  }
  const rootHash = bytesToHex(node.hash);
  const rootMatches = rootHash === publishedRoot.trim().toLowerCase();
  const totalMatches = node.sum === toUnits(publishedTotal);
  return {
    leafHash,
    leafHashMatches: leaf.hash ? leaf.hash.toLowerCase() === leafHash : null,
    steps,
    rootHash,
    rootSum: fromUnits(node.sum),
    rootMatches,
    totalMatches,
    valid: rootMatches && totalMatches,
    engine,
  };
}
