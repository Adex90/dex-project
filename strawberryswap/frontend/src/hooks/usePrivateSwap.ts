"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import type { Address, Hex } from "viem";
import { darkPoolAbi, erc20Abi } from "@/lib/abi";
import { fetchInfo, getIntentStatus, NoteNotYetKnown, submitIntent, validateAutomatorUrl, type AutomatorIntentState } from "@/lib/automator";
import { getWalletClient, publicClient } from "@/lib/clients";
import { config } from "@/lib/config";
import { errMsg } from "@/lib/format";
import { chainHead, toJoinSplitArg } from "@/lib/shielded";
import { bigintToHex32 } from "@/zk/field";
import type { IntentOpening } from "@/zk/notes";
import type { PoolState } from "./useDarkPool";
import type { useShieldedWallet } from "./useShieldedWallet";

export interface Step {
  id: number;
  label: string;
  state: "active" | "done" | "error";
  detail?: string;
}

export interface TrackedIntent {
  commitment: bigint;
  receipt: string;
  state: AutomatorIntentState;
}

const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));

export interface SwapRequest {
  dir: 0 | 1; // asset sold: 0 = token0, 1 = token1
  amount: bigint;
  slippageBps: bigint;
  validitySec: number;
}

/**
 * Orchestrates the three private operations. Every proof is generated in the prover worker; this hook only moves
 * public bundles between the worker, the user's wallet (transactions) and the Automator (intent opening).
 *
 *   deposit  : approve → proof → DarkPool.depositNote
 *   swap     : check Automator → proof → DarkPool.transferNote (creates the INTENT NOTE) → POST opening → poll
 *   withdraw : proof → DarkPool.withdrawNote
 */
export function usePrivateSwap(account: Address | undefined, w: ReturnType<typeof useShieldedWallet>, pool: PoolState | undefined) {
  const poolAddr = config.darkPool;
  const [steps, setSteps] = useState<Step[]>([]);
  const [busy, setBusy] = useState(false);
  const [tracked, setTracked] = useState<TrackedIntent[]>([]);
  const seq = useRef(0);

  const begin = useCallback((label: string): number => {
    const id = ++seq.current;
    setSteps((s) => [...s, { id, label, state: "active" }]);
    return id;
  }, []);
  const end = useCallback((id: number, state: "done" | "error", detail?: string) => {
    setSteps((s) => s.map((x) => (x.id === id ? { ...x, state, ...(detail ? { detail } : {}) } : x)));
  }, []);

  /** Runs one labelled step; marks it done/error and rethrows so the flow stops. */
  const step = useCallback(
    async <T,>(label: string, fn: () => Promise<T>): Promise<T> => {
      const id = begin(label);
      try {
        const r = await fn();
        end(id, "done");
        return r;
      } catch (e) {
        end(id, "error", errMsg(e));
        throw e;
      }
    },
    [begin, end],
  );

  const guard = useCallback(
    async (fn: () => Promise<void>): Promise<void> => {
      if (busy) return;
      setBusy(true);
      try {
        await fn();
      } catch {
        /* the failing step already recorded the reason */
      } finally {
        setBusy(false);
      }
    },
    [busy],
  );

  const requireReady = useCallback(() => {
    if (!poolAddr || !account || !pool) throw new Error("connect a wallet and configure the pool");
    if (w.phase !== "ready") throw new Error("unlock the shielded wallet first");
    const wc = getWalletClient();
    if (!wc) throw new Error("no injected wallet");
    return { wc, poolAddr, account, pool };
  }, [account, pool, poolAddr, w.phase]);

  /** Sync, and refuse to prove against a tree that does not match the chain. */
  const freshState = useCallback(async () => {
    const st = await step("Syncing the note tree from chain events", async () => {
      const s = await w.sync();
      if (!s) throw new Error("sync unavailable");
      return s;
    });
    return st;
  }, [step, w]);

  // -------------------------------------------------------------------------------- deposit
  const deposit = useCallback(
    (assetId: 0 | 1, amount: bigint) =>
      guard(async () => {
        const { wc, poolAddr: pa, account: acct, pool: ps } = requireReady();
        const token = assetId === 0 ? ps.token0 : ps.token1;
        await step("Approving the token", async () => {
          const allowance = await publicClient.readContract({ address: token, abi: erc20Abi, functionName: "allowance", args: [acct, pa] });
          if (allowance >= amount) return;
          const h = await wc.writeContract({ account: acct, address: token, abi: erc20Abi, functionName: "approve", args: [pa, amount] });
          await publicClient.waitForTransactionReceipt({ hash: h });
        });
        await freshState();
        const client = await w.prover.ensureReady();
        const bundle = await step("Generating the zero-knowledge proof (in your browser)", () => client.call("proveDeposit", { assetId: BigInt(assetId), amount, caller: acct }));
        await step("Sending depositNote", async () => {
          const h = await wc.writeContract({ account: acct, address: pa, abi: darkPoolAbi, functionName: "depositNote", args: [bundle.assetId, bundle.amount, toJoinSplitArg(bundle.js)] });
          await publicClient.waitForTransactionReceipt({ hash: h });
        });
        await step("Recovering the new note from chain events", async () => void (await w.sync()));
      }),
    [freshState, guard, requireReady, step, w],
  );

  // -------------------------------------------------------------------------------- swap
  const track = useCallback((commitment: bigint, receipt: string) => {
    setTracked((t) => [...t.filter((x) => x.commitment !== commitment), { commitment, receipt, state: "queued" }]);
  }, []);

  const postOpening = useCallback(
    async (base: string, commitment: bigint, opening: IntentOpening) => {
      // The Automator syncs the chain on demand (rate-limited), so a just-created note may be unknown for a moment.
      let lastErr: unknown;
      for (let i = 0; i < 12; i++) {
        try {
          const receipt = await submitIntent(base, opening);
          track(commitment, receipt);
          return;
        } catch (e) {
          lastErr = e;
          if (!(e instanceof NoteNotYetKnown)) throw e;
          await sleep(5000);
        }
      }
      throw lastErr instanceof Error ? lastErr : new Error("Automator never saw the note");
    },
    [track],
  );

  const swap = useCallback(
    (req: SwapRequest) =>
      guard(async () => {
        const { wc, poolAddr: pa, account: acct } = requireReady();
        const base = config.automatorUrl;
        if (!base) throw new Error("no Automator is configured in this build");
        // 1. Before anything is spent: is this a relay that implements the rules our circuits implement?
        const info = await step("Checking the Automator (batch key, parameters)", async () => {
          validateAutomatorUrl(base);
          return fetchInfo(base);
        });
        if (req.validitySec < info.epochSeconds * 2) {
          const id = begin("Validity window");
          end(id, "error", `must be at least two epochs (${info.epochSeconds * 2} s) so the intent can be batched before it expires`);
          throw new Error("validity too short");
        }
        await freshState();
        const client = await w.prover.ensureReady();

        // 2. Price protection: slippage bound from the current curve quote.
        const quote = await step("Quoting the pool", () => publicClient.readContract({ address: pa, abi: darkPoolAbi, functionName: "getAmountOut", args: [req.amount, req.dir === 0] }));
        const minOut = (quote * (10_000n - req.slippageBps)) / 10_000n;
        const head = await chainHead();
        const deadline = head.timestamp + BigInt(req.validitySec);

        // 3. The proof that moves funds into an intent note (owned by the unspendable batch key).
        const bundle = await step("Generating the zero-knowledge proof (in your browser)", () => client.call("proveIntent", { dir: req.dir, amount: req.amount, minOut, deadline }));
        try {
          await step("Sending transferNote (creates the shielded intent)", async () => {
            const h = await wc.writeContract({ account: acct, address: pa, abi: darkPoolAbi, functionName: "transferNote", args: [bundle.assetId, toJoinSplitArg(bundle.js)] });
            await publicClient.waitForTransactionReceipt({ hash: h });
          });
        } catch (e) {
          await client.call("release", { commitment: bundle.spentCommitment }); // tx failed: the note is still ours
          throw e;
        }
        await step("Recovering the intent from chain events", async () => void (await w.sync()));

        // 4. Hand the Automator the OPENING only (terms, no key). If this fails the funds are NOT lost: the intent
        //    can be resubmitted, and after its deadline it can be refunded.
        await step("Sending the intent to the Automator", () => postOpening(base, bundle.intentCommitment, bundle.opening));
      }),
    [begin, end, freshState, guard, postOpening, requireReady, step, w],
  );

  /** Resubmit intents that exist on-chain but were never (successfully) handed to the Automator. */
  const resubmit = useCallback(
    () =>
      guard(async () => {
        const base = config.automatorUrl;
        if (!base || !w.prover.client) throw new Error("no Automator configured");
        const list = await w.prover.client.call("pendingOpenings", {});
        const known = new Set(tracked.map((t) => t.commitment));
        for (const p of list.filter((x) => !known.has(x.commitment))) await step("Resubmitting a pending intent", () => postOpening(base, p.commitment, p.opening));
      }),
    [guard, postOpening, step, tracked, w.prover.client],
  );

  // -------------------------------------------------------------------------------- withdraw
  const withdraw = useCallback(
    (commitment: bigint) =>
      guard(async () => {
        const { wc, poolAddr: pa, account: acct } = requireReady();
        await freshState();
        const client = await w.prover.ensureReady();
        const bundle = await step("Generating the zero-knowledge proof (in your browser)", () => client.call("proveWithdraw", { commitment, recipient: acct }));
        try {
          await step("Sending withdrawNote", async () => {
            const h = await wc.writeContract({ account: acct, address: pa, abi: darkPoolAbi, functionName: "withdrawNote", args: [bundle.assetId, bundle.amount, bundle.recipient, toJoinSplitArg(bundle.js)] });
            await publicClient.waitForTransactionReceipt({ hash: h });
          });
        } catch (e) {
          await client.call("release", { commitment: bundle.spentCommitment });
          throw e;
        }
        await step("Updating balances", async () => void (await w.sync()));
      }),
    [freshState, guard, requireReady, step, w],
  );

  // -------------------------------------------------------------------------------- status polling
  const syncRef = useRef(w.sync);
  syncRef.current = w.sync;
  useEffect(() => {
    const base = config.automatorUrl;
    if (!base || tracked.every((t) => t.state !== "queued" && t.state !== "inflight")) return;
    const t = setInterval(() => {
      void (async () => {
        const next = await Promise.all(
          tracked.map(async (x) => {
            if (x.state !== "queued" && x.state !== "inflight") return x;
            try {
              return { ...x, state: (await getIntentStatus(base, x.receipt)).state };
            } catch {
              return x;
            }
          }),
        );
        if (next.some((n, i) => n.state !== tracked[i]?.state)) {
          setTracked(next);
          if (next.some((n) => n.state === "settled" || n.state === "refunded")) await syncRef.current().catch(() => undefined);
        }
      })();
    }, 10_000);
    return () => clearInterval(t);
  }, [tracked]);

  return { steps, busy, tracked, deposit, swap, withdraw, resubmit, clearSteps: () => setSteps([]) };
}

export type PrivateSwap = ReturnType<typeof usePrivateSwap>;
export const noteKey = (c: bigint): Hex => bigintToHex32(c);
