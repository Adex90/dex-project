"use client";

import { MarketStrip } from "@/components/markets/MarketStrip";
import { OrdersCard } from "@/components/trade/OrdersCard";
import { PoolCard } from "@/components/trade/PoolCard";
import { SwapCard } from "@/components/trade/SwapCard";
import { WalletCard } from "@/components/trade/WalletCard";
import { PageHeader } from "@/components/ui/primitives";

export default function TradePage() {
  return (
    <>
      <PageHeader title="Trade" subtitle="Private swaps settle in batches at one uniform price. No retail fee; proofs are generated on this device." />
      {/* Mobile: one column in task order (swap, wallet, orders, pool). Desktop: swap + orders left, wallet + pool right. */}
      <div className="flex flex-col gap-5 lg:grid lg:grid-cols-[minmax(0,1.1fr)_minmax(0,0.9fr)] lg:items-start">
        <div className="contents lg:flex lg:flex-col lg:gap-5">
          <SwapCard className="order-1" />
          <OrdersCard className="order-3" />
        </div>
        <div className="contents lg:flex lg:flex-col lg:gap-5">
          <WalletCard className="order-2" />
          <PoolCard className="order-4" />
        </div>
      </div>
      <MarketStrip className="mt-8" />
    </>
  );
}
