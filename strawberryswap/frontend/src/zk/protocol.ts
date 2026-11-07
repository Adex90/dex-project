import type { Address } from "viem";
import type { DepositBundle, IntentBundle, LeafEvent, WalletState, WithdrawBundle } from "./engine";
import type { IntentOpening } from "./notes";

/**
 * Typed request/response contract between the UI thread and the prover worker. Every operation that touches a
 * secret (key derivation, note openings, intent terms, witness construction, proving) exists ONLY behind this API:
 * the UI thread sends a signature once and receives public transaction bundles back.
 */
export interface Api {
  /** Download + hash-verify the proving artifacts (cached), build the prover. */
  init: { req: Record<string, never>; res: { devKeys: boolean; zkeyBytes: number } };
  unlock: { req: { signature: `0x${string}` }; res: { pubKey: bigint } };
  lock: { req: Record<string, never>; res: WalletState };
  addLeaves: { req: { leaves: LeafEvent[] }; res: WalletState };
  markSpent: { req: { nullifiers: bigint[] }; res: WalletState };
  release: { req: { commitment: bigint }; res: WalletState };
  state: { req: Record<string, never>; res: WalletState };
  pendingOpenings: { req: Record<string, never>; res: { commitment: bigint; opening: IntentOpening; deadline: bigint }[] };
  proveDeposit: { req: { assetId: bigint; amount: bigint; caller: Address }; res: DepositBundle };
  proveIntent: { req: { dir: 0 | 1; amount: bigint; minOut: bigint; deadline: bigint }; res: IntentBundle };
  proveWithdraw: { req: { commitment: bigint; recipient: Address }; res: WithdrawBundle };
}

export type ApiKey = keyof Api;

export type WorkerRequest = { [K in ApiKey]: { id: number; type: K; payload: Api[K]["req"] } }[ApiKey];

export interface ProgressMessage {
  type: "progress";
  phase: "download" | "verify" | "prepare" | "prove";
  loaded: number;
  total: number;
  label: string;
}

export type WorkerResponse =
  | { type: "result"; id: number; ok: true; result: unknown }
  | { type: "result"; id: number; ok: false; error: string }
  | ProgressMessage;

/** Shape of src/zk/manifest.generated.json, written by scripts/prepare-zk.mjs. */
export interface ArtifactEntry {
  path: string; // relative to the site root, e.g. "zk/joinsplit.wasm"
  sha256: string;
  bytes: number;
}

export interface ZkManifest {
  circuit: "joinsplit";
  /** true while the keys come from a single-party local setup (never for production). */
  devKeys: boolean;
  artifacts: { wasm: ArtifactEntry; zkey: ArtifactEntry; vkey: ArtifactEntry } | null;
}
