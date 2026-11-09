"use client";

import { useId, useMemo, useState, type KeyboardEvent, type PointerEvent } from "react";
import { ArrowDownRight, ArrowUpRight, ChartLine, Minus, type LucideIcon } from "lucide-react";

import { cx } from "@/components/ui/primitives";
import { formatMoney } from "@/lib/decimal";
import {
  CHART_RANGES,
  axisDate,
  compactMoney,
  linePath,
  nearestIndex,
  niceTicks,
  pointsInRange,
  rangeChange,
  readoutDate,
  scale,
  timeDomain,
  valueDomain,
  type BalancePoint,
  type ChartRangeId,
} from "@/lib/portfolio";
import { signedPercent, type Trend } from "@/lib/vault";

/*
 * Portfolio Chart: the trend of the client's balance, drawn in Synthetic
 * Capital Yellow on a charcoal card ("Sovereign Gold", globals.css).
 *
 * - The SVG stretches to its container; strokes keep their width
 *   (non-scaling-stroke), and the markers and axis labels are HTML on top,
 *   so nothing distorts at any size.
 * - By default a balance is drawn as steps: it holds its value until the
 *   next movement, which is what the ledger says happened.
 * - Pointer (mouse, pen, a finger dragging across) or keyboard (arrows, Home,
 *   End, Escape once focused) moves a readout: the header shows the balance at
 *   that point instead of the latest one.
 * - Amounts come in as exact decimal strings and are shown exactly; only the
 *   pixel positions and the axis labels go through floating point.
 */

/** Drawing size in SVG units. */
const W = 600;
const H = 200;
/** Headroom above the highest value and room above the bottom edge, in SVG units. */
const TOP = 14;
const BOTTOM = 6;

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
  const gradientId = `pc-gradient-${useId().replace(/[^\w-]/g, "")}`;
  const [range, setRange] = useState<ChartRangeId>(defaultRange);
  const [active, setActive] = useState<number | null>(null);
  const [announcement, setAnnouncement] = useState("");

  const shown = useMemo(() => {
    const inRange = pointsInRange(points, range, now);
    // A single balance (no movements yet) is drawn flat across the plot.
    return inRange.length === 1 ? [{ ...inRange[0]!, at: inRange[0]!.at - 1 }, inRange[0]!] : inRange;
  }, [points, range, now]);

  const chart = useMemo(() => {
    const time = timeDomain(shown);
    const values = valueDomain(shown);
    const x = scale(time, 0, W);
    const y = scale(values, H - BOTTOM, TOP);
    const line = linePath(shown, x, y, curve);
    const first = shown[0];
    const area = first ? `${line}V${H}H${x(first.at)}Z` : "";
    // A flat line gets one label, its own level: round ticks around it would invent a scale.
    const level = shown[0] && shown.every((p) => Number(p.value) === Number(shown[0]!.value)) ? Number(shown[0].value) : null;
    const ticks =
      level !== null ? [level] : niceTicks(values.min, values.max, 3).filter((t) => t >= values.min && t <= values.max);
    return { time, x, y, line, area, ticks };
  }, [shown, curve]);

  const rangeInfo = CHART_RANGES.find((r) => r.id === range)!;
  const latest = shown.at(-1) ?? null;
  const focus = active !== null ? (shown[active] ?? null) : null;
  const readout = focus ?? latest;
  const change = rangeChange(shown);
  const empty = points.length === 0;
  // The balance is known but nothing has moved it yet: a flat line, no change to report.
  const noMovements = points.length === 1;

  function pointAt(clientX: number, box: DOMRect): number {
    const fraction = Math.min(Math.max((clientX - box.left) / box.width, 0), 1);
    return chart.time.min + fraction * (chart.time.max - chart.time.min);
  }

  function onPointer(event: PointerEvent<HTMLDivElement>) {
    if (empty) return;
    setActive(nearestIndex(shown, pointAt(event.clientX, event.currentTarget.getBoundingClientRect())));
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (empty) return;
    const last = shown.length - 1;
    const current = active ?? last;
    const next =
      event.key === "ArrowLeft"
        ? Math.max(current - 1, 0)
        : event.key === "ArrowRight"
          ? Math.min(current + 1, last)
          : event.key === "Home"
            ? 0
            : event.key === "End"
              ? last
              : null;
    if (event.key === "Escape") {
      setActive(null);
      setAnnouncement("");
      return;
    }
    if (next === null) return;
    event.preventDefault();
    setActive(next);
    const point = shown[next]!;
    setAnnouncement(`${formatMoney(point.value)} MXN, ${readoutDate(point.at)}`);
  }

  const marker = focus ?? latest;
  const markerLeft = marker ? (chart.x(marker.at) / W) * 100 : 0;
  const markerTop = marker ? (chart.y(Number(marker.value)) / H) * 100 : 0;
  const span = chart.time.max - chart.time.min;

  return (
    <section
      aria-labelledby={titleId}
      className={cx("rounded-2xl border border-border bg-surface p-5 sm:p-6", className)}
      data-testid="portfolio-chart"
    >
      <header className="flex flex-wrap items-start justify-between gap-x-4 gap-y-3">
        <div className="flex min-w-0 items-start gap-3">
          <span
            aria-hidden
            className="grid size-10 shrink-0 place-items-center rounded-xl bg-synthetic-yellow/10 text-synthetic-yellow ring-1 ring-synthetic-yellow/25"
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
      {/* Screen readers: a summary of the range, and the point the keyboard cursor reaches. */}
      <p className="sr-only">
        {empty || !latest || !shown[0]
          ? "No balance history yet."
          : `${title} over ${rangeInfo.name}: ${formatMoney(shown[0].value)} MXN on ${readoutDate(shown[0].at)}, ` +
            `${formatMoney(latest.value)} MXN on ${readoutDate(latest.at)}.`}
      </p>
      <p className="sr-only" aria-live="polite">
        {announcement}
      </p>

      <div className="mt-4 flex gap-2">
        <div
          role="group"
          aria-label={`${title} chart. Use the arrow keys to read each point.`}
          tabIndex={empty ? -1 : 0}
          onPointerDown={onPointer}
          onPointerMove={onPointer}
          onPointerLeave={() => setActive(null)}
          onKeyDown={onKeyDown}
          onBlur={() => {
            setActive(null);
            setAnnouncement("");
          }}
          className={cx(
            "relative h-44 min-w-0 flex-1 touch-pan-y rounded-lg outline-none select-none sm:h-52",
            "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-4 focus-visible:ring-offset-surface",
            !empty && "cursor-crosshair",
          )}
        >
          <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" className="size-full overflow-visible" aria-hidden>
            <defs>
              <linearGradient id={gradientId} x1="0" x2="0" y1="0" y2="1">
                <stop offset="0%" stopColor="var(--color-synthetic-yellow)" stopOpacity="0.28" />
                <stop offset="100%" stopColor="var(--color-synthetic-yellow)" stopOpacity="0" />
              </linearGradient>
            </defs>
            {chart.ticks.map((tick) => (
              <line
                key={tick}
                x1={0}
                x2={W}
                y1={chart.y(tick)}
                y2={chart.y(tick)}
                stroke="var(--border)"
                strokeDasharray="2 5"
                vectorEffect="non-scaling-stroke"
              />
            ))}
            {empty ? (
              <line
                x1={0}
                x2={W}
                y1={H - BOTTOM}
                y2={H - BOTTOM}
                stroke="var(--color-synthetic-yellow)"
                strokeOpacity="0.5"
                strokeDasharray="6 6"
                strokeWidth={2}
                vectorEffect="non-scaling-stroke"
              />
            ) : (
              <>
                <path d={chart.area} fill={`url(#${gradientId})`} />
                <path
                  d={chart.line}
                  fill="none"
                  stroke="var(--color-synthetic-yellow)"
                  strokeWidth={2}
                  strokeLinejoin="round"
                  strokeLinecap="round"
                  vectorEffect="non-scaling-stroke"
                  data-testid="portfolio-line"
                />
              </>
            )}
          </svg>

          {empty ? (
            <div className="absolute inset-0 grid place-items-center text-center">
              <div>
                <p className="text-sm font-medium text-ink">No balance history yet</p>
                <p className="mt-1 text-xs text-muted">Your first deposit starts the line.</p>
              </div>
            </div>
          ) : null}

          {marker && !empty ? (
            <>
              {focus ? (
                <span
                  aria-hidden
                  className="pointer-events-none absolute inset-y-0 w-px -translate-x-1/2 bg-synthetic-yellow/45"
                  style={{ left: `${markerLeft}%` }}
                />
              ) : null}
              <span
                aria-hidden
                className="pointer-events-none absolute flex size-3 -translate-x-1/2 -translate-y-1/2"
                style={{ left: `${markerLeft}%`, top: `${markerTop}%` }}
              >
                {/* The latest point breathes (it is live); a point being read holds still. */}
                {focus ? null : (
                  <span className="absolute inline-flex size-full rounded-full bg-synthetic-yellow opacity-50 motion-safe:animate-ping motion-safe:[animation-duration:2.4s]" />
                )}
                <span className="relative inline-flex size-3 rounded-full bg-synthetic-yellow ring-2 ring-bg" />
              </span>
            </>
          ) : null}
        </div>

        {/* Value axis: its own gutter, so labels never sit on the line. */}
        <div aria-hidden className="relative w-12 shrink-0 font-mono text-[10px] text-muted tabular-nums">
          {empty
            ? null
            : chart.ticks.map((tick) => (
                <span
                  key={tick}
                  className="absolute right-0 -translate-y-1/2"
                  style={{ top: `${(chart.y(tick) / H) * 100}%` }}
                >
                  {compactMoney(tick)}
                </span>
              ))}
        </div>
      </div>

      {/* Time axis: the first and last instant shown. */}
      <div aria-hidden className="mt-2 mr-14 flex justify-between font-mono text-[10px] text-muted tabular-nums">
        {empty || !shown[0] || !latest ? (
          <span>&nbsp;</span>
        ) : (
          <>
            <span>{axisDate(shown[0].at, span)}</span>
            <span>{axisDate(latest.at, span)}</span>
          </>
        )}
      </div>
    </section>
  );
}

/* ------------------------------------------------------------------------ */

function RangePicker({ value, onChange }: { value: ChartRangeId; onChange: (id: ChartRangeId) => void }) {
  return (
    <div role="group" aria-label="Time range" className="flex rounded-lg border border-border bg-bg p-0.5">
      {CHART_RANGES.map((range) => {
        const selected = range.id === value;
        return (
          <button
            key={range.id}
            type="button"
            aria-pressed={selected}
            aria-label={`Show ${range.name}`}
            onClick={() => onChange(range.id)}
            className={cx(
              "min-h-8 min-w-10 rounded-md px-2.5 font-mono text-xs font-semibold transition-colors",
              "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none",
              selected ? "bg-synthetic-yellow text-black" : "text-muted hover:bg-surface-2 hover:text-ink",
            )}
          >
            {range.label}
          </button>
        );
      })}
    </div>
  );
}

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
