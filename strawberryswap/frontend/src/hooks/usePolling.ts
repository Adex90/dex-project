"use client";

import { useCallback, useEffect, useRef, useState } from "react";

/** Poll an async loader; pauses while the tab is hidden; exposes manual refresh. */
export function usePolling<T>(load: () => Promise<T>, intervalMs: number, enabled = true) {
  const [data, setData] = useState<T>();
  const [error, setError] = useState<string>();
  const [loading, setLoading] = useState(false);
  const loadRef = useRef(load);
  loadRef.current = load;

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      setData(await loadRef.current());
      setError(undefined);
    } catch (e) {
      const m = (e as { shortMessage?: string } | undefined)?.shortMessage ?? (e instanceof Error ? e.message : "load failed");
      setError(m.split("\n")[0]!.slice(0, 140)); // one short line: raw RPC dumps are noise in a trading UI
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!enabled) return;
    void refresh();
    const t = setInterval(() => {
      if (document.visibilityState === "visible") void refresh();
    }, intervalMs);
    return () => clearInterval(t);
  }, [enabled, intervalMs, refresh]);

  return { data, error, loading, refresh };
}
