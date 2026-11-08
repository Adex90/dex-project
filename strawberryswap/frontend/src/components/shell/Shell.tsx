"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { chain } from "@/lib/config";
import { shortAddr } from "@/lib/format";
import { useApp } from "@/components/AppState";
import { SettingsMenu, useSettings } from "@/components/settings/Settings";
import { IconEarn, IconPortfolio, IconStocks, IconTrade, IconWallet } from "@/components/ui/icons";
import { Logo } from "@/components/ui/Mascot";
import { useWalletModal } from "@/components/wallet/WalletModal";

const TABS = [
  { href: "/trade/", label: "nav.trade", short: "nav.trade", Icon: IconTrade },
  { href: "/stocks/", label: "nav.stocks", short: "nav.stocks.short", Icon: IconStocks },
  { href: "/earn/", label: "nav.earn", short: "nav.earn.short", Icon: IconEarn },
  { href: "/portfolio/", label: "nav.portfolio", short: "nav.portfolio", Icon: IconPortfolio },
] as const;

function useActive() {
  const path = usePathname() ?? "";
  return (href: string) => path.replace(/\/?$/, "/").startsWith(href);
}

function WalletButton() {
  const { wallet } = useApp();
  const { openWallet } = useWalletModal();
  const { t } = useSettings();
  if (wallet.account) {
    return (
      <span className="flex items-center gap-2">
        {!wallet.chainOk ? (
          <button className="btn-secondary hidden !border-blush/50 !px-3 !py-1.5 text-blush sm:inline-flex" onClick={() => void wallet.switchChain()}>
            {t("header.switch")}
          </button>
        ) : null}
        <button
          type="button"
          onClick={openWallet}
          title={wallet.account}
          className="flex h-10 items-center gap-2 whitespace-nowrap rounded-full border border-fuchsia/30 bg-plum-700/70 pl-2 pr-3.5 text-sm transition hover:border-fuchsia hover:shadow-glow-fuchsia focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-fuchsia"
        >
          <span className={`h-2.5 w-2.5 rounded-full ${wallet.chainOk ? "bg-mint shadow-glow-mint" : "bg-blush"}`} aria-hidden />
          <span className="num font-semibold text-ink">{shortAddr(wallet.account)}</span>
        </button>
      </span>
    );
  }
  return (
    <button className="btn-primary h-10 whitespace-nowrap !px-4 !py-0 text-sm" onClick={openWallet}>
      <IconWallet className="h-4 w-4" />
      {wallet.hasProvider ? t("header.connect") : t("header.noWallet")}
    </button>
  );
}

export function AppHeader() {
  const { wallet } = useApp();
  const { t } = useSettings();
  const active = useActive();
  return (
    <header className="sticky top-0 z-40 border-b border-lavender/10 bg-plum-950/70 backdrop-blur-xl">
      <div className="mx-auto flex max-w-6xl items-center gap-2 px-4 py-2.5 sm:gap-3 sm:px-6">
        <Link href="/" aria-label="StrawberrySwap home" className="rounded-2xl focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-fuchsia">
          <Logo compact />
        </Link>

        <nav aria-label="Main" className="ml-4 hidden items-center gap-1 rounded-full bg-plum-900/70 p-1 shadow-inset lg:flex">
          {TABS.map(({ href, label, short, Icon }) => {
            const on = active(href);
            return (
              <Link
                key={href}
                href={href}
                aria-current={on ? "page" : undefined}
                className={`flex items-center gap-2 whitespace-nowrap rounded-full px-4 py-2 text-sm font-semibold transition duration-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-fuchsia ${
                  on ? "bg-gradient-to-r from-berry-deep to-fuchsia-deep text-white shadow-glow-fuchsia" : "text-lavender hover:bg-plum-700/70 hover:text-ink"
                }`}
              >
                <Icon className="h-4 w-4" />
                <span className="xl:hidden">{t(short)}</span>
                <span className="hidden xl:inline">{t(label)}</span>
              </Link>
            );
          })}
        </nav>

        <div className="ml-auto flex items-center gap-2">
          <span className="pill-plum hidden 2xl:inline-flex" title="Network this build talks to">
            <span className="h-1.5 w-1.5 rounded-full bg-fuchsia" aria-hidden />
            <span className="num">{chain.name} · {chain.id}</span>
          </span>
          <SettingsMenu />
          <WalletButton />
        </div>
      </div>

      {wallet.account && !wallet.chainOk ? (
        <div role="alert" className="flex items-center justify-center gap-3 border-t border-blush/20 bg-blush/10 px-4 py-2 text-center text-sm text-blush">
          <span>{t("header.wrongNetwork", { chain: chain.name })}</span>
          <button className="btn-secondary !border-blush/50 !px-3 !py-1 text-xs text-blush sm:hidden" onClick={() => void wallet.switchChain()}>
            {t("header.switch")}
          </button>
        </div>
      ) : null}
      {wallet.error ? (
        <p role="alert" className="border-t border-berry/30 bg-berry/10 px-4 py-2 text-center text-sm text-berry-soft">
          {wallet.error}
        </p>
      ) : null}
    </header>
  );
}

/** Mobile-first sticky bottom navigation with a glowing active state (phones and tablets; from lg up the header pills take over). */
export function BottomNav() {
  const active = useActive();
  const { t } = useSettings();
  return (
    <nav aria-label="Main" className="fixed inset-x-0 bottom-0 z-40 lg:hidden">
      <div className="pb-safe mx-auto max-w-md px-3">
        <div className="mb-1 grid grid-cols-4 gap-1 rounded-3xl border border-lavender/10 bg-plum-900/85 p-1.5 shadow-card backdrop-blur-xl">
          {TABS.map(({ href, short, label, Icon }) => {
            const on = active(href);
            return (
              <Link
                key={href}
                href={href}
                aria-label={t(label)}
                aria-current={on ? "page" : undefined}
                className={`relative flex flex-col items-center gap-0.5 rounded-2xl py-2 text-[11px] font-semibold transition duration-200 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-fuchsia ${
                  on ? "bg-fuchsia/15 text-fuchsia shadow-glow-fuchsia" : "text-lavender active:scale-95"
                }`}
              >
                <Icon className={`h-5 w-5 ${on ? "drop-shadow-[0_0_6px_rgba(255,42,133,0.9)]" : ""}`} />
                {t(short)}
                {on ? <span className="absolute -top-1 h-1 w-6 rounded-full bg-fuchsia shadow-glow-fuchsia" aria-hidden /> : null}
              </Link>
            );
          })}
        </div>
      </div>
    </nav>
  );
}
