"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { ArrowLeft } from "lucide-react";

import { useDashboard } from "@/components/layout/dashboard-shell";
import { tabFor, useNavOrigin } from "@/components/layout/navigation";
import { OpenOrdersList } from "@/components/markets/open-orders-list";
import { OrderHistoryList } from "@/components/markets/order-history-list";
import { SegmentedControl, type SegmentOption } from "@/components/ui/segmented-control";
import { ordersPath, ordersTab, type OrdersTab } from "@/lib/order-history";

/*
 * Orders: /dashboard/orders, every order the client has placed, linked from
 * the Vault (the open orders' header and the assets' last line). Like an
 * asset's page it belongs to the tab that opened it, and its Back link
 * returns there.
 *
 * - Open: the limit and stop orders still waiting for their price, with
 *   Cancel (components/markets/open-orders-list.tsx, as on the Vault).
 * - History: everything no longer open, newest first, by pages
 *   (components/markets/order-history-list.tsx).
 *
 * The tab is in the address (`?tab=history`), so a link or a reload opens
 * the same one; it is read in the browser once mounted, as the Documents
 * page reads `?id=`.
 */

const TABS: readonly SegmentOption<OrdersTab>[] = [
  { value: "open", label: "Open" },
  { value: "history", label: "History" },
];

export default function OrdersPage() {
  const { token } = useDashboard();
  const back = tabFor(useNavOrigin());
  // Null until the address is read: no flash of the other tab.
  const [tab, setTab] = useState<OrdersTab | null>(null);

  useEffect(() => {
    setTab(ordersTab(new URLSearchParams(window.location.search).get("tab")));
  }, []);

  const choose = (next: OrdersTab) => {
    setTab(next);
    window.history.replaceState(window.history.state, "", ordersPath(next));
  };

  return (
    <div className="space-y-4" data-testid="orders-page">
      <Link
        href={back.href}
        className="inline-flex h-9 items-center gap-2 border border-border px-3 text-[11px] font-semibold tracking-[0.18em] text-ink-2 uppercase transition-colors hover:border-synthetic-yellow/60 hover:text-synthetic-yellow focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-bg focus-visible:outline-none"
        data-testid="orders-back"
      >
        <ArrowLeft className="size-4" aria-hidden />
        Back to {back.label.toLowerCase()}
      </Link>

      <header className="flex flex-wrap items-end justify-between gap-4">
        <div className="min-w-0">
          <h2 className="text-2xl font-semibold tracking-tight text-ink">Orders</h2>
          <p className="mt-1 text-sm text-muted">Your open orders, and every order you have placed.</p>
        </div>
        <SegmentedControl
          legend="Show"
          options={TABS}
          value={tab ?? "open"}
          onChange={choose}
          className="w-full sm:w-64"
          testId="orders-tabs"
        />
      </header>

      {tab === "open" ? (
        <OpenOrdersList token={token} standalone />
      ) : tab === "history" ? (
        <OrderHistoryList token={token} />
      ) : null}
    </div>
  );
}
