"use client";

import Link from "next/link";
import { useSettings } from "@/components/settings/Settings";
import { ExternalLink, FOOTER_LINKS, SiteFooter } from "@/components/shell/Footer";
import { Logo } from "@/components/ui/Mascot";

/**
 * Placeholder until the operator publishes its Terms of Service. Deliberately states only facts about this build:
 * legal terms are the operator's to write, not something to improvise in code.
 */
export default function TermsPage() {
  const { t } = useSettings();
  return (
    <>
      <header className="mx-auto flex max-w-6xl items-center justify-between gap-3 px-4 py-4 sm:px-6">
        <Link href="/" aria-label="StrawberrySwap home" className="rounded-2xl focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-fuchsia">
          <Logo compact />
        </Link>
        <Link href="/trade/" className="btn-secondary">
          {t("terms.back")}
        </Link>
      </header>
      <main className="mx-auto max-w-3xl px-4 pb-8 pt-8 sm:px-6">
        <article className="card">
          <h1 className="text-3xl font-extrabold tracking-tight">{t("terms.title")}</h1>
          <p className="mt-4 text-lavender">{t("terms.body1")}</p>
          <p className="mt-2 text-lavender">{t("terms.body2")}</p>
          <p className="mt-6 flex flex-wrap items-center gap-2 text-sm text-haze">
            {t("terms.support")} <ExternalLink href={FOOTER_LINKS.support}>GitHub Issues</ExternalLink>
          </p>
        </article>
      </main>
      <SiteFooter />
    </>
  );
}
