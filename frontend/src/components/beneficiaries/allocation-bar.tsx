import { cx } from "@/components/ui/primitives";
import { FULL, allocationLabel, type Allocation } from "@/lib/beneficiaries";

/*
 * The live allocation: one gold segment per beneficiary, in their order, over
 * a track that is exactly 100%. The label says the exact sum in cents of a
 * percent ("100% allocated", "85% allocated · 15% left"); over 100% the track
 * stretches to the sum, a white line marks 100% and the excess turns red.
 *
 * A meter for assistive technology, valued in percent and read as its label.
 */

/** Five golds, one per beneficiary: the cards' chips use the same. */
export const SEGMENT_COLORS = ["#eab308", "#a16207", "#facc15", "#ca8a04", "#fde047"] as const;

export function segmentColor(index: number): string {
  return SEGMENT_COLORS[index % SEGMENT_COLORS.length]!;
}

export function AllocationBar({ value, className }: { value: Allocation; className?: string }) {
  const label = allocationLabel(value);
  const scale = Math.max(value.allocated, FULL);
  const over = value.allocated > FULL;
  const tone =
    value.status === "exact"
      ? "text-synthetic-yellow"
      : value.status === "over" || value.status === "invalid"
        ? "text-danger"
        : "text-ink-2";
  return (
    <div className={cx("space-y-2", className)}>
      <div className="flex items-baseline justify-between gap-3">
        <p className="text-[11px] font-semibold tracking-[0.18em] text-muted uppercase">Allocation</p>
        <p className={cx("text-sm font-semibold tabular-nums", tone)} data-testid="allocation-label" aria-hidden>
          {value.status === "invalid" ? `${label} · check the shares` : label}
        </p>
      </div>
      <div
        role="meter"
        aria-label="Allocation"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={value.allocated / 100}
        aria-valuetext={label}
        className={cx(
          "relative flex h-4 w-full overflow-hidden border bg-bg",
          value.status === "exact" ? "border-synthetic-yellow" : over ? "border-danger" : "border-border-strong",
        )}
        data-testid="allocation-bar"
        data-status={value.status}
        data-allocated={value.allocated}
      >
        {value.segments.map((segment, i) =>
          segment.cents > 0 ? (
            <span
              key={segment.key}
              className="h-full border-r-2 border-black last:border-r-0 transition-[width] duration-200 motion-reduce:transition-none"
              style={{ width: `${(segment.cents / scale) * 100}%`, backgroundColor: segmentColor(i) }}
            />
          ) : null,
        )}
        {over ? (
          <>
            {/* The excess, tinted, from the 100% line to the end. */}
            <span
              aria-hidden
              className="absolute inset-y-0 right-0 bg-danger/45"
              style={{ left: `${(FULL / scale) * 100}%` }}
            />
            <span
              aria-hidden
              className="absolute inset-y-0 w-0.5 bg-white"
              style={{ left: `${(FULL / scale) * 100}%` }}
            />
          </>
        ) : null}
      </div>
    </div>
  );
}
