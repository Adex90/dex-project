"use client";

import Link from "next/link";
import type { ReactNode } from "react";
import { useOptionalApp } from "@/components/appStateContext";
import { useSettings } from "@/components/settings/Settings";
import { IconDiscord, IconGitHub, IconTelegram, IconX } from "@/components/ui/brandIcons";
import { IconArrowRight } from "@/components/ui/icons";
import { Logo } from "@/components/ui/Mascot";
import { TokenIcon } from "@/components/ui/primitives";

// Every destination is real: the repository, its docs and audit report, and its issue tracker. Change them here.
const REPO = "https://github.com/Adex90/dex-project";
export const FOOTER_LINKS = {
  docs: `${REPO}/tree/main/strawberryswap#readme`,
  github: REPO,
  audits: `${REPO}/blob/main/strawberryswap/docs/AUDIT_PHASE8.md`,
  support: `${REPO}/issues`,
} as const;

/** Only an https URL counts as set. */
const httpsUrl = (u: string | undefined) => (u && /^https:\/\/[^\s"'<>]+$/.test(u) ? u : undefined);

type Social = { name: string; href: string | undefined; Icon: typeof IconX };
/**
 * Official accounts only. X, Telegram and Discord come from the build (NEXT_PUBLIC_SOCIAL_X, NEXT_PUBLIC_SOCIAL_TELEGRAM,
 * NEXT_PUBLIC_SOCIAL_DISCORD) and stay hidden until set, so the footer never sends anyone to a guessed handle:
 * look-alike Telegram and Discord groups are a common way crypto users get phished. GitHub is the repository.
 */
const SOCIALS = (
  [
    { name: "X", href: httpsUrl(process.env.NEXT_PUBLIC_SOCIAL_X), Icon: IconX },
    { name: "Telegram", href: httpsUrl(process.env.NEXT_PUBLIC_SOCIAL_TELEGRAM), Icon: IconTelegram },
    { name: "Discord", href: httpsUrl(process.env.NEXT_PUBLIC_SOCIAL_DISCORD), Icon: IconDiscord },
    { name: "GitHub", href: REPO, Icon: IconGitHub },
  ] as Social[]
).filter((s): s is Social & { href: string } => !!s.href);

const linkCls =
  "group inline-flex items-center gap-1 rounded text-sm text-haze transition hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-fuchsia";

/** External link: new tab, no opener, no referrer (the page address is not leaked to the destination). */
export function ExternalLink({ href, children }: { href: string; children: ReactNode }) {
  return (
    <a href={href} target="_blank" rel="noopener noreferrer" className={linkCls}>
      {children}
      <span aria-hidden className="text-[10px] transition group-hover:text-fuchsia">↗</span>
    </a>
  );
}

function Column({ id, title, children }: { id: string; title: string; children: ReactNode }) {
  return (
    <nav aria-labelledby={id}>
      <h2 id={id} className="eyebrow mb-3">{title}</h2>
      <ul className="space-y-2">{children}</ul>
    </nav>
  );
}

/**
 * BERRY's price from the dark pool, in the pool's USD stablecoin, next to a Buy button that opens the private swap.
 * The figure appears only inside the app, which already polls the pool: the landing and terms pages make no chain
 * request, so there the badge shows no number. USD uses the stablecoin's stated 1:1 peg; EUR and MXN have no rate feed,
 * so the figure stays in the stablecoin.
 */
function BerryPriceBadge() {
  const app = useOptionalApp();
  const { fiat, locale, t } = useSettings();
  const ps = app?.pool.state;
  let price: number | undefined;
  let stable = "";
  if (ps) {
    const spot = Number(ps.spot) / 1e18; // token1 per token0
    if (ps.symbol0 === "BERRY" && ps.symbol1.startsWith("USD")) [price, stable] = [spot, ps.symbol1];
    else if (ps.symbol1 === "BERRY" && ps.symbol0.startsWith("USD") && spot > 0) [price, stable] = [1 / spot, ps.symbol0];
  }
  const text =
    price === undefined
      ? undefined
      : fiat === "USD"
        ? new Intl.NumberFormat(locale, { style: "currency", currency: "USD", minimumFractionDigits: 4, maximumFractionDigits: 4 }).format(price)
        : `${price.toFixed(4)} ${stable}`;
  const note = price === undefined ? undefined : fiat === "USD" ? t("fiat.peg", { stable }) : t("fiat.noRate", { fiat });
  return (
    <div className="flex items-center gap-1 self-start rounded-full border border-fuchsia/40 bg-plum-850 py-1 pl-1.5 pr-1 shadow-glow-fuchsia transition hover:border-fuchsia sm:self-auto">
      <span className="flex items-center gap-2 pr-2" title={note}>
        <TokenIcon symbol="BERRY" size={28} />
        {text ? (
          <span className="num text-sm font-semibold text-ink">
            <span className="sr-only">{t("footer.price")}: </span>
            {text}
          </span>
        ) : null}
        <span className="text-sm font-semibold text-lavender">BERRY</span>
      </span>
      <Link href="/trade/" className="btn-primary !px-4 !py-2 text-sm no-underline">
        {t("footer.buy")}
        <IconArrowRight className="h-4 w-4" />
      </Link>
    </div>
  );
}

/**
 * Institutional footer: brand line and the directory (developer, security and company links), then the community links
 * with the BERRY price badge, then the standing disclaimer. `withBottomNav` leaves room for the mobile navigation bar.
 */
export function SiteFooter({ withBottomNav = false }: { withBottomNav?: boolean }) {
  const { t } = useSettings();
  return (
    <footer className="mt-12 border-t border-lavender/10 bg-plum-950/50">
      <div className={`mx-auto max-w-6xl px-4 pt-10 sm:px-6 ${withBottomNav ? "pb-28 lg:pb-10" : "pb-10"}`}>
        <div className="grid gap-8 sm:grid-cols-2 lg:grid-cols-[1.6fr_1fr_1fr_1fr]">
          <div>
            <Logo />
            <p className="mt-3 max-w-xs text-sm leading-relaxed text-haze">{t("footer.tagline")}</p>
          </div>
          <Column id="footer-dev" title={t("footer.developers")}>
            <li><ExternalLink href={FOOTER_LINKS.docs}>{t("footer.docs")}</ExternalLink></li>
            <li><ExternalLink href={FOOTER_LINKS.github}>GitHub</ExternalLink></li>
          </Column>
          <Column id="footer-sec" title={t("footer.security")}>
            <li><ExternalLink href={FOOTER_LINKS.audits}>{t("footer.audits")}</ExternalLink></li>
            <li className="max-w-[14rem] text-xs leading-relaxed text-haze">{t("footer.auditsNote")}</li>
          </Column>
          <Column id="footer-co" title={t("footer.company")}>
            <li>
              <Link href="/terms/" className={linkCls}>
                {t("footer.terms")}
              </Link>
            </li>
            <li><ExternalLink href={FOOTER_LINKS.support}>{t("footer.support")}</ExternalLink></li>
          </Column>
        </div>
        <div className="mt-10 flex flex-col gap-5 border-t border-lavender/10 pt-6 sm:flex-row sm:items-center sm:justify-between">
          <nav aria-label={t("footer.social")}>
            <ul className="flex items-center gap-2">
              {SOCIALS.map(({ name, href, Icon }) => (
                <li key={name}>
                  <a
                    href={href}
                    target="_blank"
                    rel="noopener noreferrer"
                    aria-label={t("footer.socialOn", { name })}
                    title={name}
                    className="grid h-10 w-10 place-items-center rounded-full border border-lavender/15 bg-plum-850 text-lavender transition duration-200 hover:-translate-y-0.5 hover:border-fuchsia hover:text-ink hover:shadow-glow-fuchsia focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-fuchsia"
                  >
                    <Icon />
                  </a>
                </li>
              ))}
            </ul>
          </nav>
          <BerryPriceBadge />
        </div>
        <div className="mt-6 flex flex-col gap-2 text-xs text-haze sm:flex-row sm:items-center sm:justify-between">
          <span>{t("footer.disclaimer")}</span>
          <span>{t("footer.noEmissions")}</span>
        </div>
      </div>
    </footer>
  );
}
