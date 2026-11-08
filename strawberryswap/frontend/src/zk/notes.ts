import { encodeAbiParameters, keccak256, parseAbiParameters, type Address, type Hex } from "viem";
import { BATCH_PUBKEY, FIELD, bytesToBigInt, bytesToHex, hexToBytes, inField, randomBytes, toBytes32 } from "./field";
import { H } from "./poseidon";

// ============================================================================================ notes

/** A shielded note: Poseidon(assetId, value, pubKey, blinding) is its leaf in the note tree. */
export interface Note {
  assetId: bigint;
  value: bigint;
  pubKey: bigint;
  blinding: bigint;
  commitment: bigint;
}

export function makeNote(assetId: bigint, value: bigint, pubKey: bigint, blinding: bigint): Note {
  return { assetId, value, pubKey, blinding, commitment: H(assetId, value, pubKey, blinding) };
}

/** nullifier = Poseidon(commitment, leafIndex, privKey) — published when the note is spent. */
export const nullifierOf = (n: Note, leafIndex: number, privKey: bigint): bigint => H(n.commitment, BigInt(leafIndex), privKey);

// ============================================================================================ keys

/** The message the user signs to derive their shielded key. Binding chain + pool stops cross-deployment reuse. */
export function keyDerivationMessage(chainId: number, pool: Address): string {
  return [
    "StrawberrySwap shielded wallet — key derivation v1",
    `Chain: ${chainId}`,
    `Pool: ${pool.toLowerCase()}`,
    "",
    "Sign this ONLY inside the official StrawberrySwap app. The signature never leaves your device;",
    "it derives the key that controls your shielded notes. It does not authorise any transaction.",
  ].join("\n");
}

export interface ShieldedKeys {
  privKey: bigint;
  pubKey: bigint;
}

/** privKey = keccak256(signature) mod p (never 0); pubKey = Poseidon(privKey). Deterministic ECDSA (RFC 6979) makes this recoverable. */
export function deriveKeys(signature: Hex): ShieldedKeys {
  let priv = BigInt(keccak256(signature)) % FIELD;
  if (priv === 0n) priv = 1n;
  return { privKey: priv, pubKey: H(priv) };
}

// ============================================================================================ intent notes

/** Terms of a swap intent; hashed into the intent note's blinding, so they cannot change after creation. */
export interface IntentTerms {
  dir: 0 | 1; // asset sold into the pool: 0 = token0, 1 = token1
  value: bigint;
  destPubKey: bigint;
  refundPubKey: bigint;
  destBlinding: bigint;
  minOut: bigint;
  deadline: bigint; // unix seconds
}

export const intentBlinding = (t: IntentTerms): bigint => H(t.destPubKey, t.refundPubKey, t.destBlinding, t.minOut, t.deadline);

/** The note a user must create (with a JoinSplit) for a swap: owned by the unspendable BATCH_PUBKEY. */
export function makeIntentNote(t: IntentTerms): Note {
  return makeNote(BigInt(t.dir), t.value, BATCH_PUBKEY, intentBlinding(t));
}

/** The output the batch produces for a swap (assetOut = 1 - dir) or a refund (assetOut = dir). */
export const swapOutputCommitment = (t: IntentTerms, payout: bigint): bigint => H(BigInt(1 - t.dir), payout, t.destPubKey, t.destBlinding);
export const refundOutputCommitment = (t: IntentTerms): bigint => H(BigInt(t.dir), t.value, t.refundPubKey, t.destBlinding);

/** What the Automator's POST /v1/intents accepts (see bots/automator/internal/intent): big integers as strings. */
export interface IntentOpening {
  dir: 0 | 1;
  value: string;
  destPubKey: string;
  refundPubKey: string;
  destBlinding: string;
  minOut: string;
  deadline: number;
}

export function toOpening(t: IntentTerms): IntentOpening {
  return {
    dir: t.dir, value: t.value.toString(), destPubKey: t.destPubKey.toString(), refundPubKey: t.refundPubKey.toString(),
    destBlinding: t.destBlinding.toString(), minOut: t.minOut.toString(), deadline: Number(t.deadline),
  };
}

// ============================================================================================ extDataHash

/** DarkPool.depositNote / withdrawNote bind (address, ciphertext0, ciphertext1): keccak256(abi.encode(...)) mod p. */
export function extHashWithAddress(addr: Address, enc0: Hex, enc1: Hex): bigint {
  return BigInt(keccak256(encodeAbiParameters(parseAbiParameters("address, bytes, bytes"), [addr, enc0, enc1]))) % FIELD;
}

/** DarkPool.transferNote binds only the ciphertexts. */
export function extHashNoAddress(enc0: Hex, enc1: Hex): bigint {
  return BigInt(keccak256(encodeAbiParameters(parseAbiParameters("bytes, bytes"), [enc0, enc1]))) % FIELD;
}

// ============================================================================================ ciphertexts

const te = new TextEncoder();

async function aesKey(domain: string, secret: bigint): Promise<CryptoKey> {
  const material = new Uint8Array([...te.encode(domain), ...toBytes32(secret)]);
  const digest = await crypto.subtle.digest("SHA-256", new Uint8Array(material));
  return crypto.subtle.importKey("raw", digest, "AES-GCM", false, ["encrypt", "decrypt"]);
}

/** Self-encrypted payloads are fixed length so notes created by this wallet are indistinguishable from padding. */
export const SELF_PLAINTEXT_LEN = 1 + 5 * 32;
export const SELF_CIPHERTEXT_LEN = 12 + SELF_PLAINTEXT_LEN + 16;
export const SEALED_OUTPUT_LEN = 12 + 33 + 16; // produced by the Automator (Go intent.Seal)

export type SelfPayload =
  | { kind: "note"; assetId: bigint; value: bigint; blinding: bigint }
  | { kind: "intent"; dir: 0 | 1; value: bigint; destBlinding: bigint; minOut: bigint; deadline: bigint };

const fields = (p: SelfPayload): [number, bigint[]] =>
  p.kind === "note" ? [1, [p.assetId, p.value, p.blinding]] : [2, [BigInt(p.dir), p.value, p.destBlinding, p.minOut, p.deadline]];

/** AES-256-GCM under a key derived from the shielded private key. The wallet re-discovers its notes from chain events. */
export async function sealSelf(privKey: bigint, p: SelfPayload): Promise<Hex> {
  const [kind, fs] = fields(p);
  const pt = new Uint8Array(SELF_PLAINTEXT_LEN);
  pt[0] = kind;
  fs.forEach((f, i) => pt.set(toBytes32(f), 1 + 32 * i));
  const nonce = randomBytes(12);
  const ct = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv: nonce }, await aesKey("StrawberrySwap.selfnote.v1", privKey), pt));
  return bytesToHex(new Uint8Array([...nonce, ...ct]));
}

/** Random bytes of exactly the self-ciphertext length: padding that is indistinguishable from a real note. */
export const paddingCiphertext = (): Hex => bytesToHex(randomBytes(SELF_CIPHERTEXT_LEN));

export async function openSelf(privKey: bigint, blob: Hex): Promise<SelfPayload | undefined> {
  const b = hexToBytes(blob);
  if (b.length !== SELF_CIPHERTEXT_LEN) return undefined;
  let pt: Uint8Array;
  try {
    pt = new Uint8Array(await crypto.subtle.decrypt({ name: "AES-GCM", iv: new Uint8Array(b.slice(0, 12)) }, await aesKey("StrawberrySwap.selfnote.v1", privKey), new Uint8Array(b.slice(12))));
  } catch {
    return undefined; // not ours
  }
  const f = (i: number): bigint => bytesToBigInt(pt.slice(1 + 32 * i, 33 + 32 * i));
  if (pt[0] === 1) return { kind: "note", assetId: f(0), value: f(1), blinding: f(2) };
  if (pt[0] === 2 && (f(0) === 0n || f(0) === 1n)) return { kind: "intent", dir: Number(f(0)) as 0 | 1, value: f(1), destBlinding: f(2), minOut: f(3), deadline: f(4) };
  return undefined;
}

/**
 * Opens the output blob the Automator seals for the intent's owner (AES-256-GCM, key = SHA-256("…outnote.v1" ‖ destBlinding)).
 * Format identical to Go `intent.Seal`: nonce(12) ‖ ct(assetOut(1) ‖ value(32)) ‖ tag(16).
 */
export async function openSealedOutput(destBlinding: bigint, blob: Hex): Promise<{ assetOut: 0 | 1; value: bigint } | undefined> {
  const b = hexToBytes(blob);
  if (b.length !== SEALED_OUTPUT_LEN || !inField(destBlinding)) return undefined;
  try {
    const pt = new Uint8Array(await crypto.subtle.decrypt({ name: "AES-GCM", iv: new Uint8Array(b.slice(0, 12)) }, await aesKey("StrawberrySwap.outnote.v1", destBlinding), new Uint8Array(b.slice(12))));
    if (pt.length !== 33 || (pt[0] !== 0 && pt[0] !== 1)) return undefined;
    return { assetOut: pt[0] as 0 | 1, value: bytesToBigInt(pt.slice(1)) };
  } catch {
    return undefined;
  }
}
