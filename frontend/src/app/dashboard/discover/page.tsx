"use client";

import { useCallback, useEffect, useMemo, useRef } from "react";
import { useRouter } from "next/navigation";
import { Activity, Clock, Search, SearchX } from "lucide-react";

import { useDashboard } from "@/components/layout/dashboard-shell";
import { AssetTableSkeleton } from "@/components/markets/asset-table";
import { MarketList } from "@/components/markets/market-list";
import { MarketNotice, NOTICE_ACTION, NoticeLink } from "@/components/markets/market-notice";
import { MarketSearch } from "@/components/markets/market-search";
import { Alert, Button } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import { searchCatalog } from "@/lib/discover";
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
  const searchRef = useRef<HTMLInputElement>(null);

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

  const results = useMemo(() => (catalog ? searchCatalog(catalog.assets, query) : []), [catalog, query]);
  const term = query.trim();
  const searching = term !== "";
  const closed = error instanceof ApiError && error.code === "MERCADO_NO_DISPONIBLE";
  const simulated = catalog?.assets.some((asset) => asset.price_source === "simulated") ?? false;

  const clear = () => {
    setQuery("");
    searchRef.current?.focus();
  };

  return (
    <div className="space-y-4" data-testid="discover-page">
      <MarketSearch value={query} onChange={setQuery} inputRef={searchRef} />

      {/* Read out as the results change; the list says the same on screen. */}
      <p role="status" className="sr-only" data-testid="discover-status">
        {catalog === null || !searching
          ? ""
          : results.length === 0
            ? `No assets match ${term}`
            : `${results.length} ${results.length === 1 ? "asset matches" : "assets match"} ${term}`}
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
          <AssetTableSkeleton label="Loading the market…" rows={5} />
        )
      ) : (
        <>
          {error ? (
            <Alert tone="warning">Showing the last prices received. {describeError(error)}</Alert>
          ) : null}
          {results.length > 0 ? (
            <MarketList
              assets={results}
              icon={searching ? Search : Activity}
              title={searching ? "Results" : "Trending"}
              summary={
                searching
                  ? `${results.length} ${results.length === 1 ? "match" : "matches"} for “${term}”`
                  : "Most traded in the last 24 hours"
              }
            />
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
        </>
      )}
    </div>
  );
}
