import { cx } from "@/components/ui/primitives";
import { CHART_RANGES, type ChartRangeId } from "@/lib/portfolio";

/**
 * The time ranges of a chart, as a square segmented control: the chosen one
 * is solid Synthetic Gold with black text. Each button says in full what it
 * shows ("Show the last month") and reports its state with aria-pressed.
 */
export function RangePicker({
  ranges,
  value,
  onChange,
}: {
  ranges: readonly ChartRangeId[];
  value: ChartRangeId;
  onChange: (id: ChartRangeId) => void;
}) {
  return (
    <div role="group" aria-label="Time range" className="flex border border-border bg-bg p-0.5">
      {ranges.map((id) => {
        const range = CHART_RANGES.find((r) => r.id === id)!;
        const selected = id === value;
        return (
          <button
            key={id}
            type="button"
            aria-pressed={selected}
            aria-label={`Show ${range.name}`}
            onClick={() => onChange(id)}
            className={cx(
              "min-h-8 min-w-10 px-2.5 font-mono text-xs font-semibold transition-colors",
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
