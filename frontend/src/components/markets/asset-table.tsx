"use client";

import type { MouseEvent, ReactNode } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { ArrowDownRight, ArrowUpRight, Minus, type LucideIcon } from "lucide-react";

import { cx } from "@/components/ui/primitives";
import { signedPercent, type Trend } from "@/lib/vault";

/*
 * What the vault's holdings (components/vault/asset-list.tsx) and the
 * Discover catalog (market-list.tsx) share: one terminal-style table on a
 * charcoal card, whose rows open their asset's page.
 *
 * - The name is the row's link (keyboard, assistive technology, a new tab);
 *   a click anywhere else on the row follows it too.
 * - Rows raise to the lighter charcoal on hover or keyboard focus, with a
 *   yellow edge and the ticker in yellow.
 * - A change carries its sign and an arrow as well as green or red.
 */

/** The card around a table: charcoal with a hairline border (square: the radius tokens are 0). */
export const TABLE_CARD = "rounded-2xl border border-border bg-surface";

/** A row that opens an asset's page. */
export const ROW = "group cursor-pointer transition-colors focus-within:bg-surface-2 hover:bg-surface-2";

/**
 * The row's first cell (`<th scope="row">`), with the yellow edge: invisible
 * at rest. It takes the width the other columns leave (`w-full max-w-0`), so a
 * long name truncates instead of widening the table past a phone's screen.
 */
export const ROW_HEADER = cx(
  "relative w-full max-w-0 px-5 py-3.5 text-left font-normal sm:px-6",
  "before:absolute before:inset-y-2 before:left-0 before:w-[3px] before:bg-synthetic-yellow",
  "before:opacity-0 before:transition-opacity group-focus-within:before:opacity-100 group-hover:before:opacity-100",
);

/**
 * A row's click handler: follows `href` from anywhere on the row, except the
 * link itself (whose click is its own, modifier keys included) and the end
 * of a text selection.
 */
export function useRowNavigation(href: string) {
  const router = useRouter();
  return (event: MouseEvent<HTMLTableRowElement>) => {
    if (event.target instanceof Element && event.target.closest("a")) return;
    if (window.getSelection()?.isCollapsed === false) return;
    router.push(href);
  };
}

/** The ticker in a mono chip; yellow while its row is hovered or focused. */
export function TickerChip({ ticker }: { ticker: string }) {
  return (
    <span
      className={cx(
        "inline-flex h-7 min-w-14 shrink-0 items-center justify-center rounded-md border border-border bg-bg px-2",
        "font-mono text-xs font-semibold tracking-wide text-ink transition-colors",
        "group-focus-within:border-synthetic-yellow/50 group-focus-within:text-synthetic-yellow",
        "group-hover:border-synthetic-yellow/50 group-hover:text-synthetic-yellow",
      )}
    >
      {ticker}
    </span>
  );
}

/** The row's link: the asset's name. */
export function RowLink({ href, children, testId }: { href: string; children: ReactNode; testId: string }) {
  return (
    <Link
      href={href}
      className={cx(
        "block truncate font-medium text-ink",
        "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2",
        "focus-visible:ring-offset-surface-2 focus-visible:outline-none",
      )}
      data-testid={testId}
    >
      {children}
    </Link>
  );
}

const TREND: Record<Trend, { Icon: LucideIcon; className: string }> = {
  up: { Icon: ArrowUpRight, className: "text-gain" },
  down: { Icon: ArrowDownRight, className: "text-loss" },
  flat: { Icon: Minus, className: "text-muted" },
};

/** A percent change ("0.84" is +0.84 %); `suffix` is read aloud after it ("today"). */
export function DailyChange({ pct, suffix, className }: { pct: string; suffix: string; className?: string }) {
  const change = signedPercent(pct);
  const trend = TREND[change.trend];
  return (
    <span
      className={cx("inline-flex items-center gap-1 text-xs font-medium tabular-nums", trend.className, className)}
      data-trend={change.trend}
    >
      <trend.Icon className="size-3.5" aria-hidden />
      {change.text}
      <span className="sr-only"> {suffix}</span>
    </span>
  );
}

/** Placeholder with a table card's footprint, for while it loads. */
export function AssetTableSkeleton({ label, className, rows = 3 }: { label: string; className?: string; rows?: number }) {
  return (
    <div className={cx(TABLE_CARD, className)} role="status">
      <span className="sr-only">{label}</span>
      <div aria-hidden className="motion-safe:animate-pulse">
        <div className="flex items-center gap-3 border-b border-border px-5 py-4 sm:px-6">
          <div className="size-10 rounded-xl bg-surface-2" />
          <div className="space-y-2">
            <div className="h-4 w-20 rounded-sm bg-surface-2" />
            <div className="h-3 w-28 rounded-sm bg-surface-2" />
          </div>
        </div>
        {Array.from({ length: rows }, (_, i) => (
          <div
            key={i}
            className="flex items-center justify-between gap-4 border-b border-border px-5 py-4 last:border-b-0 sm:px-6"
          >
            <div className="flex items-center gap-3">
              <div className="h-7 w-14 rounded-md bg-surface-2" />
              <div className="h-4 w-32 rounded-sm bg-surface-2" />
            </div>
            <div className="h-4 w-20 rounded-sm bg-surface-2" />
          </div>
        ))}
      </div>
    </div>
  );
}
