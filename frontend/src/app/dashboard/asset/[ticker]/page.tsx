"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import { useParams, useRouter } from "next/navigation";
import { ArrowLeft, CircleSlash, Clock } from "lucide-react";

import { useDashboard } from "@/components/layout/dashboard-shell";
import { AssetDetail, AssetDetailSkeleton } from "@/components/markets/asset-detail";
import { Alert, Button } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import { isTicker, parseMarketQuote, parseOrderReceipt, positionFor } from "@/lib/market-data";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { useAuthStore } from "@/store/auth";
import { useVaultStore } from "@/store/vault";
import type { MarketQuote, OrderReceipt, OrderRequest } from "@/types/market";

/*
 * One asset, full page: /dashboard/asset/[ticker], inside the dashboard shell
 * (navigation, header and the vault's live WebSocket keep running).
 *
 * - The quote (GET /api/v1/market/quote/{ticker}) is fetched on load and
 *   validated whole before anything renders it.
 * - The cash and the position come from the vault store, which the shell
 *   keeps current. After an order this page never touches the store: the
 *   order's commit raises the vault's `seq`, the WebSocket pushes the new
 *   snapshot, and the cash and the position on screen follow from it (the
 *   shell's REST polling covers a stream that is down).
 * - Orders go to POST /api/v1/market/order with the checkout drawer's
 *   idempotency key, which the drawer reuses on a retry, so a retry never
 *   fills twice.
 */

type QuoteResult =
  | { status: "ready"; quote: MarketQuote }
  | { status: "missing" }
  | { status: "unavailable" }
  | { status: "error"; error: unknown };

type QuoteState = QuoteResult | { status: "loading" };

const UNREADABLE_QUOTE = "The quote could not be read. Please try again.";
const UNREADABLE_RECEIPT =
  "The order was sent, but its receipt could not be read. Check your vault before trying again: retrying this same order cannot fill it twice.";
const INVALID_AMOUNT = "Enter an amount in pesos, with at most two decimals.";

export default function AssetPage() {
  const params = useParams<{ ticker: string }>();
  const ticker = params.ticker ?? "";
  const router = useRouter();
  const { token, refresh } = useDashboard();
  const balance = useVaultStore((s) => s.balance);
  const holdings = useVaultStore((s) => s.holdings);
  const vaultStatus = useVaultStore((s) => s.status);
  const vaultError = useVaultStore((s) => s.error);

  // A request per (ticker, attempt): until its result arrives the page is loading.
  const [attempt, setAttempt] = useState(0);
  const key = `${ticker}#${attempt}`;
  const [result, setResult] = useState<{ key: string; value: QuoteResult } | null>(null);
  const quote: QuoteState = !isTicker(ticker)
    ? { status: "missing" }
    : result?.key === key
      ? result.value
      : { status: "loading" };

  const endSession = useCallback(() => {
    useAuthStore.getState().expire();
    router.replace(EXPIRED_LOGIN_PATH);
  }, [router]);

  useEffect(() => {
    if (!isTicker(ticker)) return;
    const controller = new AbortController();
    const settle = (value: QuoteResult) => {
      if (!controller.signal.aborted) setResult({ key, value });
    };
    api.marketQuote(token, ticker, controller.signal).then(
      (body) => {
        const parsed = parseMarketQuote(body);
        settle(
          parsed && parsed.asset.ticker === ticker
            ? { status: "ready", quote: parsed }
            : { status: "error", error: new Error(UNREADABLE_QUOTE) },
        );
      },
      (error: unknown) => {
        if (controller.signal.aborted) return;
        if (error instanceof ApiError) {
          if (error.isUnauthorized) return endSession();
          if (error.status === 404) return settle({ status: "missing" });
          if (error.code === "MERCADO_NO_DISPONIBLE") return settle({ status: "unavailable" });
        }
        settle({ status: "error", error });
      },
    );
    return () => controller.abort();
  }, [token, ticker, key, endSession]);

  const placeOrder = useCallback(
    async (order: OrderRequest): Promise<OrderReceipt> => {
      let body: unknown;
      try {
        body = await api.placeOrder(token, order);
      } catch (error) {
        if (error instanceof ApiError && error.isUnauthorized) endSession();
        // The drawer validates the amount before sending: a 400 means it did not
        // travel as pesos and cents. Say so in English, not in the server's words.
        if (error instanceof ApiError && error.code === "VALIDACION") throw new Error(INVALID_AMOUNT);
        throw error;
      }
      const receipt = parseOrderReceipt(body);
      // A 2xx means the order went through: never claim a failure that may be false.
      if (!receipt || receipt.ticker !== order.ticker || receipt.side !== order.side) {
        throw new Error(UNREADABLE_RECEIPT);
      }
      return receipt;
    },
    [token, endSession],
  );

  const vaultLoaded = balance !== null;

  return (
    <div className="space-y-4" data-testid="asset-page">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <Link
          href="/dashboard"
          className="inline-flex h-9 items-center gap-2 border border-border px-3 text-[11px] font-semibold tracking-[0.18em] text-ink-2 uppercase transition-colors hover:border-synthetic-yellow/60 hover:text-synthetic-yellow focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-bg focus-visible:outline-none"
          data-testid="asset-back"
        >
          <ArrowLeft className="size-4" aria-hidden />
          Back to vault
        </Link>
        {quote.status === "ready" && quote.quote.price_source === "simulated" ? (
          <p className="text-[11px] font-semibold tracking-[0.2em] text-muted uppercase" data-testid="asset-simulated">
            Demo · simulated prices
          </p>
        ) : null}
      </div>

      {quote.status === "missing" ? (
        <Notice
          icon={CircleSlash}
          title="No such asset"
          body={isTicker(ticker) ? `There is no asset with the ticker ${ticker}.` : "This address does not name an asset."}
        />
      ) : quote.status === "unavailable" ? (
        <Notice
          icon={Clock}
          title="The market is not open yet"
          body="Trading opens once live prices are connected. Your vault is unchanged."
        />
      ) : quote.status === "error" ? (
        <Alert tone="error" title="Could not load this asset">
          {describeError(quote.error)}{" "}
          <Button variant="ghost" size="sm" onClick={() => setAttempt((n) => n + 1)}>
            Retry
          </Button>
        </Alert>
      ) : vaultStatus === "error" && !vaultLoaded ? (
        <Alert tone="error" title="Could not load your vault">
          {describeError(vaultError)}{" "}
          <Button variant="ghost" size="sm" onClick={() => void refresh()}>
            Retry
          </Button>
        </Alert>
      ) : quote.status === "loading" || !vaultLoaded ? (
        <AssetDetailSkeleton />
      ) : (
        <AssetDetail
          asset={quote.quote.asset}
          history={quote.quote.history}
          now={Date.parse(quote.quote.as_of)}
          availableCash={balance.available_cash}
          position={positionFor(holdings, quote.quote.asset.ticker)}
          onPlaceOrder={placeOrder}
        />
      )}
    </div>
  );
}

/** A full-width state in place of the asset: not found, or the market closed. */
function Notice({ icon: Icon, title, body }: { icon: typeof Clock; title: string; body: string }) {
  return (
    <section className="flex flex-col items-center border border-border bg-surface px-6 py-14 text-center" data-testid="asset-notice">
      <span
        aria-hidden
        className="grid size-12 place-items-center border border-dashed border-synthetic-yellow/40 text-synthetic-yellow"
      >
        <Icon className="size-5" />
      </span>
      <h2 className="mt-4 text-base font-semibold tracking-tight text-ink">{title}</h2>
      <p className="mt-1 max-w-sm text-sm text-muted">{body}</p>
      <Link
        href="/dashboard"
        className="mt-6 inline-flex h-11 items-center justify-center bg-synthetic-yellow px-5 text-xs font-bold tracking-[0.2em] text-black uppercase transition-colors hover:bg-synthetic-yellow-hover focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none active:bg-synthetic-yellow-pressed"
      >
        Go to your vault
      </Link>
    </section>
  );
}
