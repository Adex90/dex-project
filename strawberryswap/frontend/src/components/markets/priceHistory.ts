"use client";

import { useEffect, useState } from "react";
import { parseAbiItem, type Address } from "viem";
import { darkPoolAbi } from "@/lib/abi";
import { publicClient } from "@/lib/clients";
import { config } from "@/lib/config";
import { useApp } from "@/components/AppState";

/** One observation of the pool's spot price (token1 per token0) at a unix time in seconds. */
export interface PricePoint {
  time: number;
  price: number;
}

export type HistoryStatus = "idle" | "loading" | "ready" | "unavailable";

// `DarkPool` emits Sync after every reserve change (batch settlement, rebalance, liquidity). The price itself depends
// on virtual reserves too, so it is not recomputed from the event: the pool's own spotPrice() is read AT that block.
const SYNC = parseAbiItem("event Sync(uint256 reserve0, uint256 reserve1)");
const WINDOW_BLOCKS = 50_000n; // ~1 week at 12 s blocks
const MAX_EVENTS = 240; // newest reserve changes priced from chain
const CONCURRENCY = 6; // parallel RPC reads (be gentle with public endpoints)
const CACHE_MS = 60_000;

let cache: { pool: Address; at: number; points: PricePoint[]; status: HistoryStatus } | undefined;

async function mapLimit<T, R>(items: T[], n: number, fn: (t: T) => Promise<R>): Promise<R[]> {
  const out: R[] = new Array(items.length);
  let next = 0;
  const worker = async () => {
    while (next < items.length) {
      const i = next++;
      out[i] = await fn(items[i] as T);
    }
  };
  await Promise.all(Array.from({ length: Math.min(n, items.length) }, worker));
  return out;
}

/**
 * Price after every reserve change in the recent window, plus the price at the window's start. Exact wherever the RPC
 * serves historical state (anvil, archive nodes); where it cannot, every read fails and the status is "unavailable".
 */
async function loadHistory(pool: Address): Promise<{ points: PricePoint[]; status: HistoryStatus }> {
  const head = await publicClient.getBlockNumber();
  const windowStart = head > WINDOW_BLOCKS ? head - WINDOW_BLOCKS : 0n;
  const from = windowStart > config.deployBlock ? windowStart : config.deployBlock;
  const blocks = new Set<bigint>();
  for (let hi = head; blocks.size < MAX_EVENTS; ) {
    const lo = hi - config.logChunk + 1n > from ? hi - config.logChunk + 1n : from;
    const logs = await publicClient.getLogs({ address: pool, event: SYNC, fromBlock: lo, toBlock: hi });
    for (const l of logs) if (l.blockNumber !== null) blocks.add(l.blockNumber);
    if (lo <= from) break;
    hi = lo - 1n;
  }
  const picked = [...blocks].sort((a, b) => (a < b ? -1 : a > b ? 1 : 0)).slice(-MAX_EVENTS);
  const first = picked.at(0);
  if (first !== undefined && first > from) picked.unshift(from); // where the price stood when the window opens
  if (picked.length === 0) return { points: [], status: "ready" };

  let failures = 0;
  const pts = await mapLimit(picked, CONCURRENCY, async (blockNumber): Promise<PricePoint | undefined> => {
    try {
      const [block, spot] = await Promise.all([
        publicClient.getBlock({ blockNumber }),
        publicClient.readContract({ address: pool, abi: darkPoolAbi, functionName: "spotPrice", blockNumber }),
      ]);
      const price = Number(spot) / 1e18;
      return Number.isFinite(price) && price > 0 ? { time: Number(block.timestamp), price } : undefined;
    } catch {
      failures++; // e.g. pool not yet seeded at that block, or a node without historical state
      return undefined;
    }
  });
  const points = pts.filter((p): p is PricePoint => !!p).sort((a, b) => a.time - b.time);
  return { points, status: points.length === 0 && failures > 0 ? "unavailable" : "ready" };
}

/**
 * The pool's price series for charts: on-chain history (loaded once, cached for a minute across tab switches) followed
 * by a live point on every pool poll (12 s). Nothing is interpolated: each point is a price the chain reported.
 */
export function usePriceSeries() {
  const { pool } = useApp();
  const addr = config.darkPool;
  const [history, setHistory] = useState<{ points: PricePoint[]; status: HistoryStatus }>(() =>
    cache && cache.pool === addr && Date.now() - cache.at < CACHE_MS ? { points: cache.points, status: cache.status } : { points: [], status: addr ? "loading" : "idle" },
  );
  const [live, setLive] = useState<PricePoint[]>([]);

  useEffect(() => {
    if (!addr) return;
    if (cache && cache.pool === addr && Date.now() - cache.at < CACHE_MS) return;
    let gone = false;
    loadHistory(addr)
      .then((h) => {
        cache = { pool: addr, at: Date.now(), ...h };
        if (!gone) setHistory(h);
      })
      .catch(() => {
        if (!gone) setHistory({ points: [], status: "unavailable" });
      });
    return () => {
      gone = true;
    };
  }, [addr]);

  const state = pool.state;
  useEffect(() => {
    if (!state) return;
    const price = Number(state.spot) / 1e18;
    if (!Number.isFinite(price) || price <= 0) return;
    setLive((l) => [...l, { time: Math.floor(Date.now() / 1000), price }].slice(-720));
  }, [state]);

  const lastHistory = history.points.at(-1)?.time ?? 0;
  const points = [...history.points, ...live.filter((p) => p.time > lastHistory)];
  return { configured: !!addr, status: history.status, points, symbol0: state?.symbol0, symbol1: state?.symbol1 };
}

export type PriceSeries = ReturnType<typeof usePriceSeries>;

export interface Candle {
  time: number;
  open: number;
  high: number;
  low: number;
  close: number;
  /** false when the price did not move inside the bucket (drawn muted). */
  moved: boolean;
}

/**
 * OHLC buckets from price observations. The pool price is a step function (it only changes when reserves change), so a
 * bucket with no observation is a flat bar at the previous close: the price really stood there.
 */
export function toCandles(points: PricePoint[], bucketSec: number, nowSec: number, maxBars = 400): Candle[] {
  const firstPoint = points.at(0);
  if (!firstPoint) return [];
  const startAll = Math.floor(firstPoint.time / bucketSec) * bucketSec;
  const end = Math.floor(Math.max(nowSec, points.at(-1)?.time ?? nowSec) / bucketSec) * bucketSec;
  const start = Math.max(startAll, end - (maxBars - 1) * bucketSec);

  let i = 0;
  let last = firstPoint.price;
  while (i < points.length && (points[i]?.time ?? Infinity) < start) last = points[i++]!.price;

  const out: Candle[] = [];
  for (let t = start; t <= end; t += bucketSec) {
    const open = last;
    let high = last;
    let low = last;
    let moved = false;
    while (i < points.length && (points[i]?.time ?? Infinity) < t + bucketSec) {
      const p = points[i++]!.price;
      if (p !== last) moved = true;
      high = Math.max(high, p);
      low = Math.min(low, p);
      last = p;
    }
    out.push({ time: t, open, high, low, close: last, moved: moved || open !== last });
  }
  return out;
}

/** Change of the latest price against the newest observation at least `periodSec` old (or the oldest one we have). */
export function changeOver(points: PricePoint[], periodSec: number, nowSec: number): { pct: number; full: boolean } | undefined {
  const last = points.at(-1);
  const first = points.at(0);
  if (!last || !first) return undefined;
  let ref = first;
  for (const p of points) {
    if (p.time <= nowSec - periodSec) ref = p;
    else break;
  }
  return { pct: ((last.price - ref.price) / ref.price) * 100, full: first.time <= nowSec - periodSec };
}
