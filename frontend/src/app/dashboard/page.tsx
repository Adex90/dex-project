"use client";

import { useMemo } from "react";
import Link from "next/link";
import { Lock } from "lucide-react";

import { useDashboard } from "@/components/layout/dashboard-shell";
import { OpenOrdersList } from "@/components/markets/open-orders-list";
import { Alert, Button } from "@/components/ui/primitives";
import { AssetList, AssetListSkeleton } from "@/components/vault/asset-list";
import { PortfolioAnalytics, PortfolioAnalyticsSkeleton } from "@/components/vault/portfolio-analytics";
import { PortfolioChart } from "@/components/vault/portfolio-chart";
import { SovereignBalance, SovereignBalanceSkeleton } from "@/components/vault/sovereign-balance";
import { SpeiDepositCard, SpeiDepositCardSkeleton } from "@/components/vault/spei-deposit-card";
import { describeError } from "@/lib/api/client";
import { addUnits } from "@/lib/decimal";
import { cashBalanceHistory } from "@/lib/portfolio";
import { useDashboardStore } from "@/store/dashboard";
import { useVaultStore } from "@/store/vault";

/*
 * Vault: the dashboard's home. The Sovereign Vault snapshot
 * (GET /api/v1/vault) feeds the balance, the reserve seal, the allocation,
 * the assets and the deposit details; the chart redraws the cash balance from
 * the IFPE ledger the shell already loads. The allocation
 * (components/vault/portfolio-analytics.tsx) splits the total between cash,
 * each holding and fixed term, live from the same store. Open limit and stop
 * orders sit at the top of the holdings, with what they hold
 * (components/markets/open-orders-list.tsx); the section is absent when there
 * are none.
 *
 * Layout: one column on phones (balance, allocation, trend, deposit, open
 * orders, assets); from xl, the balance (two thirds) beside the deposit card,
 * both the same height, then the allocation, the trend, the open orders and
 * the assets across the full width.
 */
export default function VaultPage() {
  const { token, client, kycLocked, refresh } = useDashboard();
  const balance = useVaultStore((s) => s.balance);
  const reserveProof = useVaultStore((s) => s.reserveProof);
  const holdings = useVaultStore((s) => s.holdings);
  const account = useVaultStore((s) => s.account);
  const status = useVaultStore((s) => s.status);
  const error = useVaultStore((s) => s.error);
  const cashAccount = useDashboardStore((s) => s.summary?.boveda ?? null);
  const movements = useDashboardStore((s) => s.movements);
  const lastUpdated = useDashboardStore((s) => s.lastUpdated);

  // The cash balance and its movements come from the same refresh, so the
  // history rebuilt backwards from that balance is exact.
  const history = useMemo(() => {
    if (!cashAccount || lastUpdated === null) return null;
    // All the account's cash: available, on hold and held for open orders.
    const cash = addUnits(cashAccount.saldo_disponible, cashAccount.saldo_bloqueado, cashAccount.saldo_comprometido ?? "0");
    return cashBalanceHistory(movements, cashAccount.id, cash, lastUpdated);
  }, [cashAccount, movements, lastUpdated]);

  const loaded = balance !== null && reserveProof !== null;

  return (
    <div className="space-y-6">
      {status === "error" && !loaded ? (
        <Alert tone="error" title="Could not load your vault">
          {describeError(error)}{" "}
          <Button variant="ghost" size="sm" onClick={() => void refresh()}>
            Retry
          </Button>
        </Alert>
      ) : null}
      {error && loaded ? (
        <Alert tone="warning">Showing the last vault data received. {describeError(error)}</Alert>
      ) : null}

      <div className="grid gap-4 sm:gap-6 xl:grid-cols-3">
        <div className="min-w-0 xl:col-span-2">
          {loaded ? (
            <SovereignBalance balance={balance} reserveProof={reserveProof} className="h-full" />
          ) : (
            <SovereignBalanceSkeleton className="h-full" />
          )}
        </div>

        <div className="min-w-0 xl:col-span-3 xl:row-start-2">
          {loaded ? <PortfolioAnalytics balance={balance} holdings={holdings} /> : <PortfolioAnalyticsSkeleton />}
        </div>

        <div className="min-w-0 xl:col-span-3 xl:row-start-3">
          {history ? (
            <PortfolioChart
              points={history}
              now={lastUpdated ?? Date.now()}
              title="Cash balance"
              caption="Your vault account over time, movement by movement."
            />
          ) : (
            <ChartSkeleton />
          )}
        </div>

        <div className="min-w-0 xl:col-start-3 xl:row-start-1">
          {/* Until the client is known, nothing invites a deposit the bank might return. */}
          {!account || !client ? (
            <SpeiDepositCardSkeleton className="h-full" />
          ) : kycLocked ? (
            <DepositLocked />
          ) : (
            <SpeiDepositCard clabe={account.clabe} bankName={account.bank_name} className="h-full" />
          )}
        </div>

        {/* Its own grid item, or nothing: no empty row (and no extra gap) without open orders. */}
        <OpenOrdersList token={token} className="min-w-0 xl:col-span-3" />

        <div className="min-w-0 xl:col-span-3">
          {loaded ? <AssetList holdings={holdings} /> : <AssetListSkeleton />}
        </div>
      </div>
    </div>
  );
}

/** In place of the deposit card while the identity is not verified: SPEI deposits would be returned. */
function DepositLocked() {
  return (
    <section
      aria-labelledby="deposit-locked-title"
      className="h-full rounded-2xl border border-border bg-surface p-5 sm:p-6"
      data-testid="vault-deposit-locked"
    >
      <div className="flex items-start gap-3">
        <span
          aria-hidden
          className="grid size-10 shrink-0 place-items-center rounded-xl bg-synthetic-yellow/10 text-synthetic-yellow ring-1 ring-synthetic-yellow/25"
        >
          <Lock className="size-[18px]" />
        </span>
        <div className="min-w-0">
          <h2 id="deposit-locked-title" className="text-base font-semibold tracking-tight">
            Deposits open once you&apos;re verified
          </h2>
          <p className="mt-1 text-sm text-muted">
            Until your identity is verified, SPEI transfers to your CLABE are returned to the sender.
          </p>
        </div>
      </div>
      <Link
        href="/auth/onboarding"
        className="mt-6 inline-flex min-h-11 w-full items-center justify-center rounded-xl bg-synthetic-yellow px-4 text-sm font-semibold text-black transition hover:bg-synthetic-yellow-hover focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none active:bg-synthetic-yellow-pressed"
      >
        Continue verification
      </Link>
    </section>
  );
}

function ChartSkeleton() {
  return (
    <div className="rounded-2xl border border-border bg-surface p-5 sm:p-6" role="status">
      <span className="sr-only">Loading your balance history…</span>
      <div aria-hidden className="motion-safe:animate-pulse">
        <div className="flex items-center justify-between gap-3">
          <div className="h-4 w-32 rounded-sm bg-surface-2" />
          <div className="h-8 w-48 rounded-lg bg-surface-2" />
        </div>
        <div className="mt-5 h-8 w-44 rounded-lg bg-surface-2" />
        <div className="mt-6 h-44 rounded-lg bg-surface-2 sm:h-52" />
      </div>
    </div>
  );
}
