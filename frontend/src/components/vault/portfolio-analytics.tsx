"use client";

import { useId, useMemo, useState } from "react";
import Link from "next/link";
import { ArrowUpRight, ChartPie } from "lucide-react";

import { cx } from "@/components/ui/primitives";
import { allocation, cashBreakdown, formatShare, type AllocationPart } from "@/lib/allocation";
import { formatMoney, isZero } from "@/lib/decimal";
import { assetPath } from "@/lib/market-data";
import type { Holding, VaultBalance } from "@/types/vault";

/*
 * Portfolio Analytics: where the vault's money sits, as one segmented bar
 * over the list of its parts, in the Sovereign Gold identity.
 *
 * - The header leads with the split: what share is invested, what share is
 *   cash.
 * - The bar: cash in Synthetic Gold, then each investment, largest first, on
 *   a zinc scale (lighter is larger; Other, the smallest holdings folded
 *   together, is the darkest). Widths are the exact shares, with a 2 px gap
 *   of the card's surface between parts; a part under a hundredth of a
 *   percent draws nothing (the list still names it). The scale was checked
 *   against the charcoal surface: gold and four zinc steps stay apart, which
 *   is why the bar holds at most five parts (lib/allocation.ts).
 * - Pointing at a part, or at its row, lights up both and reads its figures
 *   under the bar; the gaps, the order and the list carry identity too, so
 *   color is never the only cue.
 * - The list is the bar's table: each part's swatch, name, share and value
 *   (shares exact, adding up to 100.00 %). Holdings open their asset's page;
 *   cash says what it is made of (available, held for open orders, on hold).
 *
 * Presentational: feed it `balance` and `holdings` from `useVaultStore`, and
 * it follows the vault live. The math is lib/allocation.ts (BigInt, exact).
 */

export interface PortfolioAnalyticsProps {
  balance: VaultBalance;
  holdings: readonly Holding[];
  className?: string;
}

/** The parts' colors: gold for cash, then the zinc scale by place among the investments, lighter first. */
const CASH_SWATCH = "bg-synthetic-yellow";
const ZINC_SWATCHES = ["bg-zinc-200", "bg-zinc-400", "bg-zinc-500", "bg-zinc-600"] as const;

function swatchesFor(parts: readonly AllocationPart[]): string[] {
  let investment = 0;
  return parts.map((part) =>
    part.kind === "cash" ? CASH_SWATCH : ZINC_SWATCHES[Math.min(investment++, ZINC_SWATCHES.length - 1)]!,
  );
}

const CARD = "border border-border bg-surface p-5 sm:p-6";

export function PortfolioAnalytics({ balance, holdings, className }: PortfolioAnalyticsProps) {
  const titleId = useId();
  const split = useMemo(() => allocation(balance, holdings), [balance, holdings]);
  const swatches = useMemo(() => swatchesFor(split.parts), [split.parts]);
  const [hovered, setHovered] = useState<string | null>(null);
  const empty = split.parts.length === 0;
  const focus = split.parts.find((part) => part.key === hovered) ?? null;
  const invested = empty ? 0 : 10_000 - split.cashBasisPoints;
  const allCash = !empty && split.parts.every((part) => part.kind === "cash");

  return (
    <section aria-labelledby={titleId} className={cx(CARD, className)} data-testid="portfolio-analytics">
      <div className="grid gap-6 xl:grid-cols-[minmax(0,1fr)_minmax(0,1.15fr)] xl:gap-10">
        {/* From xl the figures and the bar sit at the bottom, level with the end of the list beside them. */}
        <div className="flex min-w-0 flex-col">
          <header className="flex min-w-0 items-start gap-3">
            <span
              aria-hidden
              className="grid size-10 shrink-0 place-items-center bg-synthetic-yellow/10 text-synthetic-yellow ring-1 ring-synthetic-yellow/25"
            >
              <ChartPie className="size-[18px]" />
            </span>
            <div className="min-w-0">
              <h2 id={titleId} className="text-base font-semibold tracking-tight">
                Allocation
              </h2>
              <p className="mt-0.5 text-sm text-muted">Your vault, part by part.</p>
            </div>
          </header>

          <div className="mt-6 xl:mt-auto xl:pt-6">
            {empty ? null : (
              <dl className="flex gap-8" data-testid="allocation-split">
                <Headline term="Invested" value={formatShare(invested)} />
                <Headline term="Cash" value={formatShare(split.cashBasisPoints)} gold />
              </dl>
            )}

            <AllocationBar
              className={empty ? undefined : "mt-5"}
              parts={split.parts}
              swatches={swatches}
              hovered={hovered}
              onHover={setHovered}
            />

            {/* The readout: the part pointed at, or the whole. One line, always there, so nothing jumps. */}
            <p
              className="mt-3 flex min-h-5 flex-wrap items-baseline gap-x-2 text-xs text-muted tabular-nums"
              aria-hidden
              data-testid="allocation-readout"
            >
              {focus ? (
                <>
                  <span className="font-semibold text-ink">{focus.label}</span>
                  <span>{focus.kind === "cash" ? cashDescription(balance) : focus.detail}</span>
                  <span className="text-ink">{formatMoney(focus.value)}</span>
                  <span className="font-semibold text-ink">{formatShare(focus.basisPoints, focus.value)}</span>
                </>
              ) : empty ? (
                <span>Your allocation appears with your first deposit.</span>
              ) : (
                <>
                  <span>Total</span>
                  <span className="font-semibold text-ink">{formatMoney(split.total)} MXN</span>
                  <span>
                    · {split.parts.length} {split.parts.length === 1 ? "part" : "parts"}
                  </span>
                </>
              )}
            </p>
          </div>
        </div>

        {empty ? (
          <EmptyParts />
        ) : (
          <div className="min-w-0">
            <ul className="divide-y divide-border border border-border" data-testid="allocation-parts">
              {split.parts.map((part, i) => (
                <PartRow
                  key={part.key}
                  part={part}
                  swatch={swatches[i]!}
                  detail={part.kind === "cash" ? cashDescription(balance) : part.detail}
                  lit={hovered === part.key}
                  dim={hovered !== null && hovered !== part.key}
                  onHover={setHovered}
                />
              ))}
            </ul>
            {allCash ? (
              <p className="mt-3 text-xs text-muted" data-testid="allocation-all-cash">
                Everything is in cash.{" "}
                <Link
                  href="/dashboard/discover"
                  className="font-medium text-synthetic-yellow underline-offset-2 hover:underline focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
                >
                  Discover assets
                </Link>
              </p>
            ) : null}
          </div>
        )}
      </div>
    </section>
  );
}

/* ------------------------------------------------------------------------ */

/** A headline share: large, in the same sans, proportional figures. */
function Headline({ term, value, gold = false }: { term: string; value: string; gold?: boolean }) {
  return (
    <div>
      <dt className="flex items-center gap-1.5 text-[11px] font-semibold tracking-[0.2em] text-muted uppercase">
        <span aria-hidden className={cx("size-2", gold ? "bg-synthetic-yellow" : "bg-zinc-400")} />
        {term}
      </dt>
      <dd className="mt-1 text-2xl font-semibold tracking-tight text-ink sm:text-3xl">{value}</dd>
    </div>
  );
}

/**
 * The bar: one block per part, as wide as its share (flex-grow in basis
 * points, so the 2 px gaps come off every part alike). An image to assistive
 * technology, named with every part's share; the list below has the rest.
 */
function AllocationBar({
  parts,
  swatches,
  hovered,
  onHover,
  className,
}: {
  parts: readonly AllocationPart[];
  swatches: readonly string[];
  hovered: string | null;
  onHover: (key: string | null) => void;
  className?: string;
}) {
  const drawn = parts.map((part, i) => ({ part, swatch: swatches[i]! })).filter(({ part }) => part.basisPoints > 0);
  const name =
    parts.length === 0
      ? "Allocation: nothing yet"
      : `Allocation: ${parts.map((part) => `${part.label} ${formatShare(part.basisPoints, part.value)}`).join(", ")}`;
  return (
    <div
      role="img"
      aria-label={name}
      className={cx(
        "flex h-3 gap-0.5",
        drawn.length === 0 && "border border-dashed border-border-strong/60",
        className,
      )}
      onPointerLeave={() => onHover(null)}
      data-testid="allocation-bar"
    >
      {drawn.map(({ part, swatch }) => (
        <span
          key={part.key}
          className={cx(
            "h-full min-w-px basis-0 transition-[flex-grow,opacity] duration-500 motion-reduce:transition-none",
            swatch,
            hovered !== null && hovered !== part.key && "opacity-35",
          )}
          style={{ flexGrow: part.basisPoints }}
          onPointerEnter={() => onHover(part.key)}
          data-testid="allocation-segment"
          data-key={part.key}
          data-share={part.basisPoints}
        />
      ))}
    </div>
  );
}

/** One part in the list: a link to the asset's page for a holding. */
function PartRow({
  part,
  swatch,
  detail,
  lit,
  dim,
  onHover,
}: {
  part: AllocationPart;
  swatch: string;
  detail: string;
  lit: boolean;
  dim: boolean;
  onHover: (key: string | null) => void;
}) {
  const body = (
    <>
      <span aria-hidden className={cx("size-2.5 shrink-0", swatch)} />
      <span className="min-w-0 flex-1">
        <span
          className={cx(
            "block truncate text-sm font-semibold text-ink",
            (part.kind === "holding" || part.kind === "other") && "font-mono tracking-wide",
          )}
        >
          {part.label}
        </span>
        <span className="mt-0.5 block text-xs break-words text-muted">{detail}</span>
      </span>
      <span className="shrink-0 text-right">
        <span className="block text-sm font-semibold text-ink tabular-nums" data-testid="allocation-share">
          {formatShare(part.basisPoints, part.value)}
        </span>
        <span className="mt-0.5 block text-xs text-muted tabular-nums">{formatMoney(part.value)}</span>
      </span>
      {part.kind === "holding" ? (
        <ArrowUpRight
          aria-hidden
          className="size-4 shrink-0 text-muted transition-colors group-hover:text-synthetic-yellow group-focus-visible:text-synthetic-yellow"
        />
      ) : (
        <span aria-hidden className="size-4 shrink-0" />
      )}
    </>
  );
  const row = cx(
    "flex items-center gap-3 px-4 py-3 transition-[background-color,opacity] duration-300",
    lit ? "bg-surface-2" : "bg-surface",
    dim && "opacity-60",
  );
  const hover = { onPointerEnter: () => onHover(part.key), onPointerLeave: () => onHover(null) };
  return (
    <li data-testid="allocation-part" data-key={part.key} data-kind={part.kind}>
      {part.kind === "holding" ? (
        <Link
          href={assetPath(part.key)}
          className={cx(
            row,
            "group hover:bg-surface-2 focus-visible:bg-surface-2 focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none focus-visible:ring-inset",
          )}
          onFocus={() => onHover(part.key)}
          onBlur={() => onHover(null)}
          {...hover}
        >
          {body}
        </Link>
      ) : (
        <div className={row} {...hover}>
          {body}
        </div>
      )}
    </li>
  );
}

/** What the cash is made of: all of it available, or how much is held for open orders and on hold. */
function cashDescription(balance: VaultBalance): string {
  const { available, held, onHold } = cashBreakdown(balance);
  const pieces = [`${formatMoney(available)} available`];
  if (!isZero(held)) pieces.push(`${formatMoney(held)} held for orders`);
  if (!isZero(onHold)) pieces.push(`${formatMoney(onHold)} on hold`);
  return pieces.length === 1 ? "Available to invest or withdraw" : pieces.join(" · ");
}

function EmptyParts() {
  return (
    <div
      className="grid min-h-28 place-items-center border border-dashed border-border-strong/60 px-4 text-center"
      data-testid="allocation-empty"
    >
      <div>
        <p className="text-sm font-medium text-ink">Nothing to allocate yet</p>
        <p className="mt-1 text-xs text-muted">Cash and investments appear here as your vault fills.</p>
      </div>
    </div>
  );
}

/** Placeholder with the same footprint, while the vault loads. */
export function PortfolioAnalyticsSkeleton({ className }: { className?: string }) {
  return (
    <div className={cx(CARD, className)} role="status">
      <span className="sr-only">Loading your allocation…</span>
      <div
        aria-hidden
        className="grid gap-6 motion-safe:animate-pulse xl:grid-cols-[minmax(0,1fr)_minmax(0,1.15fr)] xl:gap-10"
      >
        <div>
          <div className="flex items-center gap-3">
            <div className="size-10 bg-surface-2" />
            <div className="space-y-2">
              <div className="h-4 w-24 bg-surface-2" />
              <div className="h-3 w-40 max-w-full bg-surface-2" />
            </div>
          </div>
          <div className="mt-6 h-14 w-56 bg-surface-2" />
          <div className="mt-5 h-3 bg-surface-2" />
          <div className="mt-3 h-4 w-48 bg-surface-2" />
        </div>
        <div className="h-36 bg-surface-2" />
      </div>
    </div>
  );
}
