"use client";

import { createContext, useCallback, useContext, useEffect, useMemo, type ReactNode } from "react";
import { usePathname, useRouter } from "next/navigation";

import { DashboardHeader } from "@/components/dashboard/dashboard-header";
import { KycBanner, isKycLocked } from "@/components/dashboard/kyc-status";
import { Navigation, activeItem, resetNavOrigin, useNavOrigin } from "@/components/layout/navigation";
import { Alert, Button } from "@/components/ui/primitives";
import { useDashboardData } from "@/hooks/use-dashboard-data";
import { useSession } from "@/hooks/use-session";
import { useVaultData } from "@/hooks/use-vault-data";
import { useVaultSocket } from "@/hooks/use-vault-socket";
import { describeError } from "@/lib/api/client";
import type { Cliente } from "@/lib/api/types";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { useAuthStore } from "@/store/auth";
import { useDashboardStore } from "@/store/dashboard";
import { useMarketStore } from "@/store/market";
import { useOrdersStore } from "@/store/orders";
import { displayedRealtime, useVaultStore } from "@/store/vault";

/*
 * The dashboard's frame, shared by every tab (Vault, Discover, Account,
 * Invest, Reserve) and the asset pages: the navigation, the header, the load
 * errors and the KYC banner.
 *
 * It owns the data: the summary, the ledger and the vault load once here and
 * stay in their stores while the client moves between tabs, so switching
 * tabs never reloads or flashes skeletons. The vault stays live over its
 * WebSocket; when the stream reports a newer version (a SPEI deposit, an
 * investment) the summary and the ledger reload too, so every tab, the chart
 * included, follows within a moment. Pages read the stores and get the
 * session, the KYC lock and `refresh` from `useDashboard()`.
 */

export interface DashboardContextValue {
  /** The session's access token. */
  token: string;
  /** The account holder, once the summary has loaded. */
  client: Cliente | null;
  /** Identity not verified yet: the bank (and its database) refuses money movements. */
  kycLocked: boolean;
  /** Reloads the summary, the ledger and the vault, e.g. after a deposit or an investment. */
  refresh: () => Promise<void>;
}

const DashboardContext = createContext<DashboardContextValue | null>(null);

export function useDashboard(): DashboardContextValue {
  const value = useContext(DashboardContext);
  if (!value) throw new Error("useDashboard() must be used inside <DashboardShell>.");
  return value;
}

export function DashboardShell({ children }: { children: ReactNode }) {
  const router = useRouter();
  const pathname = usePathname();
  const { token, logout } = useSession();
  const { refresh: refreshDashboard } = useDashboardData(token);
  const { refresh: refreshVault } = useVaultData(token);
  const endSession = useCallback(() => {
    useAuthStore.getState().expire();
    router.replace(EXPIRED_LOGIN_PATH);
  }, [router]);
  useVaultSocket(token, { onAdvance: () => void refreshDashboard(), onSessionEnded: endSession });
  const realtime = useVaultStore(displayedRealtime);
  const summary = useDashboardStore((s) => s.summary);
  const loading = useDashboardStore((s) => s.loading);
  const error = useDashboardStore((s) => s.error);
  const lastUpdated = useDashboardStore((s) => s.lastUpdated);
  const origin = useNavOrigin();

  // Sign-out unmounts the dashboard: the next person in this tab starts from
  // an empty search, and an asset page opened by address belongs to the Vault.
  useEffect(
    () => () => {
      useMarketStore.getState().reset();
      useOrdersStore.getState().reset();
      resetNavOrigin();
    },
    [],
  );

  const refresh = useCallback(async () => {
    await Promise.all([refreshDashboard(), refreshVault()]);
  }, [refreshDashboard, refreshVault]);

  const signOut = useCallback(() => {
    logout();
    router.replace("/auth/login");
  }, [logout, router]);

  const client = summary?.cliente ?? null;
  // Until the identity is VERIFICADO the database refuses deposits, transfers,
  // withdrawals and investments: lock those controls rather than let a 422 explain it.
  const kycLocked = isKycLocked(client);
  const context = useMemo<DashboardContextValue | null>(
    () => (token ? { token, client, kycLocked, refresh } : null),
    [token, client, kycLocked, refresh],
  );

  if (!context) return null;
  const section = activeItem(pathname, origin);

  return (
    <DashboardContext value={context}>
      <Navigation />
      <div className="lg:pl-60">
        <DashboardHeader
          title={section?.label ?? "Dashboard"}
          client={client}
          realtime={realtime}
          lastUpdated={lastUpdated}
          refreshing={loading}
          onRefresh={() => void refresh()}
          onLogout={signOut}
        />
        {/* Bottom padding below lg: the tab bar (and the phone's home indicator) never covers content. */}
        <main className="mx-auto max-w-7xl space-y-6 px-4 pt-6 pb-[calc(6rem+env(safe-area-inset-bottom))] sm:px-6 lg:pb-10">
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
          {client && kycLocked ? <KycBanner client={client} token={context.token} /> : null}
          {children}
        </main>
      </div>
    </DashboardContext>
  );
}
