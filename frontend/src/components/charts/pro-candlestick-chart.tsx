"use client";

import { useId, useLayoutEffect, useMemo, useState, type KeyboardEvent, type PointerEvent } from "react";
import { ChartCandlestick, ChartLine, type LucideIcon } from "lucide-react";

import { cx } from "@/components/ui/primitives";
import {
  bucketEnd,
  candleChange,
  candleIndexAt,
  candleTime,
  describeCandle,
  timeTicks,
  type Candle,
  type PriceView,
} from "@/lib/candles";
import { formatMoney, toUnits } from "@/lib/decimal";
import { formatPrice } from "@/lib/market";
import {
  axisMoney,
  compactMoney,
  linePath,
  nearestIndex,
  niceTicks,
  rangeChange,
  readoutDate,
  scale,
} from "@/lib/portfolio";
import { signedPercent, type Trend } from "@/lib/vault";

/*
 * Pro Chart: an asset's price as a Synthetic Gold line or as candlesticks,
 * with a crosshair and a volume histogram, in the Sovereign Gold identity.
 *
 * - Two modes over the same window and the same axes (`priceView`,
 *   lib/candles.ts): switching between them only swaps the marks.
 *   - Line: the price, a 2 px gold line over a gold fade.
 *   - Candles: one per period of the range (30 minutes over a day, 4 hours
 *     over a week, a day over a month, a week over a year). A rising candle
 *     (close at or above its open) is drawn hollow in green, a falling one
 *     solid in red, so the direction reads without color too.
 * - The crosshair snaps to the nearest point or candle: a vertical hairline
 *   through the price and the volume, a horizontal one at its exact price,
 *   with that price tagged on the price axis and its date and time on the
 *   time axis. Above the plot, the legend reads the candle in focus (open,
 *   high, low, close, change, volume), the latest one otherwise; in line
 *   mode, the range's high, low and change.
 * - The volume (when known) is its own pane under the price, on the same
 *   time axis: one bar per candle, never a second scale on the price.
 * - The last price is tagged in gold on the price axis, with a dotted line
 *   across the plot.
 * - Drawn in the plot's own pixels (measured), so candles and hairlines stay
 *   crisp at any size. Pointer (mouse, pen, a finger dragging across) or
 *   keyboard (arrows, Home, End, Escape once focused) moves the focus; the
 *   keyboard's point is announced to screen readers.
 * - Prices arrive as exact decimal strings and are printed from them; only
 *   pixel positions and axis labels go through floating point.
 */

export type ChartMode = "line" | "candles";

export interface ProCandlestickChartProps {
  /** What the range shows (`priceView`); null without history. */
  view: PriceView | null;
  mode: ChartMode;
  /** Accessible name of the plot, with how to use it. */
  label: string;
  /** Index of the point (line: `view.line`) or the candle (`view.candles`) in focus; null for none. */
  active: number | null;
  onActiveChange: (index: number | null) => void;
  /** Shown in place of the chart without history. */
  emptyText: { title: string; body: string };
  /** data-testid of the line's path. */
  lineTestId?: string;
  className?: string;
}

/** Heights of the two panes: the price, and the volume under it. */
const PRICE_PANE = "h-60 sm:h-80";
const VOLUME_PANE = "h-12 sm:h-14";
/** Room above the highest price and below the lowest, in pixels. */
const TOP = 12;
const BOTTOM = 10;
/** A candle's body fills 70 % of its period, between 1 and 24 pixels. */
const BODY_SHARE = 0.7;
const BODY_MAX = 24;
/** A body this wide or wider can be drawn hollow. */
const HOLLOW_MIN = 4;
/** An axis tag's height, and a character's width in the axes' 10 px monospace, in pixels. */
const TAG_HEIGHT = 16;
const CHAR = 6;

interface CandleShape {
  candle: Candle;
  up: boolean;
  /** The wick's x (on a pixel's middle, for a crisp 1 px line). */
  x: number;
  /** The body's left edge and width, in whole pixels. */
  left: number;
  width: number;
  high: number;
  low: number;
  top: number;
  bottom: number;
}

export function ProCandlestickChart({
  view,
  mode,
  label,
  active,
  onActiveChange,
  emptyText,
  lineTestId,
  className,
}: ProCandlestickChartProps) {
  const gradientId = `pc-gradient-${useId().replace(/[^\w-]/g, "")}`;
  const [priceRef, price] = useSize({ width: 640, height: 320 });
  const [volumeRef, volumePane] = useSize({ width: 640, height: 56 });
  const [announcement, setAnnouncement] = useState("");
  const withVolume = view !== null && view.maxVolume !== null && toUnits(view.maxVolume) > 0n;

  const chart = useMemo(() => {
    if (!view) return null;
    const { width, height } = price;
    const x = scale({ min: view.from, max: view.to }, 0, width);
    const low = Number(view.low);
    const high = Number(view.high);
    const pad = high > low ? (high - low) * 0.06 : Math.max(Math.abs(low) * 0.01, 0.01);
    const domain = { min: Math.max(low - pad, 0), max: high + pad };
    const y = scale(domain, height - BOTTOM, TOP);
    // A flat price gets one label, its own level: round ticks around it would invent a scale.
    const ticks =
      high > low
        ? niceTicks(domain.min, domain.max, height < 280 ? 3 : 4).filter((t) => t >= domain.min && t <= domain.max)
        : [low];
    const labels = axisMoney(ticks);
    const times = timeTicks(view.from, view.to, Math.max(2, Math.min(8, Math.floor(width / 110))));

    const candles: CandleShape[] = view.candles.map((candle) => {
      const left = x(candle.start);
      const right = x(bucketEnd(candle.start, view.interval));
      // An odd width in whole pixels, so the 1 px wick sits exactly in the middle.
      let body = Math.max(1, Math.round(Math.min((right - left) * BODY_SHARE, BODY_MAX)));
      if (body % 2 === 0) body -= 1;
      const bodyLeft = Math.round((left + right) / 2 - body / 2);
      const open = Number(candle.open);
      const close = Number(candle.close);
      const top = Math.round(y(Math.max(open, close)));
      const bottom = Math.max(Math.round(y(Math.min(open, close))), top + 1);
      return {
        candle,
        up: toUnits(candle.close) >= toUnits(candle.open),
        x: bodyLeft + (body - 1) / 2 + 0.5,
        left: bodyLeft,
        width: body,
        high: Math.round(y(Number(candle.high))),
        low: Math.round(y(Number(candle.low))),
        top,
        bottom,
      };
    });

    const line = linePath(view.line, x, y, "linear");
    const first = view.line[0];
    const area = first ? `${line}V${height}H${x(first.at)}Z` : "";
    const last = view.line.at(-1)!;
    const change = rangeChange(view.line);
    // The price gutter fits its longest label or tag (monospace: `ch` is exact), with the tag's padding.
    const longest = Math.max(
      ...labels.map((l) => l.length),
      formatPrice(view.high).length,
      formatPrice(view.low).length,
    );
    return { x, y, ticks, labels, times, candles, line, area, last, change, gutter: `calc(${longest}ch + 1rem)` };
  }, [view, price]);

  const series = mode === "line" ? (view?.line ?? []) : (view?.candles ?? []);
  const focus = active !== null && active < series.length ? active : null;
  const focusPoint = mode === "line" && focus !== null ? view!.line[focus]! : null;
  const focusCandle = mode === "candles" && focus !== null ? view!.candles[focus]! : null;
  // The candle the legend reads, and whose volume bar lights up: the one in focus, or the forming one.
  const legendCandle = focusCandle ?? view?.candles.at(-1) ?? null;
  const volumeFocus =
    view === null
      ? -1
      : focusCandle
        ? focus!
        : focusPoint
          ? candleIndexAt(view.candles, view.interval, focusPoint.at)
          : -1;

  const crosshair = chart
    ? focusPoint
      ? {
          x: chart.x(focusPoint.at),
          y: chart.y(Number(focusPoint.value)),
          price: focusPoint.value,
          time: readoutDate(focusPoint.at),
        }
      : focusCandle
        ? {
            x: chart.candles[focus!]!.x,
            y: chart.y(Number(focusCandle.close)),
            price: focusCandle.close,
            time: candleTime(focusCandle, view!.interval),
          }
        : null
    : null;

  function pick(clientX: number, box: DOMRect): number | null {
    if (!view) return null;
    const fraction = Math.min(Math.max((clientX - box.left) / box.width, 0), 1);
    const at = view.from + fraction * (view.to - view.from);
    const index = mode === "line" ? nearestIndex(view.line, at) : candleIndexAt(view.candles, view.interval, at);
    return index < 0 ? null : index;
  }

  function onPointer(event: PointerEvent<HTMLDivElement>) {
    if (!view) return;
    onActiveChange(pick(event.clientX, event.currentTarget.getBoundingClientRect()));
  }

  function describe(index: number): string {
    if (!view) return "";
    if (mode === "candles") return describeCandle(view.candles[index]!, view.interval);
    const point = view.line[index]!;
    return `${formatMoney(point.value)} MXN, ${readoutDate(point.at)}`;
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (!view || series.length === 0) return;
    if (event.key === "Escape") {
      onActiveChange(null);
      setAnnouncement("");
      return;
    }
    const last = series.length - 1;
    const current = focus ?? last;
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
    setAnnouncement(describe(next));
  }

  const empty = !view || !chart;
  const lastY = chart ? chart.y(Number(chart.last.value)) : 0;
  // Tags win over labels: the gold last price gives way to the crosshair's when they meet, and
  // axis labels give way to either tag (labels are monospace: 6 px a character at 10 px).
  const showLast = chart !== null && (!crosshair || Math.abs(crosshair.y - lastY) >= TAG_HEIGHT);
  const tagRows = [...(showLast ? [lastY] : []), ...(crosshair ? [crosshair.y] : [])];
  const timeTag = crosshair ? extent(crosshair.x, crosshair.time.length * CHAR + 12, price.width) : null;

  return (
    <div className={className} data-testid="pro-chart" data-mode={mode} data-interval={view?.interval.id}>
      <p className="sr-only" aria-live="polite">
        {announcement}
      </p>

      <Legend view={view} mode={mode} candle={legendCandle} change={chart?.change ?? null} />

      <div className="mt-3 flex gap-2">
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
            !empty && "cursor-crosshair",
          )}
          data-testid="chart-plot"
        >
          <div ref={priceRef} className={cx("relative", PRICE_PANE)}>
            <svg
              width={price.width}
              height={price.height}
              viewBox={`0 0 ${price.width} ${price.height}`}
              className="absolute inset-0 block overflow-visible"
              aria-hidden
            >
              <defs>
                <linearGradient id={gradientId} x1="0" x2="0" y1="0" y2="1">
                  <stop offset="0%" stopColor="var(--color-synthetic-yellow)" stopOpacity="0.22" />
                  <stop offset="100%" stopColor="var(--color-synthetic-yellow)" stopOpacity="0" />
                </linearGradient>
              </defs>
              {chart ? (
                <>
                  {/* Hairline grid, solid and one step off the surface. */}
                  <g className="stroke-border" strokeWidth={1} shapeRendering="crispEdges">
                    {chart.ticks.map((tick) => (
                      <line
                        key={`y${tick}`}
                        x1={0}
                        x2={price.width}
                        y1={pixel(chart.y(tick))}
                        y2={pixel(chart.y(tick))}
                      />
                    ))}
                    {chart.times.map((tick) => (
                      <line
                        key={`x${tick.at}`}
                        x1={pixel(chart.x(tick.at))}
                        x2={pixel(chart.x(tick.at))}
                        y1={0}
                        y2={price.height}
                      />
                    ))}
                  </g>
                  {mode === "line" ? (
                    <>
                      <path d={chart.area} fill={`url(#${gradientId})`} />
                      <path
                        d={chart.line}
                        fill="none"
                        className="stroke-synthetic-yellow"
                        strokeWidth={2}
                        strokeLinejoin="round"
                        strokeLinecap="round"
                        data-testid={lineTestId}
                      />
                    </>
                  ) : (
                    <g data-testid="chart-candles">
                      {chart.candles.map((shape) => (
                        <CandleMark key={shape.candle.start} shape={shape} />
                      ))}
                    </g>
                  )}
                  {/* The last price, across the plot. */}
                  <line
                    x1={0}
                    x2={price.width}
                    y1={pixel(lastY)}
                    y2={pixel(lastY)}
                    className="stroke-synthetic-yellow/50"
                    strokeWidth={1}
                    strokeDasharray="1 3"
                    shapeRendering="crispEdges"
                  />
                </>
              ) : (
                <line
                  x1={0}
                  x2={price.width}
                  y1={price.height - BOTTOM}
                  y2={price.height - BOTTOM}
                  className="stroke-synthetic-yellow/50"
                  strokeDasharray="6 6"
                  strokeWidth={2}
                />
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

            {crosshair ? (
              <span
                aria-hidden
                className="pointer-events-none absolute inset-x-0 h-px bg-ink/35"
                style={{ top: Math.round(crosshair.y) }}
              />
            ) : null}

            {/* Line mode marks its point: the one in focus holds still, the latest breathes (it is live). */}
            {chart && mode === "line" ? (
              <span
                aria-hidden
                className="pointer-events-none absolute flex size-3 -translate-x-1/2 -translate-y-1/2"
                style={{
                  left: focusPoint ? chart.x(focusPoint.at) : chart.x(chart.last.at),
                  top: focusPoint ? chart.y(Number(focusPoint.value)) : lastY,
                }}
              >
                {focusPoint ? null : (
                  <span className="absolute inline-flex size-full rounded-full bg-synthetic-yellow opacity-50 motion-safe:animate-ping motion-safe:[animation-duration:2.4s]" />
                )}
                <span className="relative inline-flex size-3 rounded-full bg-synthetic-yellow ring-2 ring-surface" />
              </span>
            ) : null}
          </div>

          {withVolume ? (
            <div
              ref={volumeRef}
              className={cx("relative mt-1 border-t border-border", VOLUME_PANE)}
              data-testid="chart-volume"
            >
              <svg
                width={volumePane.width}
                height={volumePane.height}
                viewBox={`0 0 ${volumePane.width} ${volumePane.height}`}
                className="absolute inset-0 block"
                aria-hidden
              >
                {chart && view?.maxVolume
                  ? chart.candles.map((shape, i) => {
                      const share = Number(shape.candle.volume ?? 0) / Number(view.maxVolume);
                      const barHeight = Math.max(share > 0 ? 1 : 0, Math.round(share * (volumePane.height - 4)));
                      return (
                        <rect
                          key={shape.candle.start}
                          x={shape.left}
                          y={volumePane.height - barHeight}
                          width={shape.width}
                          height={barHeight}
                          className={i === volumeFocus ? "fill-zinc-400" : "fill-zinc-700"}
                          shapeRendering="crispEdges"
                        />
                      );
                    })
                  : null}
              </svg>
            </div>
          ) : null}

          {crosshair ? (
            <span
              aria-hidden
              className="pointer-events-none absolute inset-y-0 w-px bg-ink/35"
              style={{ left: Math.floor(crosshair.x) }}
              data-testid="chart-crosshair"
            />
          ) : null}
        </div>

        {/* Price axis: its own gutter, so labels never sit on the marks. */}
        <div
          aria-hidden
          className="relative shrink-0 font-mono text-[10px] text-muted tabular-nums"
          style={{ width: chart?.gutter ?? "3.5rem" }}
        >
          <div className={cx("relative", PRICE_PANE)}>
            {chart
              ? chart.ticks.map((tick, i) =>
                  tagRows.some((row) => Math.abs(row - chart.y(tick)) < TAG_HEIGHT - 2) ? null : (
                    <span key={tick} className="absolute left-1.5 -translate-y-1/2" style={{ top: chart.y(tick) }}>
                      {chart.labels[i]}
                    </span>
                  ),
                )
              : null}
            {chart && showLast ? (
              <Tag top={lastY} tone="gold" testId="chart-last-price">
                {formatPrice(chart.last.value)}
              </Tag>
            ) : null}
            {crosshair ? (
              <Tag top={crosshair.y} tone="ink" testId="chart-price-tag">
                {formatPrice(crosshair.price)}
              </Tag>
            ) : null}
          </div>
          {withVolume && view?.maxVolume ? (
            <div className={cx("relative mt-1", VOLUME_PANE)}>
              <span className="absolute top-1 left-1.5" title="Largest volume shown">
                {compactMoney(Number(view.maxVolume))}
              </span>
              <span className="absolute bottom-0 left-1.5 text-[9px] tracking-[0.14em] uppercase">Vol</span>
            </div>
          ) : null}
        </div>
      </div>

      {/* Time axis: round instants, and the one in focus tagged over them. */}
      <div
        aria-hidden
        className="relative mt-1.5 h-4 font-mono text-[10px] text-muted tabular-nums"
        // Ends where the plot ends: the gutter plus the gap-2 between them.
        style={{ marginRight: `calc(${chart?.gutter ?? "3.5rem"} + 0.5rem)` }}
      >
        {chart
          ? chart.times.map((tick) => {
              const x = chart.x(tick.at);
              if (timeTag && overlaps(extent(x, tick.label.length * CHAR, price.width), timeTag)) return null;
              return (
                <span key={tick.at} className="absolute top-0 whitespace-nowrap" style={anchor(x, price.width)}>
                  {tick.label}
                </span>
              );
            })
          : null}
        {crosshair ? (
          <span
            className="absolute top-0 z-10 bg-ink px-1.5 leading-4 whitespace-nowrap text-black"
            style={anchor(crosshair.x, price.width)}
            data-testid="chart-time-tag"
          >
            {crosshair.time}
          </span>
        ) : null}
      </div>
    </div>
  );
}

/* ------------------------------------------------------------------------ */

/** One candle: its wicks above and below the body, the body hollow when it rose (and is wide enough), solid when it fell. */
function CandleMark({ shape }: { shape: CandleShape }) {
  // A body too thin or too flat for an outline to show a hollow is drawn solid.
  const hollow = shape.up && shape.width >= HOLLOW_MIN && shape.bottom - shape.top >= 3;
  return (
    <g className={shape.up ? "text-gain" : "text-loss"} data-testid="candle" data-direction={shape.up ? "up" : "down"}>
      <line x1={shape.x} x2={shape.x} y1={shape.high} y2={shape.top} stroke="currentColor" strokeWidth={1} />
      <line x1={shape.x} x2={shape.x} y1={shape.bottom} y2={shape.low} stroke="currentColor" strokeWidth={1} />
      {hollow ? (
        <rect
          x={shape.left + 0.5}
          y={shape.top + 0.5}
          width={shape.width - 1}
          height={shape.bottom - shape.top - 1}
          fill="currentColor"
          fillOpacity={0.16}
          stroke="currentColor"
          strokeWidth={1}
        />
      ) : (
        <rect
          x={shape.left}
          y={shape.top}
          width={shape.width}
          height={shape.bottom - shape.top}
          fill="currentColor"
          shapeRendering="crispEdges"
        />
      )}
    </g>
  );
}

/** A price on the price axis: the last price in gold, the crosshair's in white. */
function Tag({ top, tone, testId, children }: { top: number; tone: "gold" | "ink"; testId: string; children: string }) {
  return (
    <span
      className={cx(
        "absolute left-0 -translate-y-1/2 px-1.5 leading-4 font-semibold whitespace-nowrap text-black",
        tone === "gold" ? "z-10 bg-synthetic-yellow" : "z-20 bg-ink",
      )}
      style={{ top }}
      data-testid={testId}
    >
      {children}
    </span>
  );
}

/** What the legend says: the candle in focus (or the forming one), or in line mode the range's extremes and change. */
function Legend({
  view,
  mode,
  candle,
  change,
}: {
  view: PriceView | null;
  mode: ChartMode;
  candle: Candle | null;
  change: ReturnType<typeof rangeChange>;
}) {
  // Two lines' worth on a phone, one from sm: the plot never jumps when the legend changes.
  const frame = "flex min-h-9 flex-wrap items-center gap-x-3 gap-y-1 font-mono text-[11px] tabular-nums sm:min-h-5";
  if (!view) return <div className={frame} />;
  if (mode === "candles" && candle) {
    const delta = candleChange(candle);
    const percent = delta.percent === null ? null : signedPercent(delta.percent);
    return (
      <div className={frame} data-testid="chart-legend">
        <span
          className="border border-border px-1.5 text-[10px] leading-4 font-semibold tracking-[0.12em] text-muted uppercase"
          title={`${view.interval.name} candles`}
        >
          {view.interval.label}
        </span>
        <dl className="contents">
          <Figure term="O" value={formatPrice(candle.open)} />
          <Figure term="H" value={formatPrice(candle.high)} />
          <Figure term="L" value={formatPrice(candle.low)} />
          <Figure term="C" value={formatPrice(candle.close)} />
          {percent ? <Figure term="Change" value={percent.text} trend={percent.trend} hideTerm /> : null}
          {candle.volume !== null ? <Figure term="Vol" value={compactMoney(Number(candle.volume))} /> : null}
        </dl>
      </div>
    );
  }
  const percent = change?.percent ? signedPercent(change.percent) : null;
  return (
    <div className={frame} data-testid="chart-legend">
      <dl className="contents">
        <Figure term="High" value={formatPrice(view.lineHigh)} />
        <Figure term="Low" value={formatPrice(view.lineLow)} />
        {percent ? <Figure term="Range" value={percent.text} trend={percent.trend} /> : null}
      </dl>
    </div>
  );
}

const TREND_TEXT: Record<Trend, string> = { up: "text-gain", down: "text-loss", flat: "text-muted" };

function Figure({
  term,
  value,
  trend,
  hideTerm = false,
}: {
  term: string;
  value: string;
  trend?: Trend;
  hideTerm?: boolean;
}) {
  return (
    <div className="inline-flex items-baseline gap-1">
      <dt className={hideTerm ? "sr-only" : "text-muted"}>{term}</dt>
      <dd className={cx("font-semibold", trend ? TREND_TEXT[trend] : "text-ink")}>{value}</dd>
    </div>
  );
}

/* ------------------------------------------------------------------------ */

/** The chart type: a square two-way toggle, the chosen one solid gold, like the range picker beside it. */
export function ChartModeToggle({ value, onChange }: { value: ChartMode; onChange: (mode: ChartMode) => void }) {
  const options: readonly { mode: ChartMode; label: string; Icon: LucideIcon }[] = [
    { mode: "line", label: "Line", Icon: ChartLine },
    { mode: "candles", label: "Candles", Icon: ChartCandlestick },
  ];
  return (
    <div
      role="group"
      aria-label="Chart type"
      className="flex border border-border bg-bg p-0.5"
      data-testid="chart-mode"
    >
      {options.map(({ mode, label, Icon }) => {
        const selected = mode === value;
        return (
          <button
            key={mode}
            type="button"
            aria-pressed={selected}
            onClick={() => onChange(mode)}
            className={cx(
              "inline-flex min-h-8 min-w-10 items-center justify-center gap-1.5 px-2.5 font-mono text-xs font-semibold transition-colors",
              "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none",
              selected ? "bg-synthetic-yellow text-black" : "text-muted hover:bg-surface-2 hover:text-ink",
            )}
            data-mode={mode}
          >
            <Icon className="size-4" aria-hidden />
            {/* The name stays for screen readers on a phone, where only the icon shows. */}
            <span className="sr-only sm:not-sr-only">{label}</span>
          </button>
        );
      })}
    </div>
  );
}

/* ------------------------------------------------------------------------ */

/** The middle of a pixel: where a 1 px line is crisp. */
function pixel(value: number): number {
  return Math.floor(value) + 0.5;
}

/** Centered on `x`, but kept inside [0, width] near the edges. */
function anchor(x: number, width: number): { left: number; transform: string } {
  if (x < 40) return { left: Math.max(x, 0), transform: "none" };
  if (x > width - 40) return { left: Math.min(x, width), transform: "translateX(-100%)" };
  return { left: x, transform: "translateX(-50%)" };
}

/** Where a label `size` pixels wide placed by `anchor` at `x` runs, left to right. */
function extent(x: number, size: number, width: number): [number, number] {
  if (x < 40) return [Math.max(x, 0), Math.max(x, 0) + size];
  if (x > width - 40) return [Math.min(x, width) - size, Math.min(x, width)];
  return [x - size / 2, x + size / 2];
}

/** Two runs closer than `margin` pixels. */
function overlaps(a: [number, number], b: [number, number], margin = 6): boolean {
  return a[0] < b[1] + margin && b[0] < a[1] + margin;
}

/**
 * A callback ref and the size of the element it lands on, in pixels: measured
 * before paint when it mounts and again on every resize; `fallback` until then.
 */
function useSize(fallback: { width: number; height: number }) {
  const [element, setElement] = useState<HTMLElement | null>(null);
  const [size, setSize] = useState(fallback);
  useLayoutEffect(() => {
    if (!element) return;
    const measure = () => {
      const { width, height } = element.getBoundingClientRect();
      setSize((current) => (current.width === width && current.height === height ? current : { width, height }));
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [element]);
  return [setElement, size] as const;
}
