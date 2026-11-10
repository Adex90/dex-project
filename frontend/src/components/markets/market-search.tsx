"use client";

import { useId, useRef, type RefObject } from "react";
import { Search, X } from "lucide-react";

import { cx } from "@/components/ui/primitives";

/*
 * Market Search: Discover's search bar. One large, square field on charcoal:
 * a magnifier that turns yellow with the focus, a yellow caret and border,
 * and a clear button once something is typed.
 *
 * - A `search` landmark with a labelled `type="search"` field: Escape clears
 *   it too, as browsers do for search fields.
 * - 16 px text or larger, so phones do not zoom into it; no autocorrect or
 *   capitalization for tickers.
 * - Clearing keeps the keyboard focus in the field.
 */

export function MarketSearch({
  value,
  onChange,
  inputRef,
  className,
}: {
  value: string;
  onChange: (value: string) => void;
  /** For a caller that moves the focus to the field (after clearing from elsewhere). */
  inputRef?: RefObject<HTMLInputElement | null>;
  className?: string;
}) {
  const id = useId();
  const ownRef = useRef<HTMLInputElement>(null);
  const field = inputRef ?? ownRef;
  return (
    <div role="search" className={cx("relative", className)}>
      <label htmlFor={id} className="sr-only">
        Search assets
      </label>
      {/* The field comes first: the icon follows its focus as a Tailwind peer. */}
      <input
        ref={field}
        id={id}
        type="search"
        value={value}
        onChange={(event) => onChange(event.target.value)}
        placeholder="Search by name, ticker or class"
        autoComplete="off"
        autoCorrect="off"
        autoCapitalize="none"
        spellCheck={false}
        enterKeyHint="search"
        maxLength={60}
        className={cx(
          "peer h-14 w-full border border-border bg-surface pr-14 pl-14 text-base text-ink caret-synthetic-yellow sm:h-16 sm:text-lg",
          "transition-colors placeholder:text-muted hover:border-border-strong",
          "focus:border-synthetic-yellow focus:ring-1 focus:ring-synthetic-yellow focus:outline-none",
          // One clear button: ours, not the browser's.
          "[&::-webkit-search-cancel-button]:appearance-none",
        )}
        data-testid="market-search"
      />
      <Search
        aria-hidden
        className="pointer-events-none absolute top-1/2 left-5 size-5 -translate-y-1/2 text-muted transition-colors peer-focus:text-synthetic-yellow"
      />
      {value !== "" ? (
        <button
          type="button"
          // The field keeps the focus: the button it was on disappears.
          onMouseDown={(event) => event.preventDefault()}
          onClick={() => {
            onChange("");
            field.current?.focus();
          }}
          aria-label="Clear search"
          className="absolute top-1/2 right-2 grid size-10 -translate-y-1/2 place-items-center text-muted transition-colors hover:text-ink focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none sm:right-3"
          data-testid="market-search-clear"
        >
          <X className="size-5" aria-hidden />
        </button>
      ) : null}
    </div>
  );
}
