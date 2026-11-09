"use client";

import { useId, useMemo, useState, type KeyboardEvent, type PointerEvent } from "react";

import { cx } from "@/components/ui/primitives";
import {
  axisDate,
  axisMoney,
  linePath,
  nearestIndex,
  niceTicks,
  scale,
  timeDomain,
  valueDomain,
  type BalancePoint,
} from "@/lib/portfolio";

/*
 * Trend Chart: the Sovereign Gold line chart the vault's balance and every
 * asset's price are drawn with. Only the plot: the card around it, the title
 * and the readout belong to the caller, which owns the point being read.
 *
 * - The line is Synthetic Gold over a gold fade, on dashed hairlines.
 * - The SVG stretches to its container; strokes keep their width
 *   (non-scaling-stroke), and the marker and axis labels are HTML on top, so
 *   nothing distorts at any size.
 * - Pointer (mouse, pen, a finger dragging across) or keyboard (arrows, Home,
 *   End, Escape once focused) chooses the point being read; the keyboard's
 *   point is announced to screen readers.
 * - Values arrive as exact decimal strings; only pixel positions and the axis
 *   labels go through floating point.
 */

/** Drawing size in SVG units. */
const W = 600;
const H = 200;
/** Headroom above the highest value and room above the bottom edge, in SVG units. */
const TOP = 14;
const BOTTOM = 6;

export interface TrendChartProps {
  /** What to draw, oldest first: `drawable(pointsInRange(...))`. */
  points: readonly BalancePoint[];
  /** "step" holds each value until the next point (a balance); "linear" joins points (a price). */
  curve?: "step" | "linear";
  /** Accessible name of the plot, with how to use it. */
  label: string;
  /** Index (into `points`) of the point being read; null marks the latest. */
  active: number | null;
  onActiveChange: (index: number | null) => void;
  /** A point in words, announced as the keyboard reaches it. */
  describe: (point: BalancePoint) => string;
  /** Shown in place of the line when there are no points. */
  emptyText: { title: string; body: string };
  /** Height of the plot. */
  heightClassName?: string;
  /** data-testid of the line's path. */
  lineTestId?: string;
  className?: string;
}

export function TrendChart({
  points,
  curve = "step",
  label,
  active,
  onActiveChange,
  describe,
  emptyText,
  heightClassName = "h-44 sm:h-52",
  lineTestId,
  className,
}: TrendChartProps) {
  const gradientId = `tc-gradient-${useId().replace(/[^\w-]/g, "")}`;
  const [announcement, setAnnouncement] = useState("");
  const empty = points.length === 0;

  const chart = useMemo(() => {
    const time = timeDomain(points);
    const values = valueDomain(points);
    const x = scale(time, 0, W);
    const y = scale(values, H - BOTTOM, TOP);
    const line = linePath(points, x, y, curve);
    const first = points[0];
    const area = first ? `${line}V${H}H${x(first.at)}Z` : "";
    // A flat line gets one label, its own level: round ticks around it would invent a scale.
    const level =
      first && points.every((p) => Number(p.value) === Number(first.value)) ? Number(first.value) : null;
    const ticks =
      level !== null ? [level] : niceTicks(values.min, values.max, 3).filter((t) => t >= values.min && t <= values.max);
    const labels = axisMoney(ticks);
    // The value gutter fits its longest label (the labels are monospace, so `ch` is exact), 3rem at least.
    const gutter = `max(3rem, ${Math.max(0, ...labels.map((l) => l.length))}ch)`;
    return { time, x, y, line, area, ticks, labels, gutter };
  }, [points, curve]);

  function onPointer(event: PointerEvent<HTMLDivElement>) {
    if (empty) return;
    const box = event.currentTarget.getBoundingClientRect();
    const fraction = Math.min(Math.max((event.clientX - box.left) / box.width, 0), 1);
    onActiveChange(nearestIndex(points, chart.time.min + fraction * (chart.time.max - chart.time.min)));
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (empty) return;
    if (event.key === "Escape") {
      onActiveChange(null);
      setAnnouncement("");
      return;
    }
    const last = points.length - 1;
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
    if (next === null) return;
    event.preventDefault();
    onActiveChange(next);
    setAnnouncement(describe(points[next]!));
  }

  const focus = active !== null ? (points[active] ?? null) : null;
  const marker = focus ?? points.at(-1) ?? null;
  const markerLeft = marker ? (chart.x(marker.at) / W) * 100 : 0;
  const markerTop = marker ? (chart.y(Number(marker.value)) / H) * 100 : 0;
  const span = chart.time.max - chart.time.min;
  const first = points[0];
  const last = points.at(-1);
  const from = first ? axisDate(first.at, span) : "";
  const to = last ? axisDate(last.at, span) : "";

  return (
    <div className={className}>
      <p className="sr-only" aria-live="polite">
        {announcement}
      </p>
      <div className="flex gap-2">
        <div
          role="group"
          aria-label={label}
          tabIndex={empty ? -1 : 0}
          onPointerDown={onPointer}
          onPointerMove={onPointer}
          onPointerLeave={() => onActiveChange(null)}
          onKeyDown={onKeyDown}
          onBlur={() => {
            onActiveChange(null);
            setAnnouncement("");
          }}
          className={cx(
            "relative min-w-0 flex-1 touch-pan-y outline-none select-none",
            "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-4 focus-visible:ring-offset-surface",
            heightClassName,
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
                  data-testid={lineTestId}
                />
              </>
            )}
          </svg>

          {empty ? (
            <div className="absolute inset-0 grid place-items-center text-center">
              <div>
                <p className="text-sm font-medium text-ink">{emptyText.title}</p>
                <p className="mt-1 text-xs text-muted">{emptyText.body}</p>
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
        <div
          aria-hidden
          className="relative shrink-0 font-mono text-[10px] text-muted tabular-nums"
          style={{ width: chart.gutter }}
        >
          {empty
            ? null
            : chart.ticks.map((tick, i) => (
                <span key={tick} className="absolute right-0 -translate-y-1/2" style={{ top: `${(chart.y(tick) / H) * 100}%` }}>
                  {chart.labels[i]}
                </span>
              ))}
        </div>
      </div>

      {/*
        Time axis: the first and last instant shown. A lone balance (drawn flat
        from a twin 1 ms earlier) would read the same at both ends: it gets one
        label, on the right, where its marker is.
      */}
      <div
        aria-hidden
        className="mt-2 flex justify-between font-mono text-[10px] text-muted tabular-nums"
        // Ends where the plot ends: the gutter plus the gap-2 between them.
        style={{ marginRight: `calc(${chart.gutter} + 0.5rem)` }}
      >
        {empty || !to ? (
          <span>&nbsp;</span>
        ) : (
          <>
            <span>{from === to ? "" : from}</span>
            <span>{to}</span>
          </>
        )}
      </div>
    </div>
  );
}
