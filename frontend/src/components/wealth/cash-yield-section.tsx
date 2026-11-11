"use client";

import { useEffect, useId, useRef, useState } from "react";

import { cx } from "@/components/ui/primitives";
import { formatAmount, formatMoney } from "@/lib/decimal";
import {
  SOVEREIGN_YIELD_PCT,
  YIELD_BASIS_DAYS,
  accrued,
  counterDecimals,
  formatTruncated,
  monthWindow,
  shareBasisPoints,
  yieldPreview,
} from "@/lib/wealth";
import { useVaultStore } from "@/store/vault";
import type { DecimalString } from "@/types/vault";

/*
 * Cash & Yield: the uninvested cash and what it earns, live, in the
 * Sovereign Gold identity. A preview: the 7.25 % Sovereign Yield is mocked
 * and nothing is paid (lib/wealth.ts, and the licensing note there).
 *
 * - The counter: "Earned this month", ticking up as the time passes, as if
 *   today's available cash (`useVaultStore`) had earned 7.25 % a year since
 *   the 1st (ACT/360). Exact to 10^-8 and rounded down; enough decimals for
 *   the last one to move every tenth of a second (`counterDecimals`). The
 *   digits are written straight into the page on each animation frame, so
 *   React does not render 60 times a second; with reduced motion they move
 *   once a second. A new month starts again from zero.
 * - Under it: the month so far as a gold bar, with what the whole month
 *   would earn; then the cash, a day's yield and the rate.
 * - Screen readers get the figure to the cent once (not every tick).
 */

const RATE_LABEL = `${formatAmount(SOVEREIGN_YIELD_PCT, 2)}%`;

export function CashYieldSection({ className }: { className?: string }) {
  const titleId = useId();
  const available = useVaultStore((s) => s.balance?.available_cash ?? null);

  return (
    <section
      aria-labelledby={titleId}
      className={cx(
        "@container relative overflow-hidden border border-border bg-surface p-5 sm:p-7",
        // A gold hairline along the top edge, as on the vault's balance.
        "before:pointer-events-none before:absolute before:inset-x-0 before:top-0 before:h-px",
        "before:bg-gradient-to-r before:from-transparent before:via-synthetic-yellow/70 before:to-transparent",
        className,
      )}
      data-testid="cash-yield"
    >
      <header className="flex flex-wrap items-center justify-between gap-3">
        <h2
          id={titleId}
          className="inline-flex items-center gap-2 text-[11px] font-semibold tracking-[0.24em] text-synthetic-yellow uppercase"
        >
          <span aria-hidden className="size-1.5 bg-synthetic-yellow" />
          Sovereign Yield
        </h2>
        <span
          className="border border-synthetic-yellow/40 px-2.5 py-1 text-[10px] font-semibold tracking-[0.2em] text-synthetic-yellow uppercase"
          data-testid="cash-yield-preview"
        >
          Preview · {RATE_LABEL} a year
        </span>
      </header>
      {available === null ? <CounterSkeleton /> : <YieldCounter available={available} />}
    </section>
  );
}

function YieldCounter({ available }: { available: DecimalString }) {
  // The figures that do not tick are read once, when the balance arrives or changes.
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => setNow(Date.now()), [available]);
  const month = monthWindow(now);
  const preview = yieldPreview(available, now);
  const decimals = counterDecimals(available);
  const integerRef = useRef<HTMLSpanElement>(null);
  const tailRef = useRef<HTMLSpanElement>(null);
  const first = splitFigure(formatTruncated(preview.earned, decimals));
  const growing = preview.perDay > 0n;

  useEffect(() => {
    if (!growing) return;
    const still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    let frame = 0;
    let painted = 0;
    let shown = "";
    const paint = (time: number) => {
      if (!still || time - painted >= 1_000) {
        painted = time;
        const at = Date.now();
        const text = formatTruncated(accrued(available, SOVEREIGN_YIELD_PCT, at - monthWindow(at).start), decimals);
        if (text !== shown) {
          shown = text;
          const { head, tail } = splitFigure(text);
          if (integerRef.current) integerRef.current.textContent = head;
          if (tailRef.current) tailRef.current.textContent = tail;
        }
      }
      frame = requestAnimationFrame(paint);
    };
    frame = requestAnimationFrame(paint);
    return () => cancelAnimationFrame(frame);
  }, [available, decimals, growing]);

  const elapsed = shareBasisPoints(BigInt(Math.max(0, now - month.start)), BigInt(month.end - month.start));

  return (
    <>
      <div className="mt-7">
        <p className="flex items-center gap-2 text-sm text-muted">
          {growing ? (
            <span className="relative flex size-2" aria-hidden>
              <span className="absolute inline-flex size-full rounded-full bg-synthetic-yellow opacity-60 motion-safe:animate-ping motion-safe:[animation-duration:2.4s]" />
              <span className="relative inline-flex size-2 rounded-full bg-synthetic-yellow" />
            </span>
          ) : null}
          Earned this month
        </p>
        <p className="sr-only">{`Earned this month, simulated: ${formatMoney(preview.earned)} MXN`}</p>
        {/* Visual only, and live: the digits change many times a second. */}
        <p
          aria-hidden
          className="mt-2 flex flex-wrap items-baseline gap-x-3 gap-y-1 leading-none"
          data-testid="cash-yield-counter"
        >
          <span className="text-[clamp(2.5rem,9cqw,4.75rem)] font-light tracking-tighter text-ink tabular-nums">
            <span className="mr-[0.06em] text-[0.5em] font-normal text-synthetic-yellow">$</span>
            <span ref={integerRef} data-testid="cash-yield-earned">
              {first.head}
            </span>
            <span ref={tailRef} className="text-[0.5em] text-muted">
              {first.tail}
            </span>
          </span>
          <span className="text-sm font-medium tracking-[0.12em] text-muted">MXN</span>
        </p>
        {growing ? null : (
          <p className="mt-3 text-sm text-muted" data-testid="cash-yield-empty">
            Add cash to your vault and watch it grow here.
          </p>
        )}
      </div>

      {/* The month so far, and what the whole of it would earn at this balance. */}
      <div className="mt-6">
        <div
          role="progressbar"
          aria-label="This month so far"
          aria-valuemin={0}
          aria-valuemax={month.days}
          aria-valuenow={month.day}
          aria-valuetext={`Day ${month.day} of ${month.days}`}
          className="h-1.5 bg-surface-2"
        >
          <div className="h-full bg-synthetic-yellow" style={{ width: `${elapsed / 100}%` }} />
        </div>
        <p className="mt-2 flex flex-wrap justify-between gap-x-4 gap-y-1 text-xs text-muted tabular-nums">
          <span>
            Day {month.day} of {month.days}
          </span>
          <span data-testid="cash-yield-projected">
            <span className="font-semibold text-ink">{formatMoney(preview.projected)}</span> for the whole month
          </span>
        </p>
      </div>

      {/* Hairline grid: a 1 px gap over the border color draws the dividers. */}
      <dl className="mt-6 grid grid-cols-1 gap-px bg-border ring-1 ring-border @md:grid-cols-3">
        <Figure label="Uninvested cash" value={formatMoney(available)} testId="cash-yield-available" />
        <Figure label="Per day" value={formatMoney(preview.perDay)} testId="cash-yield-per-day" />
        <Figure label="Rate" value={RATE_LABEL} note={`a year, ACT/${YIELD_BASIS_DAYS}`} />
      </dl>

      <p className="mt-4 text-xs text-muted" data-testid="cash-yield-note">
        Preview: simulated on today&apos;s available cash since the 1st. Nothing is paid yet.
      </p>
    </>
  );
}

/** "1,234.567890" → the pesos and cents ("1,234.56"), and the finer digits after them ("7890"). */
function splitFigure(text: string): { head: string; tail: string } {
  const point = text.indexOf(".");
  if (point === -1) return { head: text, tail: "" };
  return { head: text.slice(0, point + 3), tail: text.slice(point + 3) };
}

function Figure({ label, value, note, testId }: { label: string; value: string; note?: string; testId?: string }) {
  return (
    <div className="bg-surface-2 px-4 py-3.5">
      <dt className="text-[11px] font-medium tracking-[0.18em] text-muted uppercase">{label}</dt>
      <dd className="mt-1.5 text-lg font-medium tracking-tight text-ink tabular-nums" data-testid={testId}>
        {value}
        {note ? <span className="ml-2 text-xs font-normal text-muted">{note}</span> : null}
      </dd>
    </div>
  );
}

function CounterSkeleton() {
  return (
    <div role="status" className="mt-7">
      <span className="sr-only">Loading your cash…</span>
      <div aria-hidden className="motion-safe:animate-pulse">
        <div className="h-4 w-32 bg-surface-2" />
        <div className="mt-3 h-14 w-72 max-w-full bg-surface-2" />
        <div className="mt-6 h-1.5 bg-surface-2" />
        <div className="mt-6 h-20 bg-surface-2" />
      </div>
    </div>
  );
}
