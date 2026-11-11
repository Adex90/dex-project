"use client";

import { useCallback, useEffect, useMemo, useRef } from "react";
import { useRouter } from "next/navigation";
import { Activity, Clock, FilterX, Search, SearchX, SlidersHorizontal } from "lucide-react";

import { useDashboard } from "@/components/layout/dashboard-shell";
import { AssetScreener } from "@/components/markets/asset-screener";
import { AssetTableSkeleton } from "@/components/markets/asset-table";
import { Collections, DiscoverHighlightsSkeleton, TopMovers } from "@/components/markets/discover-highlights";
import { MarketList } from "@/components/markets/market-list";
import { MarketNotice, NOTICE_ACTION, NOTICE_SECONDARY, NoticeLink } from "@/components/markets/market-notice";
import { MarketSearch } from "@/components/markets/market-search";
import { Alert, Button } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import { searchCatalog } from "@/lib/discover";
import {
  NO_FILTERS,
  applyCollection,
  classOptions,
  describeFilters,
  facetCounts,
  isFiltered,
  screen,
  topMovers,
  type Collection,
} from "@/lib/screener";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { useAuthStore } from "@/store/auth";
import { useMarketStore } from "@/store/market";

/*
 * Discover: /dashboard/discover, the market explorer, inside the dashboard
 * shell.
 *
 * - The catalog comes from GET /api/v1/market/assets: every asset that can be
 *   quoted and traded now, validated whole in the market store. The store
 *   keeps the catalog and the search between visits, so back from an asset
 *   the same results show at once while the prices refresh behind them.
 * - The search matches the ticker, the name or the asset class, accents and
 *   case aside (lib/discover.ts). With nothing typed the list is Trending:
 *   the most traded over the last 24 hours first.
 * - Above the list, the day's top movers and the collections
 *   (components/markets/discover-highlights.tsx), from the whole catalog.
 *   They step aside while a search is typed: the page is about its results.
 * - The screener (components/markets/asset-screener.tsx) narrows the list by
 *   asset class and by the direction of the 24-hour change; a collection
 *   presets its class. Search and filters combine, and the store keeps both
 *   between visits. Everything is computed here, from the catalog
 *   (lib/screener.ts): no other request.
 * - Every row opens /dashboard/asset/{ticker}. Discover stays the current tab
 *   there, and the asset's Back link returns here.
 * - Without live prices the backend answers 503 MERCADO_NO_DISPONIBLE: the
 *   page says the market is not open yet.
 */

export default function DiscoverPage() {
  const router = useRouter();
  const { token } = useDashboard();
  const catalog = useMarketStore((s) => s.catalog);
  const status = useMarketStore((s) => s.status);
  const error = useMarketStore((s) => s.error);
  const query = useMarketStore((s) => s.query);
  const setQuery = useMarketStore((s) => s.setQuery);
  const filters = useMarketStore((s) => s.filters);
  const setFilters = useMarketStore((s) => s.setFilters);
  const searchRef = useRef<HTMLInputElement>(null);
  const screenerRef = useRef<HTMLDivElement>(null);
  const resultsRef = useRef<HTMLDivElement>(null);
  /** Set by a collection: scroll to the screener once the list it narrowed is on the page. */
  const revealRef = useRef(false);

  const load = useCallback(
    () => useMarketStore.getState().load((signal) => api.marketAssets(token, signal)),
    [token],
  );

  // Every visit refreshes the prices; a list loaded before stays on screen meanwhile.
  useEffect(() => {
    void load();
  }, [load]);

  // A session the server no longer accepts signs out, as everywhere in the dashboard.
  useEffect(() => {
    if (error instanceof ApiError && error.isUnauthorized) {
      useAuthStore.getState().expire();
      router.replace(EXPIRED_LOGIN_PATH);
    }
  }, [error, router]);

  // The search first (its order), then the screener (which only keeps or drops).
  const searched = useMemo(() => (catalog ? searchCatalog(catalog.assets, query) : []), [catalog, query]);
  const results = useMemo(() => screen(searched, filters), [searched, filters]);
  const counts = useMemo(() => facetCounts(searched, filters), [searched, filters]);
  const classes = useMemo(() => classOptions(catalog?.assets ?? []), [catalog]);
  const movers = useMemo(() => topMovers(catalog?.assets ?? []), [catalog]);
  const term = query.trim();
  const searching = term !== "";
  const filtered = isFiltered(filters);
  const closed = error instanceof ApiError && error.code === "MERCADO_NO_DISPONIBLE";
  const simulated = catalog?.assets.some((asset) => asset.price_source === "simulated") ?? false;
  /** What the list answers, in words: the search, the filters, or both. */
  const asked = searching && filtered ? `${term} with these filters` : searching ? term : "these filters";

  const clear = () => {
    setQuery("");
    searchRef.current?.focus();
  };

  /** A collection sets the screener; when its results are out of sight (a phone), the page then scrolls to them. */
  const pick = (collection: Collection) => {
    revealRef.current = true;
    setFilters(applyCollection(filters, collection));
  };

  // After the narrowed list is on the page (a scroll started before would aim at the old, longer one).
  // Only when the results are out of sight: where they already show, the page stays put.
  useEffect(() => {
    if (!revealRef.current) return;
    revealRef.current = false;
    const screener = screenerRef.current;
    const results = resultsRef.current;
    if (!screener || !results || results.getBoundingClientRect().top < window.innerHeight - 96) return;
    const still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    screener.scrollIntoView({ behavior: still ? "auto" : "smooth", block: "start" });
  }, [filters]);

  return (
    <div className="space-y-4" data-testid="discover-page">
      <MarketSearch value={query} onChange={setQuery} inputRef={searchRef} />

      {/* Read out as the results change; the list says the same on screen. */}
      <p role="status" className="sr-only" data-testid="discover-status">
        {catalog === null || (!searching && !filtered)
          ? ""
          : results.length === 0
            ? `No assets match ${asked}`
            : `${results.length} ${results.length === 1 ? "asset matches" : "assets match"} ${asked}`}
      </p>

      {simulated ? (
        <p
          className="text-right text-[11px] font-semibold tracking-[0.2em] text-muted uppercase"
          data-testid="discover-simulated"
        >
          Demo · simulated prices
        </p>
      ) : null}

      {catalog === null ? (
        status === "error" ? (
          closed ? (
            <MarketNotice
              icon={Clock}
              title="The market is not open yet"
              body="Trading opens once live prices are connected. Your vault is unchanged."
              testId="discover-notice"
            >
              <NoticeLink href="/dashboard">Go to your vault</NoticeLink>
            </MarketNotice>
          ) : (
            <Alert tone="error" title="Could not load the market">
              {describeError(error)}{" "}
              <Button variant="ghost" size="sm" onClick={() => void load()}>
                Retry
              </Button>
            </Alert>
          )
        ) : (
          <>
            <DiscoverHighlightsSkeleton />
            <AssetTableSkeleton label="Loading the market…" rows={5} />
          </>
        )
      ) : (
        <>
          {error ? (
            <Alert tone="warning">Showing the last prices received. {describeError(error)}</Alert>
          ) : null}
          {searching ? null : (
            <div className="grid gap-6 xl:grid-cols-5 xl:gap-4">
              <TopMovers movers={movers} className="xl:col-span-2" />
              <Collections assets={catalog.assets} filters={filters} onPick={pick} className="xl:col-span-3" />
            </div>
          )}
          {/* scroll-mt: room for the sticky header when a collection scrolls the screener into view. */}
          <div ref={screenerRef} className="scroll-mt-24">
            <AssetScreener
              classes={classes}
              filters={filters}
              counts={counts}
              shown={results.length}
              total={searched.length}
              onChange={setFilters}
            />
          </div>
          {/* The results: the list, or what to do when nothing matches. */}
          <div ref={resultsRef}>
            {results.length > 0 ? (
              <MarketList
                assets={results}
                icon={searching ? Search : filtered ? SlidersHorizontal : Activity}
                title={searching ? "Results" : filtered ? "Screener" : "Trending"}
                summary={
                  searching
                    ? `${results.length} ${results.length === 1 ? "match" : "matches"} for “${term}”` +
                      (filtered ? ` · ${describeFilters(filters)}` : "")
                    : filtered
                      ? `${describeFilters(filters)} · most traded first`
                      : "Most traded in the last 24 hours"
                }
              />
            ) : filtered ? (
              <MarketNotice
                icon={FilterX}
                title={searching ? `No assets match “${term}” with these filters` : "No assets match these filters"}
                body={
                  searching
                    ? "Clear the filters to search the whole market, or try another search."
                    : "Prices move through the day: try another class or direction."
                }
                testId="discover-no-match"
              >
                <div className="flex flex-wrap justify-center gap-3">
                  <button type="button" onClick={() => setFilters(NO_FILTERS)} className={NOTICE_ACTION}>
                    Clear filters
                  </button>
                  {searching ? (
                    <button type="button" onClick={clear} className={NOTICE_SECONDARY}>
                      Clear search
                    </button>
                  ) : null}
                </div>
              </MarketNotice>
            ) : searching ? (
              <MarketNotice
                icon={SearchX}
                title={`No assets match “${term}”`}
                body="Try a ticker, a name or a class: gold, CETES, equity."
                testId="discover-no-match"
              >
                <button type="button" onClick={clear} className={NOTICE_ACTION}>
                  Clear search
                </button>
              </MarketNotice>
            ) : (
              <MarketNotice
                icon={Activity}
                title="Nothing is listed yet"
                body="Assets appear here as soon as they can be traded."
                testId="discover-empty"
              />
            )}
          </div>
        </>
      )}
    </div>
  );
}
