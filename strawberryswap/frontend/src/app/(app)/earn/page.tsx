"use client";

import { BankCard } from "@/components/earn/BankCard";
import { VaultCard } from "@/components/earn/VaultCard";
import { useSettings } from "@/components/settings/Settings";
import { PageHeader } from "@/components/ui/primitives";

export default function EarnPage() {
  const { t } = useSettings();
  return (
    <>
      <PageHeader title={t("page.earn.title")} subtitle={t("page.earn.subtitle")} />
      <section aria-labelledby="vaults-heading" className="space-y-3">
        <h2 id="vaults-heading" className="text-lg font-bold">Strategy vaults</h2>
        <VaultCard />
      </section>
      <section aria-label="Sovereign Bank" className="mt-8">
        <BankCard />
      </section>
    </>
  );
}
