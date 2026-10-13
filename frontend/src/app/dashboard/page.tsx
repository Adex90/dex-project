"use client";

import { useRouter } from "next/navigation";
import { useCallback } from "react";

import { BridgeForm } from "@/components/dashboard/bridge-form";
import { DashboardHeader } from "@/components/dashboard/dashboard-header";
import { LedgerFeed } from "@/components/dashboard/ledger-feed";
import { ProofOfReservePanel } from "@/components/dashboard/proof-of-reserve";
import { SofomCard } from "@/components/dashboard/sofom-card";
import { VaultCard } from "@/components/dashboard/vault-card";
import { Alert, Button } from "@/components/ui/primitives";
import { useDashboardData } from "@/hooks/use-dashboard-data";
import { useSession } from "@/hooks/use-session";
import { describeError } from "@/lib/api/client";
import { useDashboardStore } from "@/store/dashboard";

export default function DashboardPage() {
  const router = useRouter();
  const { token, logout } = useSession();
  const { refresh } = useDashboardData(token);
  const summary = useDashboardStore((s) => s.summary);
  const movements = useDashboardStore((s) => s.movements);
  const hasMore = useDashboardStore((s) => s.hasMoreMovements);
  const rates = useDashboardStore((s) => s.rates);
  const loading = useDashboardStore((s) => s.loading);
  const loadingMore = useDashboardStore((s) => s.loadingMore);
  const error = useDashboardStore((s) => s.error);
  const lastUpdated = useDashboardStore((s) => s.lastUpdated);
  const loadMore = useDashboardStore((s) => s.loadMoreMovements);

  const signOut = useCallback(() => {
    logout();
    router.replace("/auth/login");
  }, [logout, router]);

  if (!token) return null;

  const client = summary?.cliente ?? null;
  const kycPending = client !== null && client.estatus_kyc !== "VERIFICADO";

  return (
    <div className="min-h-dvh">
      <DashboardHeader
        client={client}
        lastUpdated={lastUpdated}
        refreshing={loading}
        onRefresh={() => void refresh()}
        onLogout={signOut}
      />
      <main className="mx-auto max-w-7xl space-y-6 px-4 py-6 sm:px-6">
        {error && !summary ? (
          <Alert tone="error" title="Could not load your accounts">
            {describeError(error)}{" "}
            <Button variant="ghost" size="sm" onClick={() => void refresh()}>
              Retry
            </Button>
          </Alert>
        ) : null}
        {error && summary ? (
          <Alert tone="warning">Showing the last data received. {describeError(error)}</Alert>
        ) : null}
        {kycPending ? (
          <Alert tone="warning" title="Identity verification pending">
            Your KYC file is {client.estatus_kyc.toLowerCase().replace("_", " ")}. You can view your account, but
            money movements are disabled until compliance verifies it.
          </Alert>
        ) : null}

        <div className="grid gap-6 lg:grid-cols-5">
          <div className="lg:col-span-2">
            <VaultCard account={summary?.boveda ?? null} reserve={summary?.reserva ?? null} token={token} onChanged={refresh} />
          </div>
          <div className="lg:col-span-3">
            <SofomCard position={summary?.inversion ?? null} token={token} onChanged={refresh} />
          </div>
        </div>

        <div className="grid gap-6 lg:grid-cols-5">
          <div className="lg:col-span-2">
            <BridgeForm
              available={summary?.boveda?.saldo_disponible ?? null}
              rates={rates}
              token={token}
              disabled={kycPending}
              onInvested={refresh}
            />
          </div>
          <div className="lg:col-span-3">
            <ProofOfReservePanel token={token} />
          </div>
        </div>

        <LedgerFeed
          movements={movements}
          accountId={summary?.boveda?.id ?? null}
          loading={loading}
          hasMore={hasMore}
          loadingMore={loadingMore}
          onLoadMore={() => void loadMore(token)}
        />
      </main>
    </div>
  );
}
