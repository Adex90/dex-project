"use client";

import { useId, type ReactNode } from "react";
import Link from "next/link";
import { ArrowDownRight, ArrowUpRight, Building2, Check, Gem, Landmark, type LucideIcon } from "lucide-react";

import { TickerChip } from "@/components/markets/asset-table";
import { cx } from "@/components/ui/primitives";
import { formatMoney } from "@/lib/decimal";
import { assetPath } from "@/lib/market-data";
import {
  COLLECTIONS,
  collectionStats,
  isCollectionActive,
  type Collection,
  type ScreenerFilters,
  type TopMovers as Movers,
} from "@/lib/screener";
import { signedPercent, type Trend } from "@/lib/vault";
import type { MarketListing } from "@/types/market";

/*
 * Discover's highlights, above the screener: the day's top movers and the
 * thematic collections, on charcoal cards in the Sovereign Gold identity.
 *
 * - On a phone each section is one row that swipes sideways, card by card,
 *   the next card peeking in; from sm up it is a grid (two movers, three
 *   collections), and from xl both sections share a row.
 * - Top movers: the largest rise and the largest fall over 24 hours in the
 *   whole catalog (`topMovers`), the change large in green or red with its
 *   sign, the asset and its price. Each card opens the asset's page.
 * - Collections: presets of the screener's asset class (Hard Assets,
 *   Synthetic Equities, Fixed Income), each with its assets and their
 *   equal-weight 24-hour change. A collection is a toggle: picking it sets
 *   the screener (gold while it is the screener's choice), picking it again
 *   clears it.
 *
 * Presentational: the page owns the catalog and the filters.
 */

/** A section's row of cards: one swiping row on a phone (snapping card by card), a grid from sm. */
function Rail({ children, columns, label }: { children: ReactNode; columns: string; label: string }) {
  return (
    <ul
      aria-label={label}
      className={cx(
        // Edge to edge on a phone (the page's 16 px gutter becomes the row's own padding), no scrollbar.
        // flex-1: as tall as the section, so the movers and the collections end level when they share a row.
        "-mx-4 flex flex-1 snap-x snap-mandatory scroll-px-4 gap-3 overflow-x-auto px-4 pb-1",
        "[scrollbar-width:none] [&::-webkit-scrollbar]:hidden",
        "sm:mx-0 sm:grid sm:snap-none sm:gap-4 sm:overflow-visible sm:px-0 sm:pb-0",
        columns,
      )}
    >
      {children}
    </ul>
  );
}

/** A card's place in a rail: most of a phone's width, so the next one peeks in. */
const RAIL_ITEM = "w-[80%] max-w-80 shrink-0 snap-start sm:w-auto sm:max-w-none";

function SectionTitle({ id, children }: { id: string; children: ReactNode }) {
  return (
    <h2
      id={id}
      className="mb-3 inline-flex items-center gap-2 text-[11px] font-semibold tracking-[0.24em] text-muted uppercase"
    >
      <span aria-hidden className="size-1.5 bg-synthetic-yellow" />
      {children}
    </h2>
  );
}

const CARD = "h-full border bg-surface p-4 transition-colors sm:p-5";
const FOCUS =
  "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-bg focus-visible:outline-none";

const TREND: Record<Trend, { Icon: LucideIcon; text: string }> = {
  up: { Icon: ArrowUpRight, text: "text-gain" },
  down: { Icon: ArrowDownRight, text: "text-loss" },
  flat: { Icon: ArrowUpRight, text: "text-muted" },
};

/* ------------------------------------------------------------------------ */
/* Top movers                                                               */
/* ------------------------------------------------------------------------ */

export function TopMovers({ movers, className }: { movers: Movers; className?: string }) {
  const titleId = useId();
  return (
    <section aria-labelledby={titleId} className={cx("flex min-w-0 flex-col", className)} data-testid="top-movers">
      <SectionTitle id={titleId}>Top movers · 24h</SectionTitle>
      <Rail label="Top movers" columns="sm:grid-cols-2">
        <li className={RAIL_ITEM}>
          <MoverCard kind="gainer" asset={movers.gainer} />
        </li>
        <li className={RAIL_ITEM}>
          <MoverCard kind="loser" asset={movers.loser} />
        </li>
      </Rail>
    </section>
  );
}

function MoverCard({ kind, asset }: { kind: "gainer" | "loser"; asset: MarketListing | null }) {
  const title = kind === "gainer" ? "Top gainer" : "Top loser";
  const { Icon, text } = TREND[kind === "gainer" ? "up" : "down"];
  const eyebrow = (
    <span className="inline-flex items-center gap-1.5 text-[11px] font-semibold tracking-[0.2em] text-muted uppercase">
      <Icon className={cx("size-3.5", text)} aria-hidden />
      {title}
    </span>
  );

  if (!asset) {
    return (
      <div className={cx(CARD, "flex flex-col gap-6 border-border")} data-testid="top-mover" data-kind={kind}>
        {eyebrow}
        <span className="text-sm text-muted">
          {kind === "gainer" ? "Nothing rose in the last 24 hours." : "Nothing fell in the last 24 hours."}
        </span>
      </div>
    );
  }

  const change = signedPercent(asset.change_24h_pct);
  return (
    <Link
      href={assetPath(asset.ticker)}
      aria-label={`${title} over 24 hours: ${asset.name}, ${asset.ticker}, ${change.text}, at ${formatMoney(asset.price)}`}
      className={cx(
        CARD,
        "group flex flex-col gap-3 border-border hover:border-border-strong hover:bg-surface-2",
        FOCUS,
      )}
      data-testid="top-mover"
      data-kind={kind}
      data-ticker={asset.ticker}
    >
      {eyebrow}
      <span className="flex min-w-0 flex-col items-start gap-2">
        <TickerChip ticker={asset.ticker} />
        <span className="block w-full truncate text-sm font-medium text-ink">{asset.name}</span>
      </span>
      <span className="mt-auto flex flex-wrap items-end justify-between gap-x-3 gap-y-1 pt-1">
        {/* A large standalone figure: proportional digits, with its sign, so the color is never the only cue. */}
        <span className={cx("text-3xl leading-none font-semibold tracking-tight", text)} data-testid="top-mover-change">
          {change.text}
        </span>
        <span className="text-sm text-ink-2 tabular-nums">{formatMoney(asset.price)}</span>
      </span>
    </Link>
  );
}

/* ------------------------------------------------------------------------ */
/* Collections                                                              */
/* ------------------------------------------------------------------------ */

const COLLECTION_ICONS: Record<string, LucideIcon> = {
  "hard-assets": Gem,
  "synthetic-equities": Building2,
  "fixed-income": Landmark,
};

export function Collections({
  assets,
  filters,
  onPick,
  className,
}: {
  /** The whole catalog: a collection always shows all its assets, whatever the screener shows. */
  assets: readonly MarketListing[];
  filters: ScreenerFilters;
  onPick: (collection: Collection) => void;
  className?: string;
}) {
  const titleId = useId();
  return (
    <section aria-labelledby={titleId} className={cx("flex min-w-0 flex-col", className)} data-testid="collections">
      <SectionTitle id={titleId}>Collections</SectionTitle>
      <Rail label="Collections" columns="sm:grid-cols-3">
        {COLLECTIONS.map((collection) => (
          <li key={collection.id} className={RAIL_ITEM}>
            <CollectionCard
              collection={collection}
              assets={assets}
              active={isCollectionActive(filters, collection)}
              onPick={() => onPick(collection)}
            />
          </li>
        ))}
      </Rail>
    </section>
  );
}

function CollectionCard({
  collection,
  assets,
  active,
  onPick,
}: {
  collection: Collection;
  assets: readonly MarketListing[];
  active: boolean;
  onPick: () => void;
}) {
  const { members, change } = collectionStats(assets, collection);
  const Icon = COLLECTION_ICONS[collection.id] ?? Gem;
  const move = change === null ? null : signedPercent(change);
  const count = `${members.length} ${members.length === 1 ? "asset" : "assets"}`;
  return (
    <button
      type="button"
      aria-pressed={active}
      disabled={members.length === 0 && !active}
      onClick={onPick}
      className={cx(
        CARD,
        "flex w-full flex-col gap-4 text-left",
        active
          ? "border-synthetic-yellow bg-synthetic-yellow/[0.06]"
          : "border-border hover:border-border-strong hover:bg-surface-2",
        "disabled:cursor-not-allowed disabled:opacity-50 disabled:hover:border-border disabled:hover:bg-surface",
        FOCUS,
      )}
      data-testid="collection"
      data-collection={collection.id}
      data-active={active}
    >
      <span className="flex items-start justify-between gap-3">
        <span
          aria-hidden
          className={cx(
            "grid size-9 shrink-0 place-items-center ring-1 transition-colors",
            active
              ? "bg-synthetic-yellow text-black ring-synthetic-yellow"
              : "bg-synthetic-yellow/10 text-synthetic-yellow ring-synthetic-yellow/25",
          )}
        >
          <Icon className="size-4" />
        </span>
        <span
          className={cx(
            "inline-flex items-center gap-1 text-[11px] font-semibold tracking-[0.16em] uppercase",
            active ? "text-synthetic-yellow" : "text-muted",
          )}
        >
          {active ? <Check className="size-3.5" aria-hidden /> : null}
          {active ? "Screening" : count}
        </span>
      </span>
      <span className="block">
        <span
          className={cx("block text-base font-semibold tracking-tight", active ? "text-synthetic-yellow" : "text-ink")}
        >
          {collection.title}
        </span>
        <span className="mt-1 block text-xs text-muted">{collection.description}</span>
      </span>
      <span className="mt-auto block border-t border-border pt-3">
        <span className="block font-mono text-[11px] break-words text-ink-2" data-testid="collection-tickers">
          {members.length === 0 ? "None listed yet" : members.map((asset) => asset.ticker).join(" · ")}
        </span>
        {move ? (
          <span className="mt-1.5 flex flex-wrap items-baseline gap-x-2">
            <span
              className={cx("text-sm font-semibold tabular-nums", TREND[move.trend].text)}
              data-testid="collection-change"
            >
              {move.text}
            </span>
            <span className="text-[10px] text-muted">24h, equal weight</span>
          </span>
        ) : null}
      </span>
    </button>
  );
}

/* ------------------------------------------------------------------------ */

/** Placeholders with the highlights' footprint, while the catalog loads. */
export function DiscoverHighlightsSkeleton() {
  const card = <div className="h-36 border border-border bg-surface" />;
  return (
    <div className="grid gap-6 motion-safe:animate-pulse xl:grid-cols-5 xl:gap-4" aria-hidden>
      <div className="xl:col-span-2">
        <div className="mb-3 h-3 w-32 bg-surface-2" />
        <div className="grid grid-cols-2 gap-3 sm:gap-4">
          {card}
          {card}
        </div>
      </div>
      <div className="xl:col-span-3">
        <div className="mb-3 h-3 w-28 bg-surface-2" />
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 sm:gap-4">
          {card}
          {card}
          <div className="hidden h-36 border border-border bg-surface sm:block" />
        </div>
      </div>
    </div>
  );
}
