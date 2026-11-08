"use client";

import { PortfolioCard } from "@/components/portfolio/PortfolioCard";
import { useSettings } from "@/components/settings/Settings";
import { PageHeader } from "@/components/ui/primitives";

export default function PortfolioPage() {
  const { t } = useSettings();
  return (
    <>
      <PageHeader title={t("page.portfolio.title")} subtitle={t("page.portfolio.subtitle")} />
      <PortfolioCard />
    </>
  );
}
