"use client";

import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Check, Info, Pause, Play, Repeat, X } from "lucide-react";

import { TABLE_CARD, TickerChip } from "@/components/markets/asset-table";
import { cx } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import { formatMoney, isZero } from "@/lib/decimal";
import { assetPath } from "@/lib/market-data";
import {
  FREQUENCY_LABELS,
  actionSentence,
  formatPlanDay,
  lastPurchaseSentence,
  monthlyTotal,
  nextPurchaseLabel,
  plansCount,
} from "@/lib/savings-plans";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { formatShares } from "@/lib/vault";
import { useAuthStore } from "@/store/auth";
import { useMarketStore } from "@/store/market";
import { useSavingsPlansStore, visiblePlans, type PlansNotice } from "@/store/savings-plans";
import { useVaultStore } from "@/store/vault";
import type { PlanAction, SavingsPlan } from "@/types/market";

/*
 * Savings plans: the client's recurring purchases
 * (GET /api/v1/market/savings-plans) on the Vault, beside Open orders, in the
 * Sovereign Gold identity: a terminal-style table on the charcoal card, gold
 * for what runs next, quiet zinc actions.
 *
 * - Each row: the ticker (it opens the asset's page), what it buys and how
 *   often ("$500.00 weekly"), its next purchase in Mexico City time (or
 *   "Paused"), the last purchase (bought, or skipped and why, in neutral
 *   words), and what it has invested and bought so far. The header counts
 *   active and paused plans and adds up what the active ones invest in an
 *   average month.
 * - Pause and Resume are instant and optimistic (store/savings-plans.ts):
 *   the row changes at once and the action goes to
 *   POST /api/v1/market/savings-plans/{id}/{pause|resume}; a failure brings
 *   it back and says why. Cancel is final, so it asks once more in place
 *   ("Cancel plan? Yes, cancel · Keep") before it goes.
 * - The list reloads whenever the vault moves (the stream's new version):
 *   each purchase, made or skipped, and each change bumps it, so a purchase
 *   shows up as it happens and a plan created on another device appears.
 * - With no plans it renders nothing, unless it has something to say (an
 *   action just confirmed, or a list that could not load). The first load
 *   shows nothing either, so the Vault never jumps for clients without plans.
 */

/** A confirmation stays this long; an error stays until dismissed or replaced. */
const NOTICE_MS = 6_000;
/** How often the dates' "In a moment" and their year are checked against the clock. */
const CLOCK_MS = 15_000;

export function SavingsPlansList({ token, className }: { token: string; className?: string }) {
  const titleId = useId();
  const router = useRouter();
  const all = useSavingsPlansStore((s) => s.plans);
  const changing = useSavingsPlansStore((s) => s.changing);
  const status = useSavingsPlansStore((s) => s.status);
  const error = useSavingsPlansStore((s) => s.error);
  const notice = useSavingsPlansStore((s) => s.notice);
  const vaultMoved = useVaultStore((s) => s.updatedAt);
  const catalog = useMarketStore((s) => s.catalog);
  const catalogStatus = useMarketStore((s) => s.status);
  const plans = useMemo(() => visiblePlans({ plans: all, changing }), [all, changing]);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());

  // A 401 ends the session, as on every dashboard page.
  const endSession = useRef(() => {
    useAuthStore.getState().expire();
    router.replace(EXPIRED_LOGIN_PATH);
  });
  const signOutOn401 = useCallback((error: unknown): never => {
    if (error instanceof ApiError && error.isUnauthorized) endSession.current();
    throw error;
  }, []);

  const load = useCallback(
    () => useSavingsPlansStore.getState().load((signal) => api.savingsPlans(token, signal).catch(signOutOn401)),
    [token, signOutOn401],
  );

  // On mount, and whenever the vault moves: a purchase made or skipped, a plan created or changed elsewhere.
  useEffect(() => {
    void load();
  }, [load, vaultMoved]);

  // The next purchase reads "In a moment" once due: the clock moves on its own.
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), CLOCK_MS);
    return () => clearInterval(timer);
  }, []);

  // Each asset's quantity increment, so what a plan bought reads like its fills (Discover may have loaded it already).
  const hasPlans = plans.length > 0;
  useEffect(() => {
    if (hasPlans && catalogStatus === "idle") {
      void useMarketStore.getState().load((signal) => api.marketAssets(token, signal));
    }
  }, [hasPlans, catalogStatus, token]);

  useEffect(() => {
    if (notice === null || notice.kind === "failed") return;
    const timer = setTimeout(() => useSavingsPlansStore.getState().dismissNotice(), NOTICE_MS);
    return () => clearTimeout(timer);
  }, [notice]);

  const act = useCallback(
    (plan: SavingsPlan, action: PlanAction) => {
      setConfirming(null);
      void useSavingsPlansStore
        .getState()
        .act(plan.plan_id, action, (id, a) => api.changeSavingsPlan(token, id, a).catch(signOutOn401));
    },
    [token, signOutOn401],
  );

  const decimalsFor = (ticker: string) =>
    catalog?.assets.find((asset) => asset.ticker === ticker)?.quantity_decimals ?? 4;
  const monthly = monthlyTotal(plans);

  if (plans.length === 0 && notice === null) {
    if (status === "error") {
      return (
        <section className={cx(TABLE_CARD, "px-5 py-4 sm:px-6", className)} data-testid="savings-plans-error">
          <p role="alert" className="text-sm text-danger">
            Could not load your savings plans. {describeError(error)}
          </p>
          <button type="button" onClick={() => void load()} className={cx(QUIET_BUTTON, "mt-3")}>
            Retry
          </button>
        </section>
      );
    }
    return null;
  }

  return (
    <section
      id="savings-plans"
      aria-labelledby={titleId}
      className={cx(TABLE_CARD, "scroll-mt-24", className)}
      data-testid="savings-plans"
    >
      <header className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-5 py-4 sm:px-6">
        <div className="flex min-w-0 items-center gap-3">
          <span
            aria-hidden
            className="grid size-10 shrink-0 place-items-center rounded-xl bg-synthetic-yellow/10 text-synthetic-yellow ring-1 ring-synthetic-yellow/25"
          >
            <Repeat className="size-[18px]" />
          </span>
          <div className="min-w-0">
            <h2 id={titleId} className="text-base font-semibold tracking-tight">
              Savings plans
            </h2>
            <p className="text-xs text-muted" data-testid="savings-plans-count">
              {plansCount(plans)}
            </p>
          </div>
        </div>
        {!isZero(monthly) ? (
          <div className="text-right">
            <p className="text-[11px] font-medium tracking-[0.18em] text-muted uppercase">About a month</p>
            <p className="text-lg font-medium tracking-tight text-ink tabular-nums" data-testid="savings-plans-monthly">
              {formatMoney(monthly)}
            </p>
          </div>
        ) : null}
      </header>

      {notice ? <Notice notice={notice} now={now} /> : null}

      {plans.length > 0 ? (
        <table className="w-full border-collapse text-sm">
          <caption className="sr-only">
            Your savings plans: what each one buys and how often, its next purchase, and what it has bought
          </caption>
          <thead>
            <tr className="text-[11px] font-medium tracking-[0.16em] text-muted uppercase">
              <th scope="col" className="px-5 py-2.5 text-left font-medium sm:px-6">
                Plan
              </th>
              <th scope="col" className="hidden px-3 py-2.5 text-left font-medium lg:table-cell">
                Next purchase
              </th>
              <th scope="col" className="hidden px-3 py-2.5 text-right font-medium sm:table-cell">
                Invested
              </th>
              <th scope="col" className="px-5 py-2.5 text-right font-medium sm:px-6">
                <span className="sr-only">Actions</span>
              </th>
            </tr>
          </thead>
          <tbody className="divide-y divide-border border-t border-border">
            {plans.map((plan) => (
              <PlanRow
                key={plan.plan_id}
                plan={plan}
                decimals={decimalsFor(plan.ticker)}
                now={now}
                busy={changing[plan.plan_id] !== undefined}
                confirming={confirming === plan.plan_id}
                onAct={act}
                onAskCancel={() => setConfirming(plan.plan_id)}
                onKeep={() => setConfirming(null)}
              />
            ))}
          </tbody>
        </table>
      ) : null}
    </section>
  );
}

/* ------------------------------------------------------------------------ */

const QUIET_BUTTON = cx(
  "inline-flex h-10 items-center gap-1.5 border border-border px-3",
  "text-[11px] font-semibold tracking-[0.18em] text-muted uppercase transition-colors",
  "hover:border-border-strong hover:text-ink",
  "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
);

const ROW_BUTTON = cx(
  "inline-flex h-10 items-center justify-center gap-1.5 border border-border px-2.5 sm:px-3",
  "text-[11px] font-semibold tracking-[0.14em] text-muted uppercase transition-colors sm:tracking-[0.18em]",
  "focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
  "disabled:cursor-not-allowed disabled:opacity-40",
);

/** "$500.00 weekly", "$500.00 every 2 weeks", "$1,000.00 monthly". */
function cadence(plan: SavingsPlan): string {
  return `${formatMoney(plan.amount)} ${FREQUENCY_LABELS[plan.frequency].toLowerCase()}`;
}

function PlanRow({
  plan,
  decimals,
  now,
  busy,
  confirming,
  onAct,
  onAskCancel,
  onKeep,
}: {
  plan: SavingsPlan;
  decimals: number;
  now: number;
  busy: boolean;
  confirming: boolean;
  onAct: (plan: SavingsPlan, action: PlanAction) => void;
  onAskCancel: () => void;
  onKeep: () => void;
}) {
  const keepRef = useRef<HTMLButtonElement>(null);
  const paused = plan.status === "paused";
  const next = nextPurchaseLabel(plan, now);
  // Resumed a moment ago: the server is computing its next date.
  const nextText = paused ? "Paused" : (next ?? "Scheduling…");
  const then = paused ? [] : plan.upcoming.slice(1).map(formatPlanDay);
  const last = lastPurchaseSentence(plan, decimals);
  const skipped = plan.last_result === "skipped";
  const bought = `${formatShares(plan.quantity, decimals)} ${plan.ticker}`;
  const counts = `${plan.purchases} ${plan.purchases === 1 ? "purchase" : "purchases"}${
    plan.skipped > 0 ? ` · ${plan.skipped} skipped` : ""
  }`;
  const what = `the ${cadence(plan)} plan of ${plan.ticker}`;

  // Asked to confirm the cancellation: "Keep" takes the focus, the safe answer.
  useEffect(() => {
    if (confirming) keepRef.current?.focus();
  }, [confirming]);

  return (
    <tr
      data-testid="savings-plan"
      data-plan-id={plan.plan_id}
      data-ticker={plan.ticker}
      data-status={plan.status}
      aria-busy={busy || undefined}
    >
      {/* The width the other columns leave (w-full max-w-0): a long line wraps or truncates, never widens a phone. */}
      <th scope="row" className="w-full max-w-0 py-3.5 pr-2 pl-5 text-left font-normal sm:px-6">
        <div className="flex min-w-0 items-start gap-3">
          <Link
            href={assetPath(plan.ticker)}
            aria-label={`${plan.ticker}: open the asset`}
            className="shrink-0 focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none"
          >
            <TickerChip ticker={plan.ticker} />
          </Link>
          <span className="min-w-0">
            <span className="flex flex-wrap items-center gap-x-2 gap-y-1">
              <span className="font-medium text-ink tabular-nums" data-testid="savings-plan-cadence">
                {cadence(plan)}
              </span>
              {paused ? (
                <span className="inline-flex h-5 items-center border border-border px-1.5 text-[10px] font-semibold tracking-[0.16em] text-muted uppercase">
                  Paused
                </span>
              ) : null}
            </span>
            {/* The next purchase has its own column from lg up. */}
            <span className="mt-0.5 block text-xs text-ink-2 tabular-nums lg:hidden" data-testid="savings-plan-next">
              {paused ? "No purchases until you resume it" : `Next ${nextText}`}
            </span>
            {last ? (
              <span
                className={cx("mt-0.5 flex items-start gap-1 text-xs", skipped ? "text-ink-2" : "text-muted")}
                data-testid="savings-plan-last"
              >
                {skipped ? <Info className="mt-px size-3 shrink-0 text-synthetic-yellow" aria-hidden /> : null}
                <span className="min-w-0">{last}</span>
              </span>
            ) : null}
            {/* What it has invested has its own column from sm up. */}
            <span className="mt-1 block text-xs text-ink-2 tabular-nums sm:hidden">
              Invested {formatMoney(plan.invested)} · {bought}
            </span>
          </span>
        </div>
      </th>
      <td className="hidden px-3 py-3.5 align-top whitespace-nowrap lg:table-cell">
        <span
          className={cx("block font-medium tabular-nums", paused ? "text-muted" : "text-synthetic-yellow")}
          data-testid="savings-plan-next-column"
        >
          {nextText}
        </span>
        {then.length > 0 ? (
          <span className="block text-xs text-muted tabular-nums">Then {then.join(" · ")}</span>
        ) : paused ? (
          <span className="block text-xs text-muted">Until you resume it</span>
        ) : null}
      </td>
      <td className="hidden px-3 py-3.5 text-right align-top whitespace-nowrap sm:table-cell">
        <span className="block font-medium text-ink tabular-nums" data-testid="savings-plan-invested">
          {formatMoney(plan.invested)}
        </span>
        <span className="block text-xs text-muted tabular-nums">{bought}</span>
        <span className="block text-xs text-muted tabular-nums">{counts}</span>
      </td>
      <td className="py-3.5 pr-5 pl-1 text-right align-top sm:px-6">
        {confirming ? (
          <div
            role="group"
            aria-label={`Cancel ${what}? This cannot be undone.`}
            className="ml-auto flex w-max flex-col items-end gap-1.5"
          >
            <span className="text-xs font-medium text-danger">Cancel for good?</span>
            <div className="flex gap-1.5">
              <button
                ref={keepRef}
                type="button"
                onClick={onKeep}
                className={cx(
                  ROW_BUTTON,
                  "hover:border-border-strong hover:text-ink focus-visible:ring-synthetic-yellow",
                )}
                data-testid="savings-plan-cancel-keep"
              >
                Keep
              </button>
              <button
                type="button"
                onClick={() => onAct(plan, "cancel")}
                className={cx(
                  ROW_BUTTON,
                  "border-danger/60 bg-danger-soft text-danger hover:bg-danger hover:text-white focus-visible:ring-danger",
                )}
                data-testid="savings-plan-cancel-confirm"
              >
                Yes, cancel
              </button>
            </div>
          </div>
        ) : (
          // Stacked on a phone, both as wide as the wider one.
          <div className="ml-auto flex w-max flex-col items-stretch gap-1.5 sm:flex-row">
            {paused ? (
              <button
                type="button"
                onClick={() => onAct(plan, "resume")}
                disabled={busy}
                aria-label={`Resume ${what}`}
                className={cx(
                  ROW_BUTTON,
                  "hover:border-synthetic-yellow/60 hover:text-synthetic-yellow focus-visible:ring-synthetic-yellow",
                )}
                data-testid="savings-plan-resume"
              >
                <Play className="size-3.5" aria-hidden />
                Resume
              </button>
            ) : (
              <button
                type="button"
                onClick={() => onAct(plan, "pause")}
                disabled={busy}
                aria-label={`Pause ${what}`}
                className={cx(
                  ROW_BUTTON,
                  "hover:border-border-strong hover:text-ink focus-visible:ring-synthetic-yellow",
                )}
                data-testid="savings-plan-pause"
              >
                <Pause className="size-3.5" aria-hidden />
                Pause
              </button>
            )}
            <button
              type="button"
              onClick={onAskCancel}
              disabled={busy}
              aria-label={`Cancel ${what}`}
              className={cx(
                ROW_BUTTON,
                "hover:border-danger/60 hover:bg-danger-soft hover:text-danger focus-visible:ring-danger",
              )}
              data-testid="savings-plan-cancel"
            >
              <X className="size-3.5" aria-hidden />
              Cancel
            </button>
          </div>
        )}
      </td>
    </tr>
  );
}

/** What the last action did. Announced politely; an error persists until dismissed. */
function Notice({ notice, now }: { notice: PlansNotice; now: number }) {
  const failed = notice.kind === "failed";
  const Icon = notice.kind === "done" ? Check : failed ? X : Info;
  const text =
    notice.kind === "done"
      ? actionSentence(notice.plan, notice.action, now)
      : notice.kind === "gone"
        ? `The savings plan for ${notice.plan.ticker} is no longer active: it was cancelled.`
        : `Could not ${notice.action} the savings plan for ${notice.plan.ticker}. ${describeError(notice.error)}`;
  return (
    <div
      className={cx(
        "flex items-start gap-2.5 border-b px-5 py-3 text-sm sm:px-6",
        failed ? "border-danger/40 bg-danger-soft text-danger" : "border-border bg-bg text-ink-2",
      )}
      data-testid="savings-plans-notice"
      data-kind={notice.kind}
    >
      <Icon className={cx("mt-0.5 size-4 shrink-0", failed ? "text-danger" : "text-synthetic-yellow")} aria-hidden />
      <p role={failed ? "alert" : "status"} className="min-w-0 flex-1">
        {text}
      </p>
      <button
        type="button"
        onClick={() => useSavingsPlansStore.getState().dismissNotice()}
        aria-label="Dismiss"
        className="grid size-6 shrink-0 place-items-center text-muted transition-colors hover:text-ink focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
      >
        <X className="size-3.5" aria-hidden />
      </button>
    </div>
  );
}
