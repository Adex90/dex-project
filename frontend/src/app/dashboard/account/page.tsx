"use client";

import { LedgerFeed } from "@/components/dashboard/ledger-feed";
import { VaultCard } from "@/components/dashboard/vault-card";
import { useDashboard } from "@/components/layout/dashboard-shell";
import { CardManagement } from "@/components/wealth/card-management";
import { CashYieldSection } from "@/components/wealth/cash-yield-section";
import { useDashboardStore } from "@/store/dashboard";

/*
 * Account: the wealth and banking dashboard, around the IFPE sight account.
 *
 * - Cash & Yield (components/wealth/cash-yield-section.tsx): the uninvested
 *   cash and the Sovereign Yield counter, live. A preview: nothing is paid.
 * - The debit card (components/wealth/card-management.tsx): freeze, PIN,
 *   settings and saveback. A preview: no card is issued yet.
 * - The account itself (balance, CLABE, deposits) and its ledger, as before.
 *
 * Layout: one column on phones (yield, card, account, ledger); from xl, the
 * yield above the account in two thirds, the card in the third beside them,
 * and the ledger across the full width (a wide table, 760 px at least).
 */
export default function AccountPage() {
  const { token, kycLocked, refresh } = useDashboard();
  const summary = useDashboardStore((s) => s.summary);
  const movements = useDashboardStore((s) => s.movements);
  const hasMore = useDashboardStore((s) => s.hasMoreMovements);
  const loading = useDashboardStore((s) => s.loading);
  const loadingMore = useDashboardStore((s) => s.loadingMore);
  const loadMore = useDashboardStore((s) => s.loadMoreMovements);

  return (
    <div className="space-y-6">
      <div className="grid gap-4 sm:gap-6 xl:grid-cols-3">
        <CashYieldSection className="min-w-0 xl:col-span-2" />
        {/* self-start: as tall as its content, not stretched beside the two sections it spans. */}
        <CardManagement className="min-w-0 xl:col-start-3 xl:row-span-2 xl:row-start-1 xl:self-start" />
        <div className="min-w-0 xl:col-span-2">
          <VaultCard
            account={summary?.boveda ?? null}
            reserve={summary?.reserva ?? null}
            token={token}
            kycLocked={kycLocked}
            onChanged={refresh}
          />
        </div>
      </div>
      <div className="min-w-0">
        <LedgerFeed
          movements={movements}
          accountId={summary?.boveda?.id ?? null}
          loading={loading}
          hasMore={hasMore}
          loadingMore={loadingMore}
          onLoadMore={() => void loadMore(token)}
        />
      </div>
    </div>
  );
}
