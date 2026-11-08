"use client";

import { useState, type ReactNode } from "react";
import { IconBank, IconClock, IconShield } from "@/components/ui/icons";
import { Card, CardHeader, Notice, Segmented, TabPanel } from "@/components/ui/primitives";

type Product = { name: string; icon: ReactNode; pills: { cls: string; text: string }[]; terms: [string, string][] };

// Terms are the contracts' own rules (contracts/src/banking/SovereignBank.sol and its tests), not marketing figures.
const DEMAND: Product[] = [
  {
    name: "Demand deposit",
    icon: <IconShield />,
    pills: [
      { cls: "pill-mint", text: "100% reserve" },
      { cls: "pill-plum", text: "0% yield" },
    ],
    terms: [
      ["Withdraw", "any time, to any address"],
      ["Custody", "held 1:1; the contract has no function to lend it"],
      ["If a token loses balance", "shared pro rata, never a freeze"],
    ],
  },
];

const TIME: Product[] = [
  {
    name: "Time deposit · Hold",
    icon: <IconClock />,
    pills: [
      { cls: "pill-mint", text: "100% reserve" },
      { cls: "pill-plum", text: "0% yield" },
    ],
    terms: [
      ["Term", "1 day to 5 years"],
      ["At maturity", "principal back to the owner"],
      ["Before maturity", "locked"],
    ],
  },
  {
    name: "Time deposit · Routed",
    icon: <IconClock />,
    pills: [{ cls: "pill-blush", text: "Variable adapter yield" }],
    terms: [
      ["Term", "1 day to 5 years"],
      ["At maturity", "exactly what the chosen yield adapter returns"],
      ["Loss", "borne by that deposit only"],
    ],
  },
];

function ProductCard({ p }: { p: Product }) {
  return (
    <article className="card-inset flex flex-col gap-3 sm:flex-row sm:items-center">
      <span className="flex min-w-0 flex-1 items-center gap-3">
        <span className="grid h-11 w-11 shrink-0 place-items-center rounded-2xl bg-plum-700 text-fuchsia">{p.icon}</span>
        <span className="min-w-0">
          <span className="block font-bold">{p.name}</span>
          <span className="mt-1 flex flex-wrap gap-1.5">
            {p.pills.map((x) => (
              <span key={x.text} className={x.cls}>{x.text}</span>
            ))}
          </span>
        </span>
      </span>
      <dl className="grid flex-[1.3] gap-1 text-sm">
        {p.terms.map(([k, val]) => (
          <div key={k} className="flex justify-between gap-3">
            <dt className="text-haze">{k}</dt>
            <dd className="text-right text-lavender">{val}</dd>
          </div>
        ))}
      </dl>
      <button type="button" className="btn-secondary shrink-0" disabled title="The bank contracts are not connected in this build">
        Not in this build
      </button>
    </article>
  );
}

/** Sovereign Bank products behind a Demand / Time segmented control. Informational until the bank is wired into a build. */
export function BankCard() {
  const [tab, setTab] = useState<"demand" | "time">("demand");
  return (
    <Card aria-labelledby="bank-title">
      <CardHeader
        icon={<IconBank />}
        title={<span id="bank-title">Sovereign Bank</span>}
        subtitle="Segregated deposits: never lent, staked or rehypothecated"
      />
      <Segmented
        idPrefix="bank"
        label="Deposit type"
        value={tab}
        onChange={setTab}
        options={[
          { value: "demand", label: "Demand Deposits" },
          { value: "time", label: "Time Deposits (Lockups)" },
        ]}
        className="mb-4 w-full [&>button]:flex-1 sm:w-auto sm:[&>button]:flex-none"
      />
      <TabPanel idPrefix="bank" value="demand" active={tab === "demand"}>
        <div className="space-y-3">
          {DEMAND.map((p) => (
            <ProductCard key={p.name} p={p} />
          ))}
        </div>
      </TabPanel>
      <TabPanel idPrefix="bank" value="time" active={tab === "time"}>
        <div className="space-y-3">
          {TIME.map((p) => (
            <ProductCard key={p.name} p={p} />
          ))}
        </div>
      </TabPanel>
      <Notice tone="info" className="mt-4">
        This build has no bank address, so deposits open here once the bank contracts are connected. The terms above are enforced by the contract
        code itself.
      </Notice>
    </Card>
  );
}
