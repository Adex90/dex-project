"use client";

import { useEffect, type MouseEvent } from "react";
import { Star } from "lucide-react";

import { useDashboard } from "@/components/layout/dashboard-shell";
import { cx } from "@/components/ui/primitives";
import { useSignOutOn401 } from "@/hooks/use-sign-out-on-401";
import { describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import { watchLabel } from "@/lib/watchlist";
import { isWatched, useWatchlistStore } from "@/store/watchlist";
import type { MarketListing } from "@/types/market";

/*
 * The watchlist star, in the Sovereign Gold identity: outlined zinc when the
 * asset is not followed, solid gold when it is.
 *
 * - `icon`: a 40 px square with the star alone, for table rows (Discover's
 *   catalog, the Vault's Watchlist). A row ignores clicks on its buttons, so
 *   the star never opens the asset.
 * - `labelled`: a square hairline button with its word ("Watch",
 *   "Watching"), for an asset's header.
 *
 * Pressing it asks for the new state (store/watchlist.ts: optimistic, and a
 * retry cannot flip it back). Until the watchlist first loads the star is
 * disabled, so it never shows an asset as not followed when it is. A failure
 * restores the star; `WatchError` says why, beside it.
 */

/** A failure stays this long. */
const NOTICE_MS = 6_000;

export interface WatchToggleProps {
  ticker: string;
  /** The asset's listing, so a newly followed asset shows in the Vault's Watchlist before the next load. */
  asset: MarketListing;
  variant?: "icon" | "labelled";
  className?: string;
}

export function WatchToggle({ ticker, asset, variant = "icon", className }: WatchToggleProps) {
  const { token } = useDashboard();
  const signOutOn401 = useSignOutOn401();
  const watched = useWatchlistStore((s) => isWatched(s, ticker));
  const loading = useWatchlistStore((s) => s.status === "idle" || s.status === "loading");
  const label = watchLabel(ticker, watched);

  const toggle = (event: MouseEvent<HTMLButtonElement>) => {
    event.stopPropagation();
    void useWatchlistStore
      .getState()
      .set(ticker, !watched, (t, w) => api.setWatched(token, t, w).catch(signOutOn401), asset);
  };

  const icon = (
    <Star
      className={cx(variant === "icon" ? "size-[18px]" : "size-4", watched && "fill-current")}
      strokeWidth={watched ? 1.5 : 2}
      aria-hidden
    />
  );

  return (
    <button
      type="button"
      onClick={toggle}
      disabled={loading}
      aria-pressed={watched}
      aria-label={label}
      title={label}
      className={cx(
        "transition-colors disabled:cursor-wait disabled:opacity-50",
        "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
        variant === "icon"
          ? cx(
              "grid size-10 place-items-center",
              watched
                ? "text-synthetic-yellow hover:text-synthetic-yellow-hover"
                : "text-muted hover:text-synthetic-yellow",
            )
          : cx(
              "inline-flex h-8 items-center gap-1.5 border px-2.5 text-[11px] font-semibold tracking-[0.18em] uppercase",
              watched
                ? "border-synthetic-yellow/60 bg-synthetic-yellow/10 text-synthetic-yellow hover:bg-synthetic-yellow/15"
                : "border-border text-muted hover:border-synthetic-yellow/60 hover:text-synthetic-yellow",
            ),
        className,
      )}
      data-testid="watch-toggle"
      data-ticker={ticker}
      data-watched={watched}
    >
      {icon}
      {variant === "labelled" ? (watched ? "Watching" : "Watch") : null}
    </button>
  );
}

/** Why the star for `ticker` did not change, while it is news; nothing otherwise. */
export function WatchError({ ticker, className }: { ticker: string; className?: string }) {
  const notice = useWatchlistStore((s) => (s.notice?.ticker === ticker ? s.notice : null));

  useEffect(() => {
    if (notice === null) return;
    const timer = setTimeout(() => {
      if (useWatchlistStore.getState().notice === notice) useWatchlistStore.getState().dismissNotice();
    }, NOTICE_MS);
    return () => clearTimeout(timer);
  }, [notice]);

  if (notice === null) return null;
  return (
    <span role="alert" className={cx("block text-xs text-danger", className)} data-testid="watch-error">
      {notice.watched ? "Not added to your watchlist" : "Not removed from your watchlist"}:{" "}
      {describeError(notice.error)}
    </span>
  );
}
