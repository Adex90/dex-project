"use client";

import { useState, type ReactNode } from "react";
import { useApp } from "@/components/AppState";
import { sinceStartPct } from "@/components/earn/VaultCard";
import { useSettings } from "@/components/settings/Settings";
import { IconFlame, IconPause, IconPlay } from "@/components/ui/icons";
import { TokenIcon } from "@/components/ui/primitives";
import { bpsToPct, fmt } from "@/lib/format";

/** Listed on the RWA desk; they read "feed pending" until a zk-DECO-attested price feed exists (as in Markets). */
const SYNTHETICS = [
  { ticker: "SPY", name: "SPY" },
  { ticker: "NVDA", name: "NVIDIA" },
] as const;

/** Four copies of the item row; the track moves by two (−50 %), so the loop is seamless while one copy is at least
 *  half the visible width. Only the first copy is exposed to assistive technology. */
const COPIES = 4;

function Item({ icon, label, children }: { icon?: ReactNode; label: string; children?: ReactNode }) {
  return (
    <li className="flex shrink-0 items-center gap-2 whitespace-nowrap">
      {icon}
      <span className="font-semibold text-ink">{label}</span>
      {children}
    </li>
  );
}

/**
 * "HOT" ticker under the app header: a seamless marquee of the venue's live figures. Every value comes from the chain
 * through the shared app state (the pool's spot price and fee, the vault's share price), so the bar adds no requests and
 * never shows a figure the contracts did not report. Pauses on hover and keyboard focus and with its button
 * (WCAG 2.2.2); with reduced motion it stands still and scrolls by hand.
 */
export function HotTicker() {
  const { pool, vault } = useApp();
  const { t } = useSettings();
  const [paused, setPaused] = useState(false);
  const ps = pool.state;
  const vs = vault.state;
  const vaultPct = vs ? sinceStartPct(vs.sharePrice) : undefined;

  const items = (
    <>
      <Item
        icon={
          <span className="flex -space-x-1.5">
            <TokenIcon symbol={ps?.symbol0 ?? "BERRY"} size={18} />
            <TokenIcon symbol={ps?.symbol1 ?? "USDB"} size={18} />
          </span>
        }
        label={ps ? `${ps.symbol0}/${ps.symbol1}` : "BERRY/USDB"}
      >
        {ps ? (
          <>
            <span className="num text-ink">{fmt(ps.spot, 18, 4)}</span>
            <span className="inline-flex items-center gap-1 text-mint">
              <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-mint" aria-hidden />
              {t("ticker.live")}
            </span>
          </>
        ) : (
          <span className="text-haze">{pool.configured ? "—" : t("ticker.offline")}</span>
        )}
      </Item>
      {ps ? (
        <Item label={t("ticker.fee")}>
          <span className="num text-mint">{bpsToPct(ps.feeBps)}</span>
        </Item>
      ) : null}
      {vs && vaultPct !== undefined ? (
        <Item icon={<TokenIcon symbol={vs.symbol} size={18} />} label={t("ticker.vault")}>
          <span className={`num ${vaultPct > 0 ? "text-mint" : vaultPct < 0 ? "text-berry-soft" : "text-lavender"}`}>
            {`${vaultPct > 0 ? "+" : ""}${vaultPct.toFixed(2)}%`}
          </span>
          <span className="text-haze">{t("ticker.sinceStart")}</span>
        </Item>
      ) : null}
      {SYNTHETICS.map((s) => (
        <Item key={s.ticker} icon={<TokenIcon symbol={s.ticker} size={18} />} label={`${s.name} (${t("ticker.synthetic")})`}>
          <span className="text-haze">{t("ticker.feedPending")}</span>
        </Item>
      ))}
    </>
  );

  return (
    <section aria-label={t("ticker.label")} className="border-b border-lavender/10 bg-plum-950">
      <div className="mx-auto flex max-w-6xl items-center gap-3 px-4 py-2 sm:px-6">
        <span className="inline-flex shrink-0 items-center gap-1 rounded-full bg-berry-deep px-2.5 py-1 text-[11px] font-extrabold uppercase tracking-wider text-white shadow-glow-berry">
          <IconFlame className="h-3.5 w-3.5" />
          {t("ticker.hot")}
        </span>
        <div className="group min-w-0 flex-1 overflow-hidden motion-reduce:no-scrollbar motion-reduce:overflow-x-auto">
          <div
            className={`flex w-max animate-marquee text-sm group-focus-within:[animation-play-state:paused] group-hover:[animation-play-state:paused] motion-reduce:animate-none ${
              paused ? "[animation-play-state:paused]" : ""
            }`}
          >
            {Array.from({ length: COPIES }, (_, i) => (
              <ul key={i} aria-hidden={i > 0 || undefined} className={`flex shrink-0 items-center gap-8 pr-8 ${i > 0 ? "motion-reduce:hidden" : ""}`}>
                {items}
              </ul>
            ))}
          </div>
        </div>
        <button
          type="button"
          onClick={() => setPaused((p) => !p)}
          aria-pressed={paused}
          aria-label={paused ? t("ticker.play") : t("ticker.pause")}
          title={paused ? t("ticker.play") : t("ticker.pause")}
          className="grid h-7 w-7 shrink-0 place-items-center rounded-full text-lavender transition hover:bg-plum-700 hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-fuchsia motion-reduce:hidden"
        >
          {paused ? <IconPlay className="h-3.5 w-3.5" /> : <IconPause className="h-3.5 w-3.5" />}
        </button>
      </div>
    </section>
  );
}
