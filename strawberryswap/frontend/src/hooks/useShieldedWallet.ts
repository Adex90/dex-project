"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import type { Address } from "viem";
import { getWalletClient } from "@/lib/clients";
import { chain, config } from "@/lib/config";
import { errMsg } from "@/lib/format";
import { chainHead, fetchLeaves, poolRoot, spentAmong } from "@/lib/shielded";
import { keyDerivationMessage } from "@/zk/notes";
import type { WalletState } from "@/zk/engine";
import { useProver } from "./useProver";

export type WalletPhase = "locked" | "unlocking" | "syncing" | "ready" | "error";

/**
 * The shielded wallet, as seen from the UI thread. The key is derived INSIDE the prover worker from a wallet
 * signature; this hook only ever holds public summaries (note values, indices, nullifiers). Closing the tab forgets
 * everything — signing the same message again re-derives the key and the chain scan restores every note.
 */
export function useShieldedWallet(account: Address | undefined) {
  const pool = config.darkPool;
  const prover = useProver();
  const [wallet, setWallet] = useState<WalletState>();
  const [phase, setPhase] = useState<WalletPhase>("locked");
  const [error, setError] = useState<string>();
  const [inSync, setInSync] = useState<boolean>();
  const nextBlock = useRef<bigint>(config.deployBlock);
  const syncing = useRef(false);

  const sync = useCallback(async (): Promise<WalletState | undefined> => {
    if (!pool || syncing.current) return undefined;
    syncing.current = true;
    try {
      const client = await prover.ensureReady();
      const head = await chainHead();
      let st = wallet;
      if (head.number >= nextBlock.current) {
        const leaves = await fetchLeaves(pool, nextBlock.current, head.number);
        st = await client.call("addLeaves", { leaves });
        nextBlock.current = head.number + 1n;
      }
      // Ask the chain which of our notes are already spent, then tell the worker.
      const unspent = (st?.notes ?? []).filter((n) => !n.spent);
      if (unspent.length > 0) st = await client.call("markSpent", { nullifiers: await spentAmong(pool, unspent.map((n) => n.nullifier)) });
      st ??= await client.call("state", {});
      setWallet(st);
      setInSync((await poolRoot(pool)) === st.root); // a mismatch means a missed event: never prove against it
      return st;
    } catch (e) {
      setError(errMsg(e));
      throw e;
    } finally {
      syncing.current = false;
    }
  }, [pool, prover, wallet]);

  const unlock = useCallback(async () => {
    setError(undefined);
    try {
      if (!pool || !account) throw new Error("connect a wallet and configure a DarkPool first");
      const wc = getWalletClient();
      if (!wc) throw new Error("no injected wallet");
      setPhase("unlocking");
      const client = await prover.ensureReady();
      // The signature is sent to the worker and nowhere else; it is the root of the shielded key.
      const message = keyDerivationMessage(chain.id, pool);
      const signature = await wc.signMessage({ account, message });
      // SAFETY: the key is derived from this signature, so it must be reproducible. A wallet that signs
      // non-deterministically would derive a different key next session and strand anything shielded now.
      // Ask twice and refuse unless identical — before any value is shielded.
      const again = await wc.signMessage({ account, message });
      if (signature !== again) throw new Error("this wallet signs non-deterministically, so a recoverable shielded key cannot be derived from it. Use a wallet with deterministic (RFC 6979) signatures.");
      await client.call("unlock", { signature });
      nextBlock.current = config.deployBlock;
      setPhase("syncing");
      await sync();
      setPhase("ready");
    } catch (e) {
      setPhase("error");
      setError(errMsg(e));
    }
  }, [account, pool, prover, sync]);

  const lock = useCallback(async () => {
    if (prover.client) setWallet(await prover.client.call("lock", {}).catch(() => undefined));
    nextBlock.current = config.deployBlock;
    setInSync(undefined);
    setPhase("locked");
  }, [prover.client]);

  // Switching accounts must never leave the previous account's notes in memory.
  const lastAccount = useRef(account);
  useEffect(() => {
    if (lastAccount.current !== account) {
      lastAccount.current = account;
      void lock();
    }
  }, [account, lock]);

  return { prover, wallet, phase, error, inSync, unlock, lock, sync, setWallet, setError };
}
