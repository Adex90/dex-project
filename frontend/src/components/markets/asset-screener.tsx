"use client";

import { useId, type ReactNode } from "react";
import { ArrowDownRight, ArrowUpRight, SlidersHorizontal } from "lucide-react";

import { cx } from "@/components/ui/primitives";
import {
  DIRECTION_OPTIONS,
  NO_FILTERS,
  isFiltered,
  toggleClass,
  toggleDirection,
  type ClassOption,
  type Direction,
  type FacetCounts,
  type ScreenerFilters,
} from "@/lib/screener";

/*
 * Asset Screener: Discover's inline filter panel, in the Sovereign Gold
 * identity. It narrows the market list below it as each toggle changes.
 *
 * - Two rows of square toggles: the asset class (Equities, Commodities,
 *   Fixed income) and the 24-hour performance (Positive, Negative). Within
 *   a row the choices add up; across rows they narrow (lib/screener.ts).
 * - A toggle that is on is solid Synthetic Gold with black text, like the
 *   chosen range of a chart. Each shows how many assets it would leave with
 *   the other row's choices and the search: a toggle that would leave none
 *   is disabled until something else changes (one that is on stays
 *   clickable, to turn it off).
 * - The header says how many assets the list shows, of how many, and clears
 *   every filter at once.
 * - Native buttons with aria-pressed, in labelled groups; the page announces
 *   the count as it changes.
 *
 * Presentational: the page owns the filters (the market store), the counts
 * (`facetCounts`) and what the list shows.
 */

export interface AssetScreenerProps {
  /** The classes to offer (`classOptions`). */
  classes: readonly ClassOption[];
  filters: ScreenerFilters;
  /** What each toggle would leave (`facetCounts` of the searched catalog). */
  counts: FacetCounts;
  /** How many assets the list shows, and of how many (the search applied, the filters not). */
  shown: number;
  total: number;
  onChange: (filters: ScreenerFilters) => void;
  className?: string;
}

const DIRECTION_ICONS: Record<Direction, { Icon: typeof ArrowUpRight; className: string }> = {
  up: { Icon: ArrowUpRight, className: "text-gain" },
  down: { Icon: ArrowDownRight, className: "text-loss" },
};

export function AssetScreener({ classes, filters, counts, shown, total, onChange, className }: AssetScreenerProps) {
  const titleId = useId();
  const classesId = useId();
  const directionsId = useId();
  const filtered = isFiltered(filters);

  return (
    <section
      aria-labelledby={titleId}
      className={cx("border border-border bg-surface", className)}
      data-testid="asset-screener"
    >
      <header className="flex flex-wrap items-center gap-x-4 gap-y-1 border-b border-border px-4 py-3 sm:px-5">
        <h2
          id={titleId}
          className="inline-flex items-center gap-2 text-[11px] font-semibold tracking-[0.24em] text-ink uppercase"
        >
          <SlidersHorizontal className="size-3.5 text-synthetic-yellow" aria-hidden />
          Screener
        </h2>
        <p className="text-xs text-muted tabular-nums" data-testid="screener-count">
          <span className="font-semibold text-ink">{shown}</span> of {total} {total === 1 ? "asset" : "assets"}
        </p>
        {filtered ? (
          <button
            type="button"
            onClick={() => onChange(NO_FILTERS)}
            className="ml-auto text-xs font-semibold text-synthetic-yellow underline-offset-2 hover:underline focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
            data-testid="screener-clear"
          >
            Clear filters
          </button>
        ) : null}
      </header>

      {/* The two rows side by side once both fit on one line. */}
      <div className="flex flex-col gap-3 px-4 py-3.5 sm:px-5 xl:flex-row xl:items-center xl:gap-10">
        <ToggleRow labelId={classesId} label="Asset class">
          {classes.map((option) => (
            <Toggle
              key={option.value}
              label={option.label}
              on={filters.classes.includes(option.value)}
              count={counts.classes[option.value] ?? 0}
              onToggle={() => onChange(toggleClass(filters, option.value))}
              testValue={option.value}
            />
          ))}
        </ToggleRow>
        <ToggleRow labelId={directionsId} label="Performance · 24h">
          {DIRECTION_OPTIONS.map((option) => {
            const { Icon, className: tone } = DIRECTION_ICONS[option.value];
            const on = filters.directions.includes(option.value);
            return (
              <Toggle
                key={option.value}
                label={option.label}
                icon={<Icon className={cx("size-3.5", on ? "text-black" : tone)} aria-hidden />}
                on={on}
                count={counts.directions[option.value]}
                onToggle={() => onChange(toggleDirection(filters, option.value))}
                testValue={option.value}
              />
            );
          })}
        </ToggleRow>
      </div>
    </section>
  );
}

/* ------------------------------------------------------------------------ */

/** A labelled group of toggles: the label above them on a phone, beside them from sm. */
function ToggleRow({ labelId, label, children }: { labelId: string; label: string; children: ReactNode }) {
  return (
    <div role="group" aria-labelledby={labelId} className="flex flex-wrap items-center gap-2">
      <span
        id={labelId}
        className="w-full text-[10px] font-semibold tracking-[0.2em] text-muted uppercase sm:mr-1 sm:w-auto"
      >
        {label}
      </span>
      {children}
    </div>
  );
}

/** One filter: a square toggle, solid gold while on, with what it would leave. */
function Toggle({
  label,
  icon,
  on,
  count,
  onToggle,
  testValue,
}: {
  label: string;
  icon?: ReactNode;
  on: boolean;
  count: number;
  onToggle: () => void;
  testValue: string;
}) {
  return (
    <button
      type="button"
      aria-pressed={on}
      // Nothing to show: off and disabled until another choice makes room. On stays clickable, to turn it off.
      disabled={!on && count === 0}
      onClick={onToggle}
      className={cx(
        "inline-flex h-9 items-center gap-2 border px-3 text-xs font-semibold transition-colors",
        "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
        on
          ? "border-synthetic-yellow bg-synthetic-yellow text-black hover:bg-synthetic-yellow-hover"
          : "border-border bg-bg text-ink-2 hover:border-border-strong hover:text-ink",
        "disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:border-border disabled:hover:text-ink-2",
      )}
      data-testid="screener-option"
      data-value={testValue}
    >
      {icon}
      {label}
      <span
        aria-hidden
        className={cx("font-mono text-[11px] tabular-nums", on ? "text-black/70" : "text-muted")}
        data-testid="screener-option-count"
      >
        {count}
      </span>
      <span className="sr-only">
        , {count} {count === 1 ? "asset" : "assets"}
      </span>
    </button>
  );
}
