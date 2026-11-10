"use client";

import { useId } from "react";

import { cx } from "@/components/ui/primitives";

/*
 * Segmented Control: one choice among a few, as square hairline segments; the
 * chosen one is solid Synthetic Gold with black text, like the chart's range
 * picker and the phone's active tab.
 *
 * Native radio buttons underneath: Tab enters the group, the arrow keys move
 * the choice, and screen readers announce "radio, 2 of 4" under the legend.
 */

export interface SegmentOption<T extends string> {
  value: T;
  label: string;
}

export function SegmentedControl<T extends string>({
  legend,
  options,
  value,
  onChange,
  disabled = false,
  className,
  testId,
}: {
  /** What is being chosen, for screen readers ("Order type"). */
  legend: string;
  options: readonly SegmentOption<T>[];
  value: T;
  onChange: (value: T) => void;
  disabled?: boolean;
  className?: string;
  testId?: string;
}) {
  const name = useId();
  return (
    <fieldset className={cx("min-w-0", className)} disabled={disabled} data-testid={testId}>
      <legend className="sr-only">{legend}</legend>
      <div
        className="grid border border-border bg-bg p-0.5"
        style={{ gridTemplateColumns: `repeat(${options.length}, minmax(0, 1fr))` }}
      >
        {options.map((option) => {
          const selected = option.value === value;
          return (
            <label key={option.value} className="relative min-w-0 cursor-pointer has-[:disabled]:cursor-not-allowed">
              <input
                type="radio"
                name={name}
                value={option.value}
                checked={selected}
                onChange={() => onChange(option.value)}
                className="peer sr-only"
              />
              <span
                className={cx(
                  "flex min-h-10 items-center justify-center px-1 text-center text-[11px] leading-tight font-semibold tracking-[0.08em] uppercase transition-colors",
                  "peer-focus-visible:ring-2 peer-focus-visible:ring-synthetic-yellow peer-focus-visible:ring-inset",
                  "peer-disabled:opacity-50",
                  selected ? "bg-synthetic-yellow text-black" : "text-muted hover:bg-surface-2 hover:text-ink",
                )}
              >
                {option.label}
              </span>
            </label>
          );
        })}
      </div>
    </fieldset>
  );
}
