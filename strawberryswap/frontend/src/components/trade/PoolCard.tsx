"use client";

import { bpsToPct, fmt } from "@/lib/format";
import { useApp } from "@/components/AppState";
import { IconZap } from "@/components/ui/icons";
import { Card, CardHeader, Notice, PreLaunch, StatTile } from "@/components/ui/primitives";

/** Live pool market data (read-only). Trading itself happens privately through the swap card. */
export function PoolCard({ className = "" }: { className?: string }) {
  const { pool } = useApp();
  const s = pool.state;
  const loading = pool.configured && !s && !pool.error;
  const how = (
    <p className="mt-4 text-sm text-lavender">
      Swaps execute in batches at one uniform price derived from the curve; only the net flow per asset is public. Arbitrageurs pay the
      Treasury to rebalance the pool, which is what keeps the retail fee at zero.
    </p>
  );

  return (
    <Card className={className} aria-labelledby="pool-title">
      <CardHeader
        icon={<IconZap />}
        title={<span id="pool-title">Dark pool</span>}
        subtitle={pool.configured ? "Live reserves · refreshed every 12 s" : "Zero-fee batch settlement"}
        right={<span className="pill-mint">{s ? bpsToPct(s.feeBps) : "0.00%"} retail fee</span>}
      />
      {!pool.configured ? (
        <>
          <PreLaunch title="The Dark Pool launches with Mainnet">
            Live reserves, the spot price and the MEV bounty appear here once the pool contract is deployed.
          </PreLaunch>
          {how}
        </>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-3">
            <StatTile label={`Reserve ${s?.symbol0 ?? ""}`} value={fmt(s?.reserve0)} loading={loading} />
            <StatTile label={`Reserve ${s?.symbol1 ?? ""}`} value={fmt(s?.reserve1)} loading={loading} />
            <StatTile label="Spot" value={fmt(s?.spot, 18, 6)} sub={s ? `${s.symbol1} per ${s.symbol0}` : undefined} loading={loading} />
            <StatTile
              label="MEV bounty"
              value={!s ? "—" : s.extractable === undefined ? "oracle stale" : fmt(s.extractable)}
              sub={s ? `${Number(s.extractionBps) / 100}% → Treasury` : undefined}
              tone="mint"
              loading={loading}
            />
          </div>
          {how}
          {pool.error ? <Notice tone="err" className="mt-3">Pool read failed: {pool.error}</Notice> : null}
        </>
      )}
    </Card>
  );
}
