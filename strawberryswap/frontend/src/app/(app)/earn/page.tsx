"use client";

import { BankCard } from "@/components/earn/BankCard";
import { VaultCard } from "@/components/earn/VaultCard";
import { PageHeader } from "@/components/ui/primitives";

export default function EarnPage() {
  return (
    <>
      <PageHeader title="Earn & Vaults" subtitle="Non-custodial strategy vaults and segregated bank deposits. Nobody can withdraw your assets but you." />
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
