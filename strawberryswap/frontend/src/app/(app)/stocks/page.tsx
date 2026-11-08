"use client";

import { MarketStrip } from "@/components/markets/MarketStrip";
import { IconStocks } from "@/components/ui/icons";
import { Card, CardHeader, Notice, PageHeader } from "@/components/ui/primitives";

const STEPS: [string, string][] = [
  ["Mint a pair", "Lock collateral once and receive a long and a short position on a capped index. Every pair is fully funded."],
  ["Trade either side", "Long and short positions can change hands freely until expiry."],
  ["Settle on proof", "After expiry the locked collateral is split at a zk-DECO-attested price. With no valid proof, both sides get half back."],
];

export default function StocksPage() {
  return (
    <>
      <PageHeader title="Stocks & RWAs" subtitle="Synthetic exposure to real-world prices, fully collateralised and settled against zero-knowledge-attested quotes." />
      <MarketStrip title="Market trackers" />
      <Card className="mt-6" aria-labelledby="rwa-title">
        <CardHeader icon={<IconStocks />} title={<span id="rwa-title">Synthetic RWAs</span>} subtitle="Long/short index pairs with 100% collateral" right={<span className="pill-plum">Not in this build</span>} />
        <ol className="grid gap-3 md:grid-cols-3">
          {STEPS.map(([title, text], i) => (
            <li key={title} className="card-inset">
              <span className="grid h-8 w-8 place-items-center rounded-full bg-gradient-to-br from-berry-deep to-fuchsia-deep text-sm font-bold text-white shadow-glow-fuchsia">{i + 1}</span>
              <h3 className="mt-3 font-bold">{title}</h3>
              <p className="mt-1 text-sm text-lavender">{text}</p>
            </li>
          ))}
        </ol>
        <Notice tone="info" className="mt-4">
          Prices for listed markets appear here once a production zk-DECO price feed is connected. Until then this page shows no quotes rather than
          estimates.
        </Notice>
      </Card>
    </>
  );
}
