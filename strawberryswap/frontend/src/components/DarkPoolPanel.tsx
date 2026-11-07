"use client";

import type { useDarkPool } from "@/hooks/useDarkPool";
import { fmt } from "@/lib/format";
import { Notice, Panel, Stat } from "./Panel";

/** Market data for the pool. Trading happens privately in § 3 (shielded wallet), never from this panel. */
export function DarkPoolPanel({ pool }: { pool: ReturnType<typeof useDarkPool> }) {
  const s = pool.state;

  return (
    <Panel no="1" title="Dark Pool" tag="Retail fee 0.00%">
      {!pool.configured ? (
        <Notice tone="warn">No DarkPool address in this build. Set NEXT_PUBLIC_DARKPOOL_ADDRESS and rebuild.</Notice>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-x-8 gap-y-5">
            <Stat label={`Reserve ${s?.symbol0 ?? "T0"}`} value={fmt(s?.reserve0)} />
            <Stat label={`Reserve ${s?.symbol1 ?? "T1"}`} value={fmt(s?.reserve1)} />
            <Stat label="Spot" value={fmt(s?.spot, 18, 6)} sub={s ? `${s.symbol1}/${s.symbol0}` : undefined} tone="info" />
            <Stat
              label="MEV bounty"
              value={!s ? "—" : s.extractable === undefined ? "oracle stale" : fmt(s.extractable)}
              sub={s ? `${Number(s.extractionBps) / 100}% → Treasury` : undefined}
              tone="pos"
            />
          </div>
          <p className="mt-6 text-[15px] text-muted">
            Swaps execute in <em>batches</em> at one uniform price derived from the curve; only the net flow per asset is public. To trade,
            use the shielded wallet in § 3 — your keys and trade parameters stay on this device.
          </p>
          {pool.error ? <Notice tone="err">Pool read failed: {pool.error}</Notice> : null}
        </>
      )}
    </Panel>
  );
}
