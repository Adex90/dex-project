"use client";

import { useEffect, useId, useMemo, useRef, useState, type ChangeEvent, type FormEvent } from "react";
import { ArrowDownRight, ArrowUpRight, BellRing, Check, X } from "lucide-react";

import { cx } from "@/components/ui/primitives";
import { useSignOutOn401 } from "@/hooks/use-sign-out-on-401";
import { ApiError, describeError } from "@/lib/api/client";
import { api, newIdempotencyKey } from "@/lib/api/endpoints";
import { PRICE_DECIMALS, formatAmountInput, formatPrice, priceEntry, priceForApi, sanitizeAmount } from "@/lib/market";
import { formatOrderTime } from "@/lib/order-history";
import {
  alertsCount,
  alertsFor,
  buildPriceAlert,
  directionFor,
  notifySentence,
  reachedSentence,
  targetPhrase,
} from "@/lib/price-alerts";
import { usePriceAlertsStore, visibleAlerts } from "@/store/price-alerts";
import type { PriceAlert } from "@/types/market";
import type { DecimalString } from "@/types/vault";

/*
 * Price alerts, on an asset's page: set a target price and the dashboard
 * says so when the asset gets there (the market worker checks every few
 * seconds; "Price target reached" shows at the top of any page).
 *
 * - The target's side of the current price sets the direction: above, it
 *   waits for the price to rise to it; below, to fall. The sentence under
 *   the field says which before it is set ("Notify me when AUG1 rises to
 *   $1,700.00"); a target at the current price cannot be set.
 * - Sending reuses its idempotency key while the target stays the same, so
 *   a retry never sets it twice.
 * - The list: the active targets, nearest first, each with Cancel (the row
 *   goes at once; a failure brings it back and says why), then those reached
 *   in the last 30 days with the price that reached them.
 * - Never the singular "alert" on screen (an AML word no client screen may
 *   show): "Price alerts", "price target", "Notify me".
 */

/** A confirmation stays this long. */
const CONFIRMATION_MS = 6_000;
const INVALID_TARGET = "Enter a price in pesos, with at most four decimals.";
const NOT_LISTED = "This asset is no longer listed, so a price target cannot be set.";
const MARKET_CLOSED = "Price targets can be set once live prices are connected.";

export interface PriceAlertsCardProps {
  ticker: string;
  /** The asset's current price, MXN. */
  price: DecimalString;
  token: string;
  className?: string;
}

export function PriceAlertsCard({ ticker, price, token, className }: PriceAlertsCardProps) {
  const titleId = useId();
  const inputId = useId();
  const hintId = useId();
  const signOutOn401 = useSignOutOn401();
  const all = usePriceAlertsStore((s) => s.alerts);
  const cancelling = usePriceAlertsStore((s) => s.cancelling);
  const status = usePriceAlertsStore((s) => s.status);
  const loadError = usePriceAlertsStore((s) => s.error);
  const notice = usePriceAlertsStore((s) => s.notice);
  const rows = useMemo(
    () => alertsFor(visibleAlerts({ alerts: all, cancelling }), ticker, price),
    [all, cancelling, ticker, price],
  );

  const [target, setTarget] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [made, setMade] = useState<PriceAlert | null>(null);
  // The same target retried keeps its key; another target is a new request.
  const attempt = useRef<{ target: DecimalString; key: string } | null>(null);

  const direction = directionFor(target, price);
  const sentence = notifySentence(ticker, target, price);
  const atPrice = target !== "" && sentence !== null && direction === null;

  useEffect(() => {
    if (made === null) return;
    const timer = setTimeout(() => setMade(null), CONFIRMATION_MS);
    return () => clearTimeout(timer);
  }, [made]);

  function onType(event: ChangeEvent<HTMLInputElement>) {
    const next = sanitizeAmount(event.target.value, PRICE_DECIMALS);
    if (next === null) return;
    setTarget(next);
    setError(null);
    setMade(null);
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (direction === null || busy) return;
    const exact = priceForApi(target);
    if (attempt.current?.target !== exact) attempt.current = { target: exact, key: newIdempotencyKey() };
    const request = buildPriceAlert(ticker, target, price, attempt.current.key);
    if (request === null) return;
    setBusy(true);
    setError(null);
    try {
      const alert = await usePriceAlertsStore
        .getState()
        .create(() => api.createPriceAlert(token, request).catch(signOutOn401));
      attempt.current = null;
      setTarget("");
      setMade(alert);
    } catch (cause) {
      // The server answered and set nothing: the next try is a new request.
      if (cause instanceof ApiError && cause.status >= 400 && cause.status < 500) attempt.current = null;
      setError(cause);
    } finally {
      setBusy(false);
    }
  }

  const cancel = (alert: PriceAlert) =>
    void usePriceAlertsStore
      .getState()
      .cancel(alert.alert_id, (id) => api.cancelPriceAlert(token, id).catch(signOutOn401));

  const retry = () =>
    void usePriceAlertsStore.getState().load((signal) => api.priceAlerts(token, signal).catch(signOutOn401));

  return (
    <section
      aria-labelledby={titleId}
      className={cx("mt-4 border border-border bg-surface sm:mt-6", className)}
      data-testid="price-alerts"
    >
      <header className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-5 py-4">
        <div className="flex min-w-0 items-center gap-3">
          <span
            aria-hidden
            className="grid size-9 shrink-0 place-items-center border border-synthetic-yellow/40 bg-synthetic-yellow/10 text-synthetic-yellow"
          >
            <BellRing className="size-4" />
          </span>
          <div className="min-w-0">
            <h3 id={titleId} className="text-base font-semibold tracking-tight text-ink">
              Price alerts
            </h3>
            <p className="text-xs text-muted">
              Choose a price. When {ticker} reaches it, a notice appears on your dashboard.
            </p>
          </div>
        </div>
        <p
          className="text-[11px] font-semibold tracking-[0.18em] text-muted uppercase"
          data-testid="price-alerts-count"
        >
          {alertsCount(rows)}
        </p>
      </header>

      <form onSubmit={submit} className="px-5 py-4" noValidate>
        <div className="flex items-baseline justify-between gap-3">
          <label htmlFor={inputId} className="text-[11px] font-semibold tracking-[0.24em] text-muted uppercase">
            Target price
          </label>
          <span className="text-xs text-muted tabular-nums">Now {formatPrice(price)}</span>
        </div>
        <div className="mt-2 grid gap-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-end">
          <div className="flex items-baseline gap-1.5 border-b-2 border-border pb-1 transition-colors focus-within:border-synthetic-yellow">
            <span aria-hidden className="text-xl font-semibold text-synthetic-yellow">
              $
            </span>
            <input
              id={inputId}
              value={target === "" ? "" : formatAmountInput(target)}
              onChange={onType}
              placeholder={formatAmountInput(priceEntry(price))}
              inputMode="decimal"
              autoComplete="off"
              spellCheck={false}
              aria-describedby={hintId}
              aria-invalid={atPrice || undefined}
              readOnly={busy}
              className="min-w-0 flex-1 bg-transparent p-0 font-mono text-2xl font-semibold tracking-tight text-ink tabular-nums caret-synthetic-yellow outline-none placeholder:text-muted/50"
              data-testid="price-alert-target"
            />
            <span className="text-xs font-semibold tracking-[0.16em] text-muted">MXN</span>
          </div>
          <button
            type="submit"
            disabled={direction === null || busy}
            className={cx(
              "inline-flex h-12 items-center justify-center gap-2 bg-synthetic-yellow px-6 text-sm font-bold tracking-[0.2em] text-black uppercase transition-colors",
              "hover:bg-synthetic-yellow-hover active:bg-synthetic-yellow-pressed",
              "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
              "disabled:cursor-not-allowed disabled:opacity-40",
            )}
            data-testid="price-alert-submit"
          >
            {busy ? "Setting…" : "Notify me"}
          </button>
        </div>
        <p
          id={hintId}
          className={cx("mt-2 text-xs", atPrice ? "font-medium text-danger" : "text-muted")}
          data-testid="price-alert-hint"
        >
          {sentence ?? "Above the current price, it waits for a rise; below it, for a fall."}
        </p>
        {error !== null ? (
          <p role="alert" className="mt-2 text-sm text-danger" data-testid="price-alert-error">
            {describeTargetError(error)}
          </p>
        ) : null}
        {made !== null ? (
          <p
            role="status"
            className="mt-2 inline-flex items-center gap-1.5 text-sm text-ink-2"
            data-testid="price-alert-made"
          >
            <Check className="size-4 text-synthetic-yellow" aria-hidden />
            Set: {targetPhrase(made).toLowerCase()}. It shows on your dashboard when it is reached.
          </p>
        ) : null}
      </form>

      {notice !== null && notice.alert.ticker === ticker ? (
        <div
          className="flex items-start gap-2.5 border-t border-danger/40 bg-danger-soft px-5 py-3 text-sm text-danger"
          data-testid="price-alerts-notice"
        >
          <X className="mt-0.5 size-4 shrink-0" aria-hidden />
          <p role="alert" className="min-w-0 flex-1">
            Could not cancel the price target {targetPhrase(notice.alert).toLowerCase()}. {describeError(notice.error)}
          </p>
          <button
            type="button"
            onClick={() => usePriceAlertsStore.getState().dismissNotice()}
            aria-label="Dismiss"
            className="grid size-6 shrink-0 place-items-center text-danger/80 transition-colors hover:text-danger focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
          >
            <X className="size-3.5" aria-hidden />
          </button>
        </div>
      ) : null}

      {status === "error" && rows.length === 0 ? (
        <p className="border-t border-border px-5 py-3 text-sm text-danger" role="alert">
          Your price targets could not be loaded. {describeError(loadError)}{" "}
          <button type="button" onClick={retry} className="font-semibold underline underline-offset-4">
            Retry
          </button>
        </p>
      ) : rows.length > 0 ? (
        <ul
          aria-label={`Your price targets for ${ticker}`}
          className="divide-y divide-border border-t border-border"
          data-testid="price-alerts-list"
        >
          {rows.map((alert) => (
            <TargetRow key={alert.alert_id} alert={alert} onCancel={cancel} />
          ))}
        </ul>
      ) : null}
    </section>
  );
}

/* ------------------------------------------------------------------------ */

function describeTargetError(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.code === "VALIDACION") return INVALID_TARGET;
    if (error.status === 404) return NOT_LISTED;
    if (error.code === "MERCADO_NO_DISPONIBLE") return MARKET_CLOSED;
  }
  return describeError(error);
}

function TargetRow({ alert, onCancel }: { alert: PriceAlert; onCancel: (alert: PriceAlert) => void }) {
  const reached = alert.status === "triggered";
  const Icon = reached ? Check : alert.direction === "above" ? ArrowUpRight : ArrowDownRight;
  const phrase = targetPhrase(alert);

  return (
    <li
      className="flex items-center gap-3 px-5 py-3"
      data-testid="price-alert"
      data-alert-id={alert.alert_id}
      data-status={alert.status}
    >
      <span
        aria-hidden
        className={cx(
          "grid size-8 shrink-0 place-items-center border",
          reached
            ? "border-synthetic-yellow bg-synthetic-yellow text-black"
            : "border-synthetic-yellow/40 text-synthetic-yellow",
        )}
      >
        <Icon className="size-4" />
      </span>
      <div className="min-w-0 flex-1">
        <p className="flex flex-wrap items-center gap-x-2 text-sm font-medium text-ink tabular-nums">
          {phrase}
          {reached ? (
            <span className="border border-synthetic-yellow/50 bg-synthetic-yellow/10 px-1.5 py-px text-[10px] font-semibold tracking-[0.16em] text-synthetic-yellow uppercase">
              Reached
            </span>
          ) : null}
        </p>
        <p className="text-xs text-muted tabular-nums">
          {reached
            ? `${reachedSentence(alert)} · ${formatOrderTime(alert.triggered_at ?? alert.created_at)}`
            : `Set at ${formatPrice(alert.reference_price)} · ${formatOrderTime(alert.created_at)}`}
        </p>
      </div>
      {reached ? null : (
        <button
          type="button"
          onClick={() => onCancel(alert)}
          aria-label={`Cancel the price target: ${phrase.toLowerCase()}`}
          className={cx(
            "inline-flex h-9 shrink-0 items-center gap-1.5 border border-border px-2.5",
            "text-[11px] font-semibold tracking-[0.16em] text-muted uppercase transition-colors",
            "hover:border-danger/60 hover:bg-danger-soft hover:text-danger",
            "focus-visible:ring-2 focus-visible:ring-danger focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
          )}
          data-testid="price-alert-cancel"
        >
          <X className="size-3.5" aria-hidden />
          Cancel
        </button>
      )}
    </li>
  );
}
