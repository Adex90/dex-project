"use client";

import { chain, config } from "@/lib/config";
import { useSettings } from "@/components/settings/Settings";

/**
 * The trusted-bootstrap disclosure, at the top of every page of a testnet build (config.testnet). It cannot be
 * dismissed: whoever lands on any page sees, before anything else, that the price is only as honest as the operator's
 * endpoint and notary, that the verifiers' keys come from a single-party setup, and that the tokens have no value.
 * The same text stands on chain in TestnetDisclosure (linked when the build knows its address and an explorer).
 */
export function TestnetBanner() {
  const { t } = useSettings();
  if (!config.testnet) return null;

  const explorer = config.explorerUrl;
  const at = (address: string | undefined, tab: string) => (explorer && address ? `${explorer}/address/${address}${tab}` : undefined);
  const links = [
    { href: at(config.disclosure, "#readContract"), label: t("testnet.disclosure") },
    { href: at(config.faucets.tWETH, "#writeContract"), label: t("testnet.faucet", { token: "tWETH" }) },
    { href: at(config.faucets.tUSD, "#writeContract"), label: t("testnet.faucet", { token: "tUSD" }) },
  ].filter((l): l is { href: string; label: string } => l.href !== undefined);

  return (
    <aside aria-label={t("testnet.region")} data-testid="testnet-banner" className="border-b border-blush/40 bg-blush/10 text-blush">
      <div className="mx-auto flex max-w-6xl gap-3 px-4 py-2.5 sm:px-6">
        {/* Phones: the header's badge, right below, says the same; the text gets the width. */}
        <span aria-hidden className="mt-0.5 hidden shrink-0 select-none self-start rounded-full border border-blush/50 px-2 py-0.5 text-[11px] font-bold uppercase tracking-wider sm:inline-block">
          {t("testnet.badge")}
        </span>
        <div className="min-w-0 text-[13px] leading-snug sm:text-sm">
          <p className="font-semibold">{t("testnet.title", { chain: chain.name })}</p>
          <p className="mt-0.5 text-blush/90">
            {t("testnet.oracle")} {t("testnet.keys")} {t("testnet.tokens")}
          </p>
          {links.length > 0 ? (
            <p className="mt-1 flex flex-wrap gap-x-4 gap-y-1">
              {links.map((l) => (
                <a key={l.href} href={l.href} target="_blank" rel="noopener noreferrer" className="font-semibold underline underline-offset-2 hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-fuchsia">
                  {l.label} ↗
                </a>
              ))}
            </p>
          ) : null}
        </div>
      </div>
    </aside>
  );
}

/** Always-visible marker in the sticky header (the banner scrolls away with the page). */
export function TestnetBadge() {
  const { t } = useSettings();
  if (!config.testnet) return null;
  return (
    <span title={`${t("testnet.oracle")} ${t("testnet.keys")} ${t("testnet.tokens")}`} className="shrink-0 rounded-full border border-blush/50 bg-blush/10 px-2 py-0.5 text-[11px] font-bold uppercase tracking-wider text-blush">
      {t("testnet.badge")}
    </span>
  );
}
