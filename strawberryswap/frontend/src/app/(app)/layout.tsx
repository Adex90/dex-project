import { AppStateProvider } from "@/components/AppState";
import { AppFooter, AppHeader, BottomNav } from "@/components/shell/Shell";
import { WalletModalProvider } from "@/components/wallet/WalletModal";

/**
 * Shared shell for Trade, Stocks, Earn and Portfolio. The layout stays mounted across these routes, so the state held
 * by AppStateProvider (wallet, pool, shielded wallet, swap progress, vault) survives switching tabs.
 */
export default function AppLayout({ children }: { children: React.ReactNode }) {
  return (
    <AppStateProvider>
      <WalletModalProvider>
        <AppHeader />
        <main className="mx-auto max-w-6xl px-4 pb-10 pt-6 sm:px-6 sm:pt-8">{children}</main>
        <AppFooter />
        <BottomNav />
      </WalletModalProvider>
    </AppStateProvider>
  );
}
