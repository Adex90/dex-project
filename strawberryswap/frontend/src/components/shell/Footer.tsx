"use client";

import Link from "next/link";
import type { ReactNode } from "react";
import { useSettings } from "@/components/settings/Settings";
import { Logo } from "@/components/ui/Mascot";

// Every destination is real: the repository, its docs and audit report, and its issue tracker. Change them here.
const REPO = "https://github.com/Adex90/dex-project";
export const FOOTER_LINKS = {
  docs: `${REPO}/tree/main/strawberryswap#readme`,
  github: REPO,
  audits: `${REPO}/blob/main/strawberryswap/docs/AUDIT_PHASE8.md`,
  support: `${REPO}/issues`,
} as const;

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
 * Institutional footer: brand line, developer and security resources, company links and the standing disclaimer.
 * `withBottomNav` leaves room for the mobile navigation bar.
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
        <div className="mt-10 flex flex-col gap-2 border-t border-lavender/10 pt-6 text-xs text-haze sm:flex-row sm:items-center sm:justify-between">
          <span>{t("footer.disclaimer")}</span>
          <span>{t("footer.noEmissions")}</span>
        </div>
      </div>
    </footer>
  );
}
