"use client";

import { PortfolioCard } from "@/components/portfolio/PortfolioCard";
import { PageHeader } from "@/components/ui/primitives";

export default function PortfolioPage() {
  return (
    <>
      <PageHeader title="Portfolio" subtitle="Your shielded balances, vault position and private swap history." />
      <PortfolioCard />
    </>
  );
}
