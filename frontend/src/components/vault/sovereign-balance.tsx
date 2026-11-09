import { ArrowDownRight, ArrowUpRight, Minus, type LucideIcon } from "lucide-react";

import { cx } from "@/components/ui/primitives";
import { formatMoney } from "@/lib/decimal";
import { moneyParts, shortHash, signedPercent, type Trend } from "@/lib/vault";
import type { DecimalString, ReserveProof, ReserveProofStatus, VaultBalance } from "@/types/vault";

/*
 * Sovereign Balance: the hero of the Bóveda Soberana dashboard.
 *
 * "Midnight" aesthetic: pure black, white figures, zinc-400 for secondary
 * text (8.6:1 on black, so even the small labels pass WCAG AA), and color
 * only where it carries meaning (reserve state, profit or loss).
 *
 * Presentational and hook-free, so it renders as a Server Component or
 * inside a client tree alike. Feed it from `useVaultStore` (balance,
 * reserveProof). Amounts are decimal strings, formatted exactly.
 */

export interface SovereignBalanceProps {
  balance: VaultBalance;
  reserveProof: ReserveProof;
  className?: string;
}

export function SovereignBalance({ balance, reserveProof, className }: SovereignBalanceProps) {
  const total = moneyParts(balance.total_mxn);
  const pnl = signedPercent(balance.unrealized_pnl_percentage);
  const trend = TREND[pnl.trend];

  return (
    <section aria-label="Sovereign Vault balance" className={cx("bg-black text-white", className)}>
      <header className="flex flex-wrap items-center justify-between gap-3">
        <p className="text-[11px] font-medium tracking-[0.22em] text-zinc-400 uppercase">Sovereign Vault</p>
        <ReserveBadge status={reserveProof.status} />
      </header>

      <p className="mt-8 text-sm text-zinc-400">Total balance</p>
      {/* Screen readers get one clean figure; the styled one below is visual only. */}
      <p className="sr-only">
        {formatMoney(balance.total_mxn)} MXN
      </p>
      <p
        aria-hidden
        className="mt-2 flex flex-wrap items-baseline gap-x-3 gap-y-1"
        data-testid="vault-total"
      >
        {/* Fluid size: large on desktop, never wider than a 360 px phone for 8-figure balances. */}
        <span className="text-[clamp(2.5rem,11vw,5.5rem)] leading-none font-light tracking-tighter tabular-nums">
          {total.negative ? "−" : null}
          <span className="mr-[0.06em] text-[0.5em] font-normal text-zinc-400">$</span>
          {total.integer}
          <span className="text-[0.5em] text-zinc-400">.{total.fraction}</span>
        </span>
        <span className="text-sm font-medium tracking-wide text-zinc-400">MXN</span>
      </p>

      {/* The sign and the arrow carry the direction too, so color is never the only cue. */}
      <p
        className={cx("mt-4 inline-flex items-center gap-1.5 text-sm font-medium tabular-nums", trend.className)}
        data-testid="vault-pnl"
        data-trend={pnl.trend}
      >
        <trend.Icon className="size-4" aria-hidden />
        <span>{pnl.text}</span>
        <span className="font-normal text-zinc-400">unrealized P&amp;L</span>
      </p>

      {/* Hairline grid: a 1 px gap over a translucent white backdrop draws the dividers. */}
      <dl className="mt-8 grid grid-cols-2 gap-px overflow-hidden rounded-2xl bg-white/10 ring-1 ring-white/10">
        <BalanceStat label="Cash" value={balance.available_cash} />
        <BalanceStat label="Invested" value={balance.invested_assets} />
      </dl>

      <p className="mt-6 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-zinc-400">
        <span>ZK reserve root</span>
        <code className="font-mono text-zinc-300" title={reserveProof.zk_root_hash}>
          {shortHash(reserveProof.zk_root_hash)}
        </code>
      </p>
    </section>
  );
}

/* ------------------------------------------------------------------------ */

const TREND: Record<Trend, { Icon: LucideIcon; className: string }> = {
  up: { Icon: ArrowUpRight, className: "text-emerald-400" },
  down: { Icon: ArrowDownRight, className: "text-rose-400" },
  flat: { Icon: Minus, className: "text-zinc-400" },
};

/**
 * The badge follows `reserveProof.status`: "100% Reserve Verified" appears
 * only for a proof that verified. A pending epoch pulses amber; a failed
 * proof is shown as such and never pulses.
 */
const RESERVE_BADGE: Record<ReserveProofStatus, { label: string; pill: string; dot: string; pulse: boolean }> = {
  verified: {
    label: "100% Reserve Verified",
    pill: "border-emerald-400/25 bg-emerald-400/10 text-emerald-300",
    dot: "bg-emerald-400",
    pulse: true,
  },
  pending: {
    label: "Verifying reserve",
    pill: "border-amber-300/25 bg-amber-300/10 text-amber-200",
    dot: "bg-amber-300",
    pulse: true,
  },
  failed: {
    label: "Reserve not verified",
    pill: "border-rose-400/30 bg-rose-400/10 text-rose-300",
    dot: "bg-rose-400",
    pulse: false,
  },
};

function ReserveBadge({ status }: { status: ReserveProofStatus }) {
  const badge = RESERVE_BADGE[status];
  return (
    <span
      className={cx("inline-flex items-center gap-2 rounded-full border px-3 py-1 text-xs font-medium", badge.pill)}
      data-testid="reserve-badge"
      data-status={status}
    >
      <span className="relative flex size-2" aria-hidden>
        {/* A slow ping, and none at all for people who prefer reduced motion. */}
        {badge.pulse ? (
          <span
            className={cx(
              "absolute inline-flex size-full rounded-full opacity-60 motion-safe:animate-ping motion-safe:[animation-duration:2.4s]",
              badge.dot,
            )}
          />
        ) : null}
        <span className={cx("relative inline-flex size-2 rounded-full", badge.dot)} />
      </span>
      {badge.label}
    </span>
  );
}

function BalanceStat({ label, value }: { label: string; value: DecimalString }) {
  return (
    <div className="bg-black px-4 py-3.5">
      <dt className="text-[11px] font-medium tracking-[0.18em] text-zinc-400 uppercase">{label}</dt>
      <dd className="mt-1.5 text-lg font-medium tracking-tight text-white tabular-nums">{formatMoney(value)}</dd>
    </div>
  );
}

/** Placeholder with the same footprint, for the store's `loading` state. */
export function SovereignBalanceSkeleton({ className }: { className?: string }) {
  return (
    <div className={cx("bg-black", className)} role="status">
      <span className="sr-only">Loading your vault…</span>
      <div aria-hidden className="motion-safe:animate-pulse">
        <div className="flex items-center justify-between gap-3">
          <div className="h-3 w-28 rounded-full bg-zinc-900" />
          <div className="h-6 w-40 rounded-full bg-zinc-900" />
        </div>
        <div className="mt-8 h-3 w-24 rounded-full bg-zinc-900" />
        <div className="mt-3 h-14 w-4/5 max-w-md rounded-2xl bg-zinc-900" />
        <div className="mt-5 h-4 w-44 rounded-full bg-zinc-900" />
        <div className="mt-8 h-[74px] rounded-2xl bg-zinc-900" />
      </div>
    </div>
  );
}
