"use client";

import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { getProverClient, type ProverClient, type ProverStatus } from "@/zk/client";
import type { ProgressMessage } from "@/zk/protocol";

const IDLE: ProverStatus = { phase: "idle" };
const noopSubscribe = (): (() => void) => () => undefined;

/**
 * The prover worker as React state. `ensureReady()` triggers the (cached, hash-verified) artifact download and
 * resolves when proving is possible. `busyFor` is a live elapsed-seconds counter while a proof is being generated,
 * so the UI can show honest progress without the worker having to guess a percentage.
 */
export function useProver() {
  const client: ProverClient | undefined = useMemo(() => (typeof window === "undefined" ? undefined : getProverClient()), []);
  const status = useSyncExternalStore(client?.subscribe ?? noopSubscribe, client?.getStatus ?? (() => IDLE), () => IDLE);
  const progress = useSyncExternalStore<ProgressMessage | undefined>(client?.subscribe ?? noopSubscribe, client?.getProgress ?? (() => undefined), () => undefined);

  const ensureReady = useCallback(async (): Promise<ProverClient> => {
    if (!client) throw new Error("the prover is only available in the browser");
    await client.init();
    return client;
  }, [client]);

  return { client, status, progress, ensureReady };
}

/** Elapsed whole seconds while `active` is true (0 otherwise). */
export function useElapsed(active: boolean): number {
  const [s, setS] = useState(0);
  const start = useRef(0);
  useEffect(() => {
    if (!active) {
      setS(0);
      return;
    }
    start.current = Date.now();
    const t = setInterval(() => setS(Math.floor((Date.now() - start.current) / 1000)), 500);
    return () => clearInterval(t);
  }, [active]);
  return s;
}
