"use client";

import { useEffect, useMemo, useRef, useState } from "react";
import type { CandlestickData, IChartApi, ISeriesApi, MouseEventParams, Time, UTCTimestamp } from "lightweight-charts";
import { IconCandles, IconLine } from "@/components/ui/icons";
import { Loading, Segmented, Skeleton } from "@/components/ui/primitives";
import { changeOver, toCandles, type Candle, type PriceSeries } from "./priceHistory";

type Kind = "candles" | "line";
const TIMEFRAMES = [
  { value: "1m", label: "1m", sec: 60 },
  { value: "15m", label: "15m", sec: 900 },
  { value: "1h", label: "1H", sec: 3_600 },
  { value: "4h", label: "4H", sec: 14_400 },
  { value: "1d", label: "1D", sec: 86_400 },
] as const;
type Tf = (typeof TIMEFRAMES)[number]["value"];

// Midnight-Berry chart theme.
const BG = "#080214";
const UP = "#00ff88";
const DOWN = "#ff1a53";
const FLAT = "rgba(185, 163, 217, 0.45)"; // a bucket where the pool price did not move
const GRID = "rgba(255, 42, 133, 0.07)";
const AXIS = "rgba(185, 163, 217, 0.15)";
const fmtPrice = (p: number) => (p >= 1000 ? p.toFixed(2) : p.toFixed(6));
const VISIBLE_BARS = 60;

type Lib = typeof import("lightweight-charts");

/**
 * TradingView-style chart of the pool price (lightweight-charts, Apache-2.0, attribution kept on the chart and below
 * it). Candles or line, five timeframes, OHLC legend under the crosshair, live updates without resetting the user's
 * zoom. For markets without a feed it renders an explicit empty state instead of a chart.
 */
export function MarketChart({ series, market, pendingName }: { series: PriceSeries; market: string; pendingName?: string }) {
  const box = useRef<HTMLDivElement>(null);
  const lib = useRef<Lib | undefined>(undefined);
  const chart = useRef<IChartApi | undefined>(undefined);
  const main = useRef<ISeriesApi<"Candlestick"> | ISeriesApi<"Area"> | undefined>(undefined);
  const shown = useRef<{ key: string; first: number; count: number } | undefined>(undefined);
  const [ready, setReady] = useState(false);
  const [kind, setKind] = useState<Kind>("candles");
  const [tf, setTfState] = useState<Tf>("15m");
  const [tfChosen, setTfChosen] = useState(false);
  const [hover, setHover] = useState<Candle | undefined>();
  const isPool = market === "pool";
  const setTf = (v: Tf) => {
    setTfChosen(true);
    setTfState(v);
  };

  // Until the user picks one, use the largest timeframe that still gives ~30 bars over the history we actually have.
  const firstTime = series.points.at(0)?.time;
  const lastTime = series.points.at(-1)?.time;
  useEffect(() => {
    if (tfChosen || firstTime === undefined || lastTime === undefined) return;
    const span = Math.max(lastTime, Math.floor(Date.now() / 1000)) - firstTime;
    const auto = [...TIMEFRAMES].reverse().find((x) => span / x.sec >= 30) ?? TIMEFRAMES[0];
    setTfState(auto.value);
  }, [tfChosen, firstTime, lastTime]);

  const bucket = TIMEFRAMES.find((x) => x.value === tf)?.sec ?? 900;
  const candles = useMemo(() => toCandles(series.points, bucket, Math.floor(Date.now() / 1000)), [series.points, bucket]);
  const lastPoint = series.points.at(-1);
  const change = useMemo(() => changeOver(series.points, 86_400, Math.floor(Date.now() / 1000)), [series.points]);

  // 1. create the chart once (client only; the library is code-split and never runs during prerender)
  useEffect(() => {
    if (!isPool) return;
    let disposed = false;
    void import("lightweight-charts").then((lc) => {
      if (disposed || !box.current) return;
      lib.current = lc;
      chart.current = lc.createChart(box.current, {
        autoSize: true,
        layout: {
          background: { type: lc.ColorType.Solid, color: BG },
          textColor: "#b9a3d9",
          fontFamily: "'Outfit Variable', system-ui, sans-serif",
          fontSize: 11,
          attributionLogo: true, // the link to tradingview.com the library's licence asks for
        },
        grid: { vertLines: { color: GRID }, horzLines: { color: GRID } },
        crosshair: {
          mode: lc.CrosshairMode.Normal,
          vertLine: { color: "rgba(255, 42, 133, 0.55)", labelBackgroundColor: "#c8006a" },
          horzLine: { color: "rgba(255, 42, 133, 0.55)", labelBackgroundColor: "#c8006a" },
        },
        rightPriceScale: { borderColor: AXIS },
        timeScale: { borderColor: AXIS, timeVisible: true, secondsVisible: false },
        localization: { priceFormatter: fmtPrice },
      });
      setReady(true);
    });
    return () => {
      disposed = true;
      chart.current?.remove();
      chart.current = undefined;
      main.current = undefined;
      shown.current = undefined;
      setReady(false);
    };
  }, [isPool]);

  // 2. (re)create the series when the chart type changes
  useEffect(() => {
    const c = chart.current;
    const lc = lib.current;
    if (!ready || !c || !lc) return;
    if (main.current) c.removeSeries(main.current);
    shown.current = undefined;
    main.current =
      kind === "candles"
        ? c.addSeries(lc.CandlestickSeries, {
            upColor: UP,
            downColor: DOWN,
            borderUpColor: UP,
            borderDownColor: DOWN,
            wickUpColor: UP,
            wickDownColor: DOWN,
            priceLineColor: "#ff2a85",
            priceFormat: { type: "price", precision: 6, minMove: 0.000001 },
          })
        : c.addSeries(lc.AreaSeries, {
            lineColor: "#ff2a85",
            topColor: "rgba(255, 42, 133, 0.35)",
            bottomColor: "rgba(255, 42, 133, 0.02)",
            lineWidth: 2,
            priceLineColor: "#ff2a85",
            priceFormat: { type: "price", precision: 6, minMove: 0.000001 },
          });
  }, [ready, kind]);

  // 3. data: full reload on a new timeframe/type/history, in-place update for live ticks (keeps the user's zoom)
  useEffect(() => {
    const s = main.current;
    const c = chart.current;
    if (!ready || !s || !c || candles.length === 0) return;
    const bar = (k: Candle) =>
      kind === "candles"
        ? ({ time: k.time as UTCTimestamp, open: k.open, high: k.high, low: k.low, close: k.close, ...(k.moved ? {} : { color: FLAT, borderColor: FLAT, wickColor: FLAT }) } satisfies CandlestickData<Time>)
        : { time: k.time as UTCTimestamp, value: k.close };
    const key = `${kind}|${tf}`;
    const first = candles[0]!.time;
    const prev = shown.current;
    if (prev && prev.key === key && prev.first === first && candles.length >= prev.count && candles.length <= prev.count + 1) {
      for (const k of candles.slice(prev.count - 1)) (s as ISeriesApi<"Candlestick">).update(bar(k) as CandlestickData<Time>);
    } else {
      (s as ISeriesApi<"Candlestick">).setData(candles.map(bar) as CandlestickData<Time>[]);
      // Show the newest ~60 bars; a young market keeps empty space on the left instead of a few stretched candles.
      const n = candles.length;
      c.timeScale().setVisibleLogicalRange({ from: n - VISIBLE_BARS, to: n + 1 });
    }
    shown.current = { key, first, count: candles.length };
  }, [ready, kind, tf, candles]);

  // 4. OHLC legend under the crosshair
  useEffect(() => {
    const c = chart.current;
    if (!ready || !c) return;
    const onMove = (p: MouseEventParams<Time>) => {
      if (p.time === undefined) return setHover(undefined);
      setHover(candles.find((k) => k.time === p.time));
    };
    c.subscribeCrosshairMove(onMove);
    return () => c.unsubscribeCrosshairMove(onMove);
  }, [ready, candles]);

  const legend = hover ?? candles.at(-1);
  const pair = series.symbol0 && series.symbol1 ? `${series.symbol0}/${series.symbol1}` : "Dark pool";
  const up = (change?.pct ?? 0) >= 0;

  return (
    <div className="card mt-4 !p-4 sm:!p-5">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="text-lg font-bold">{isPool ? pair : pendingName ?? market}</div>
          {isPool ? (
            <div className="flex flex-wrap items-baseline gap-2">
              <span className="num text-2xl font-semibold">{lastPoint ? fmtPrice(lastPoint.price) : "—"}</span>
              {series.symbol1 ? <span className="text-xs text-haze">{series.symbol1} per {series.symbol0}</span> : null}
              {change ? <span className={up ? "pill-mint" : "pill-berry"}>{`${up ? "+" : ""}${change.pct.toFixed(2)}% ${change.full ? "24h" : "in range"}`}</span> : null}
            </div>
          ) : null}
        </div>
        {isPool ? (
          <div className="flex flex-wrap items-center gap-2">
            <Segmented
              label="Chart type"
              value={kind}
              onChange={setKind}
              options={[
                { value: "candles", label: <span className="flex items-center gap-1.5"><IconCandles className="h-4 w-4" />Candles</span> },
                { value: "line", label: <span className="flex items-center gap-1.5"><IconLine className="h-4 w-4" />Line</span> },
              ]}
            />
            <Segmented label="Timeframe" value={tf} onChange={setTf} options={TIMEFRAMES} />
          </div>
        ) : null}
      </div>

      {isPool ? (
        <>
          <div className="num mt-3 flex min-h-5 flex-wrap gap-x-3 text-xs text-lavender" aria-live="off">
            {legend ? (
              <>
                <span>O <span className="text-ink">{fmtPrice(legend.open)}</span></span>
                <span>H <span className="text-mint">{fmtPrice(legend.high)}</span></span>
                <span>L <span className="text-berry-soft">{fmtPrice(legend.low)}</span></span>
                <span>C <span className="text-ink">{fmtPrice(legend.close)}</span></span>
              </>
            ) : null}
          </div>
          <div className="relative mt-2 h-80 overflow-hidden rounded-2xl border border-lavender/10" style={{ background: BG }}>
            <div
              ref={box}
              className="absolute inset-0"
              role="img"
              aria-label={lastPoint ? `${pair} price chart, ${candles.length} ${tf} bars, last ${fmtPrice(lastPoint.price)}` : `${pair} price chart`}
            />
            {/* overlays sit above the chart's canvases (z-10), which otherwise paint over them */}
            {!series.configured ? (
              <p className="absolute inset-0 z-10 grid place-items-center text-sm text-haze">The Dark Pool launches with Mainnet.</p>
            ) : series.status === "loading" && series.points.length === 0 ? (
              <Loading label="Loading price history" className="absolute inset-0 z-10 flex flex-col justify-end gap-2 p-4">
                <Skeleton className="h-full w-full" />
              </Loading>
            ) : series.points.length === 0 ? (
              <p className="absolute inset-0 z-10 grid place-items-center px-6 text-center text-sm text-haze">No reserve changes yet: the chart starts with the first trade.</p>
            ) : null}
          </div>
          <p className="mt-2 text-[11px] leading-relaxed text-haze">
            {series.status === "unavailable"
              ? "This RPC does not serve historical state, so the chart shows live prices since this page opened. "
              : "Pool spot price after every reserve change, read from the chain at that block, then live every 12 s. Flat bars: the price did not move. "}
            Charts by{" "}
            <a href="https://www.tradingview.com/" target="_blank" rel="noopener noreferrer" className="underline decoration-dotted hover:text-lavender">
              TradingView Lightweight Charts™
            </a>
            , © 2023 TradingView, Inc.
          </p>
        </>
      ) : (
        <div className="mt-4 grid h-80 place-items-center rounded-2xl border border-dashed border-lavender/20" style={{ background: BG }}>
          <div className="max-w-sm px-6 text-center">
            <p className="font-semibold text-ink">No feed for {market} yet</p>
            <p className="mt-1 text-sm text-haze">Its chart appears when a production zk-DECO price feed is connected; until then there is nothing real to plot.</p>
          </div>
        </div>
      )}
    </div>
  );
}
