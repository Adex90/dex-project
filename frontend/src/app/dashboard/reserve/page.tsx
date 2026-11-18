"use client";

import { useDashboard } from "@/components/layout/dashboard-shell";
import { SolvencyExplorer } from "@/components/reserve/solvency-explorer";
import { useDashboardStore } from "@/store/dashboard";

/**
 * Reserve: the Solvency Explorer. The published snapshot (root, ratio,
 * anchors, asset roots), the customer's own leaf, and the in-browser proof
 * that the leaf hashes up to the published root.
 */
export default function ReservePage() {
  const { token } = useDashboard();
  const accountId = useDashboardStore((s) => s.summary?.boveda?.id ?? null);

  return <SolvencyExplorer token={token} accountId={accountId} />;
}
