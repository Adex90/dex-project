import Link from "next/link";
import { Ticker } from "@/components/Ticker";

const RULES: [string, string][] = [
  ["01", "100% RESERVE. Demand deposits sit in a contract that has no lend function. Not a toggle: it is not in the bytecode."],
  ["02", "ZERO INFLATION. Berrycoin's whole supply is minted once, in the first block. There is no mint to call."],
  ["03", "ZERO FEES. Arbitrage bots pay the Treasury for the right to rebalance the pool. Retail pays nothing."],
  ["04", "NO RUG. A fund manager can trade through audited adapters. They cannot withdraw. There is no such function."],
];

export default function Home() {
  return (
    <>
      <Ticker />
      <main className="mx-auto max-w-5xl px-4 pb-24 pt-10">
        <p className="label">Est. block 0 · No admin · No mint · No fee</p>
        <h1 className="mt-3 font-slab text-6xl uppercase leading-[0.9] tracking-tight md:text-9xl">
          <span className="ransom bg-paper text-ink">Straw</span>
          <span className="ransom -rotate-3 bg-berry text-ink">berry</span>
          <br />
          <span className="ransom rotate-1 border-2 border-acid text-acid">swap</span>
        </h1>
        <p className="mt-6 max-w-2xl font-mono text-sm text-ash">
          A broker that cannot lend your deposit, cannot print money, and cannot be switched off. Institutional
          plumbing, photocopied and stapled to a lamppost.
        </p>
        <Link href="/trade/" className="btn mt-8 inline-block">Open terminal →</Link>

        <ol className="mt-16 grid gap-6 md:grid-cols-2">
          {RULES.map(([n, t]) => (
            <li key={n} className="panel p-4">
              <span className="sticker">Rule {n}</span>
              <p className="mt-3 font-mono text-sm">{t}</p>
            </li>
          ))}
        </ol>
      </main>
    </>
  );
}
