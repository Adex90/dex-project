"use client";

import { BridgeForm } from "@/components/dashboard/bridge-form";
import { SofomCard } from "@/components/dashboard/sofom-card";
import { useDashboard } from "@/components/layout/dashboard-shell";
import { useDashboardStore } from "@/store/dashboard";

/** Invest: fixed-term mutuum contracts (SOFOM) and the bridge that funds them from the vault. */
export default function InvestPage() {
  const { token, client, kycLocked, refresh } = useDashboard();
  const summary = useDashboardStore((s) => s.summary);
  const rates = useDashboardStore((s) => s.rates);

  return (
    <div className="grid gap-6 xl:grid-cols-5">
      <div className="min-w-0 xl:col-span-2">
        <BridgeForm
          available={summary?.boveda?.saldo_disponible ?? null}
          rates={rates}
          token={token}
          disabled={client === null}
          kycLocked={kycLocked}
          onInvested={refresh}
        />
      </div>
      <div className="min-w-0 xl:col-span-3">
        <SofomCard position={summary?.inversion ?? null} token={token} onChanged={refresh} />
      </div>
    </div>
  );
}
