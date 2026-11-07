import { keccak256, toBytes, type Hex } from "viem";

/** BN254 scalar field order — the native field of the circuits and of NoteTree.sol. */
export const FIELD = 21888242871839275222246405745257275088548364400416034343698204186575808495617n;
export const DEPTH = 20;
export const WAD = 10n ** 18n;

/** uint256(keccak256(label)) mod p — how every nothing-up-my-sleeve constant is derived (matches Go + circuits). */
export const labelToField = (label: string): bigint => BigInt(keccak256(toBytes(label))) % FIELD;

/** Owner key of intent notes: no known Poseidon pre-image, so no JoinSplit can ever spend one. */
export const BATCH_PUBKEY = labelToField("StrawberrySwap.batch.pubkey");
export const BATCH_NK = labelToField("StrawberrySwap.batch.nullifierKey");
export const TREE_ZERO = labelToField("StrawberrySwap.NoteTree.zero");

export function randomBytes(n: number): Uint8Array<ArrayBuffer> {
  return crypto.getRandomValues(new Uint8Array(n));
}

export function bytesToBigInt(b: Uint8Array): bigint {
  let v = 0n;
  for (const x of b) v = (v << 8n) | BigInt(x);
  return v;
}

/** 31 random bytes = 248 bits < FIELD: uniform enough for blindings and keys, always a reduced element. */
export function randomField(): bigint {
  return bytesToBigInt(randomBytes(31));
}

export function toBytes32(x: bigint): Uint8Array {
  const out = new Uint8Array(32);
  let v = x;
  for (let i = 31; i >= 0; i--) {
    out[i] = Number(v & 0xffn);
    v >>= 8n;
  }
  return out;
}

export function bytesToHex(b: Uint8Array): Hex {
  let s = "0x";
  for (const x of b) s += x.toString(16).padStart(2, "0");
  return s as Hex;
}

export function hexToBytes(h: Hex): Uint8Array {
  const s = h.startsWith("0x") ? h.slice(2) : h;
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = Number.parseInt(s.slice(2 * i, 2 * i + 2), 16);
  return out;
}

export const bigintToHex32 = (x: bigint): Hex => bytesToHex(toBytes32(x));
export const inField = (x: bigint): boolean => x >= 0n && x < FIELD;
