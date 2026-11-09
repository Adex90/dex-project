"use client";

import { useId, useMemo, useState } from "react";
import { ArrowDownRight, ArrowUpRight, ChartLine, Minus, type LucideIcon } from "lucide-react";

import { RangePicker } from "@/components/charts/range-picker";
import { TrendChart } from "@/components/charts/trend-chart";
import { cx } from "@/components/ui/primitives";
import { formatMoney } from "@/lib/decimal";
import {
  CHART_RANGES,
  drawable,
  pointsInRange,
  rangeChange,
  readoutDate,
  type BalancePoint,
  type ChartRangeId,
} from "@/lib/portfolio";
import { signedPercent, type Trend } from "@/lib/vault";

/*
 * Portfolio Chart: the trend of the client's balance, drawn in Synthetic
 * Gold on a charcoal card ("Sovereign Gold", globals.css). The plot is the
 * shared TrendChart (components/charts); this card adds the title, the time
 * ranges and the readout.
 *
 * - By default a balance is drawn as steps: it holds its value until the
 *   next movement, which is what the ledger says happened.
 * - The readout shows the latest balance and its change over the range, or
 *   the balance at the point under the pointer or the keyboard cursor.
 * - Amounts come in as exact decimal strings and are shown exactly.
 */

/** The ranges a balance is read over: a single day of a cash balance says little. */
const BALANCE_RANGES: readonly ChartRangeId[] = ["1W", "1M", "3M", "1Y", "ALL"];

export interface PortfolioChartProps {
  /** Balance points, oldest first (`cashBalanceHistory` in lib/portfolio). */
  points: readonly BalancePoint[];
  /** When the balance was read (epoch ms): the right edge of every range. */
  now: number;
  title?: string;
  /** One line under the title: what the line measures. */
  caption?: string;
  curve?: "step" | "linear";
  defaultRange?: ChartRangeId;
  className?: string;
}

export function PortfolioChart({
  points,
  now,
  title = "Balance",
  caption,
  curve = "step",
  defaultRange = "1M",
  className,
}: PortfolioChartProps) {
  const titleId = useId();
  const [range, setRange] = useState<ChartRangeId>(defaultRange);
  const [active, setActive] = useState<number | null>(null);

  const shown = useMemo(() => drawable(pointsInRange(points, range, now)), [points, range, now]);
  const rangeInfo = CHART_RANGES.find((r) => r.id === range)!;
  const latest = shown.at(-1) ?? null;
  const focus = active !== null ? (shown[active] ?? null) : null;
  const readout = focus ?? latest;
  const change = rangeChange(shown);
  const empty = points.length === 0;
  // The balance is known but nothing has moved it yet: a flat line, no change to report.
  const noMovements = points.length === 1;

  return (
    <section
      aria-labelledby={titleId}
      className={cx("border border-border bg-surface p-5 sm:p-6", className)}
      data-testid="portfolio-chart"
    >
      <header className="flex flex-wrap items-start justify-between gap-x-4 gap-y-3">
        <div className="flex min-w-0 items-start gap-3">
          <span
            aria-hidden
            className="grid size-10 shrink-0 place-items-center bg-synthetic-yellow/10 text-synthetic-yellow ring-1 ring-synthetic-yellow/25"
          >
            <ChartLine className="size-[18px]" />
          </span>
          <div className="min-w-0">
            <h2 id={titleId} className="text-base font-semibold tracking-tight">
              {title}
            </h2>
            {caption ? <p className="mt-0.5 text-sm text-muted">{caption}</p> : null}
          </div>
        </div>
        <RangePicker
          ranges={BALANCE_RANGES}
          value={range}
          onChange={(id) => {
            setRange(id);
            setActive(null);
          }}
        />
      </header>

      {/* The readout: the latest balance, or the one under the pointer or the keyboard cursor. */}
      <div className="mt-5 min-h-[3.75rem]" aria-hidden>
        {readout ? (
          <>
            <p className="text-2xl font-medium tracking-tight text-ink tabular-nums sm:text-3xl">
              {formatMoney(readout.value)}
              <span className="ml-2 text-xs font-medium tracking-[0.12em] text-muted">MXN</span>
            </p>
            {focus ? (
              <p className="mt-1 text-sm text-muted tabular-nums">{readoutDate(focus.at)}</p>
            ) : noMovements ? (
              <p className="mt-1 text-sm text-muted">No movements yet</p>
            ) : change ? (
              <ChangeLine amount={change.amount} percent={change.percent} rangeName={rangeInfo.name} />
            ) : null}
          </>
        ) : null}
      </div>
      {/* Screen readers: a summary of the range. */}
      <p className="sr-only">
        {empty || !latest || !shown[0]
          ? "No balance history yet."
          : `${title} over ${rangeInfo.name}: ${formatMoney(shown[0].value)} MXN on ${readoutDate(shown[0].at)}, ` +
            `${formatMoney(latest.value)} MXN on ${readoutDate(latest.at)}.`}
      </p>

      <TrendChart
        className="mt-4"
        points={shown}
        curve={curve}
        label={`${title} chart. Use the arrow keys to read each point.`}
        active={active}
        onActiveChange={setActive}
        describe={(point) => `${formatMoney(point.value)} MXN, ${readoutDate(point.at)}`}
        emptyText={{ title: "No balance history yet", body: "Your first deposit starts the line." }}
        lineTestId="portfolio-line"
      />
    </section>
  );
}

/* ------------------------------------------------------------------------ */

const TREND: Record<Trend, { Icon: LucideIcon; className: string }> = {
  up: { Icon: ArrowUpRight, className: "text-gain" },
  down: { Icon: ArrowDownRight, className: "text-loss" },
  flat: { Icon: Minus, className: "text-muted" },
};

/** "+$1,250.00 (+5.26%) over the last month": sign, arrow and color agree. */
function ChangeLine({ amount, percent, rangeName }: { amount: string; percent: string | null; rangeName: string }) {
  // The trend follows the amount as displayed (to the cent), like the percentage's sign.
  const trend = signedPercent(amount).trend;
  const style = TREND[trend];
  const money = formatMoney(amount);
  const signed = trend === "up" ? `+${money}` : trend === "down" ? `−${money.slice(1)}` : money;
  return (
    <p className={cx("mt-1 inline-flex flex-wrap items-center gap-x-1.5 text-sm font-medium tabular-nums", style.className)}>
      <style.Icon className="size-4" aria-hidden />
      <span>{signed}</span>
      {percent !== null ? <span>({signedPercent(percent).text})</span> : null}
      <span className="font-normal text-muted">over {rangeName}</span>
    </p>
  );
}
