import { ArrowDownRight, ArrowUpRight, Minus, type LucideIcon } from "lucide-react";

import { cx } from "@/components/ui/primitives";
import { formatMoney } from "@/lib/decimal";
import { moneyParts, shortHash, signedPercent, type Trend } from "@/lib/vault";
import type { DecimalString, ReserveProof, ReserveProofStatus, VaultBalance } from "@/types/vault";

/*
 * Sovereign Balance: the hero of the Bóveda Soberana dashboard, in the
 * "Sovereign Gold" identity (globals.css).
 *
 * A charcoal card on the black ground with a gold hairline on top. The brand
 * yellow marks what is ours: the eyebrow, the peso sign and the reserve seal.
 * The figures are white; green and red appear only for profit and loss.
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

/** The card both the balance and its skeleton sit in. */
const CARD =
  "relative overflow-hidden rounded-2xl border border-border bg-surface p-5 sm:p-7 " +
  // A gold hairline along the top edge: the vault's seal.
  "before:pointer-events-none before:absolute before:inset-x-0 before:top-0 before:h-px " +
  "before:bg-gradient-to-r before:from-transparent before:via-synthetic-yellow/70 before:to-transparent";

export function SovereignBalance({ balance, reserveProof, className }: SovereignBalanceProps) {
  const total = moneyParts(balance.total_mxn);
  const pnl = signedPercent(balance.unrealized_pnl_percentage);
  const trend = TREND[pnl.trend];

  return (
    <section aria-label="Sovereign Vault balance" className={cx(CARD, className)}>
      <header className="flex flex-wrap items-center justify-between gap-3">
        <p className="inline-flex items-center gap-2 text-[11px] font-semibold tracking-[0.24em] text-synthetic-yellow uppercase">
          {/* A gold bar, square-cut: the mark of the eyebrow. */}
          <span aria-hidden className="size-1.5 bg-synthetic-yellow" />
          Sovereign Vault
        </p>
        <ReserveBadge status={reserveProof.status} />
      </header>

      <p className="mt-7 text-sm text-muted">Total balance</p>
      {/* Screen readers get one clean figure; the styled one below is visual only. */}
      <p className="sr-only">{formatMoney(balance.total_mxn)} MXN</p>
      <p aria-hidden className="mt-2 flex flex-wrap items-baseline gap-x-3 gap-y-1" data-testid="vault-total">
        {/* Fluid size: large on desktop, never wider than a 360 px phone for 8-figure balances. */}
        <span className="text-[clamp(2.5rem,10vw,5rem)] leading-none font-light tracking-tighter text-ink tabular-nums">
          {total.negative ? "−" : null}
          <span className="mr-[0.06em] text-[0.5em] font-normal text-synthetic-yellow">$</span>
          {total.integer}
          <span className="text-[0.5em] text-muted">.{total.fraction}</span>
        </span>
        <span className="text-sm font-medium tracking-[0.12em] text-muted">MXN</span>
      </p>

      {/* The sign and the arrow carry the direction too, so color is never the only cue. */}
      <p
        className={cx("mt-4 inline-flex items-center gap-1.5 text-sm font-medium tabular-nums", trend.className)}
        data-testid="vault-pnl"
        data-trend={pnl.trend}
      >
        <trend.Icon className="size-4" aria-hidden />
        <span>{pnl.text}</span>
        <span className="font-normal text-muted">unrealized P&amp;L</span>
      </p>

      {/* Hairline grid: a 1 px gap over the border color draws the dividers. */}
      <dl className="mt-7 grid grid-cols-2 gap-px overflow-hidden rounded-xl bg-border ring-1 ring-border">
        <BalanceStat label="Cash" value={balance.available_cash} />
        <BalanceStat label="Invested" value={balance.invested_assets} />
      </dl>

      <p className="mt-5 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted">
        <span className="font-medium tracking-[0.14em] uppercase">ZK reserve root</span>
        <code className="font-mono text-ink-2" title={reserveProof.zk_root_hash}>
          {shortHash(reserveProof.zk_root_hash)}
        </code>
      </p>
    </section>
  );
}

/* ------------------------------------------------------------------------ */

const TREND: Record<Trend, { Icon: LucideIcon; className: string }> = {
  up: { Icon: ArrowUpRight, className: "text-gain" },
  down: { Icon: ArrowDownRight, className: "text-loss" },
  flat: { Icon: Minus, className: "text-muted" },
};

/**
 * The badge follows `reserveProof.status`: "100% Reserve Verified" appears
 * only for a proof that verified, as a gold seal. A pending epoch is neutral
 * and pulses; a failed proof is red and never pulses.
 */
const RESERVE_BADGE: Record<ReserveProofStatus, { label: string; pill: string; dot: string; pulse: boolean }> = {
  verified: {
    label: "100% Reserve Verified",
    pill: "border-synthetic-yellow/40 bg-synthetic-yellow/10 text-synthetic-yellow",
    dot: "bg-synthetic-yellow",
    pulse: true,
  },
  pending: {
    label: "Verifying reserve",
    pill: "border-border-strong/60 bg-surface-2 text-ink-2",
    dot: "bg-ink-2",
    pulse: true,
  },
  failed: {
    label: "Reserve not verified",
    pill: "border-loss/40 bg-loss/10 text-loss",
    dot: "bg-loss",
    pulse: false,
  },
};

function ReserveBadge({ status }: { status: ReserveProofStatus }) {
  const badge = RESERVE_BADGE[status];
  return (
    <span
      className={cx("inline-flex items-center gap-2 rounded-sm border px-3 py-1 text-xs font-semibold tracking-wide uppercase", badge.pill)}
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
    <div className="bg-surface-2 px-4 py-3.5">
      <dt className="text-[11px] font-medium tracking-[0.18em] text-muted uppercase">{label}</dt>
      <dd className="mt-1.5 text-lg font-medium tracking-tight text-ink tabular-nums">{formatMoney(value)}</dd>
    </div>
  );
}

/** Placeholder with the same footprint, for the store's `loading` state. */
export function SovereignBalanceSkeleton({ className }: { className?: string }) {
  return (
    <div className={cx(CARD, className)} role="status">
      <span className="sr-only">Loading your vault…</span>
      <div aria-hidden className="motion-safe:animate-pulse">
        <div className="flex items-center justify-between gap-3">
          <div className="h-3 w-28 rounded-sm bg-surface-2" />
          <div className="h-6 w-40 rounded-sm bg-surface-2" />
        </div>
        <div className="mt-7 h-3 w-24 rounded-sm bg-surface-2" />
        <div className="mt-3 h-14 w-4/5 max-w-md rounded-2xl bg-surface-2" />
        <div className="mt-5 h-4 w-44 rounded-sm bg-surface-2" />
        <div className="mt-7 h-[74px] rounded-xl bg-surface-2" />
      </div>
    </div>
  );
}
