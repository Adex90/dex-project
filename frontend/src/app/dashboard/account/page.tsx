"use client";

import { LedgerFeed } from "@/components/dashboard/ledger-feed";
import { VaultCard } from "@/components/dashboard/vault-card";
import { useDashboard } from "@/components/layout/dashboard-shell";
import { useDashboardStore } from "@/store/dashboard";

/** Account: the IFPE sight account (balance, deposits, transfers) and its ledger. */
export default function AccountPage() {
  const { token, kycLocked, refresh } = useDashboard();
  const summary = useDashboardStore((s) => s.summary);
  const movements = useDashboardStore((s) => s.movements);
  const hasMore = useDashboardStore((s) => s.hasMoreMovements);
  const loading = useDashboardStore((s) => s.loading);
  const loadingMore = useDashboardStore((s) => s.loadingMore);
  const loadMore = useDashboardStore((s) => s.loadMoreMovements);

  // Stacked: the ledger is a wide table (760 px minimum) that needs the full width.
  return (
    <div className="space-y-6">
      <div className="min-w-0 max-w-3xl">
        <VaultCard
          account={summary?.boveda ?? null}
          reserve={summary?.reserva ?? null}
          token={token}
          kycLocked={kycLocked}
          onChanged={refresh}
        />
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
