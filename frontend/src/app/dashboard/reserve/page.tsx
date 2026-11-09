"use client";

import { ProofOfReservePanel } from "@/components/dashboard/proof-of-reserve";
import { useDashboard } from "@/components/layout/dashboard-shell";
import { useDashboardStore } from "@/store/dashboard";

/** Reserve: verify, in the browser, that this account is inside the bank's published 100% reserve. */
export default function ReservePage() {
  const { token } = useDashboard();
  const accountId = useDashboardStore((s) => s.summary?.boveda?.id ?? null);

  return (
    <div className="max-w-4xl">
      <ProofOfReservePanel token={token} accountId={accountId} />
    </div>
  );
}
