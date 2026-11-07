import Link from "next/link";

const PRINCIPLES: [string, string, string][] = [
  ["Full reserve", "Demand deposits", "Held in a contract whose code contains no function to lend, stake or rehypothecate them. This is a property of the bytecode, not a setting."],
  ["Fixed supply", "Berrycoin", "The entire supply is created once, in the genesis block. The deployed contract has no mint function and no administrator."],
  ["Zero retail fee", "Dark pool", "Arbitrageurs pay the Treasury for the right to rebalance the pool toward the oracle price. Retail traders pay no trading fee."],
  ["No custody risk", "Strategy vaults", "A manager may trade through audited adapters under per-trade limits. No function exists by which a manager can withdraw fund assets."],
];

export default function Home() {
  return (
    <>
      <div className="border-b border-rule">
        <div className="mx-auto flex max-w-3xl items-center justify-between px-6 py-3">
          <span className="font-serif text-lg tracking-wide" style={{ fontVariantCaps: "small-caps" }}>StrawberrySwap</span>
          <Link href="/trade/" className="label text-accent no-underline hover:underline">Terminal →</Link>
        </div>
      </div>

      <main className="mx-auto max-w-3xl px-6 pb-24 pt-16">
        <p className="label">Technical Overview · Revision 1</p>
        <h1 className="mt-3 text-balance font-serif text-5xl font-normal leading-[1.1]">
          A broker that cannot lend your deposit, print money, or be switched off.
        </h1>
        <p className="mt-6 text-lg italic text-muted">
          StrawberrySwap is an institutional trading venue built from immutable smart contracts: a zero-fee dark pool,
          fully collateralised derivatives, a segregated deposit bank and non-custodial strategy vaults.
        </p>

        <div className="mt-8">
          <Link href="/trade/" className="btn inline-block no-underline">Open the trading terminal</Link>
        </div>

        <section className="mt-20">
          <h2 className="section-title font-normal"><span className="section-no">§ 1</span>Design principles</h2>
          <ol className="mt-2">
            {PRINCIPLES.map(([name, scope, text], i) => (
              <li key={name} className="grid gap-x-6 border-b border-rule/60 py-5 sm:grid-cols-[9.5rem_1fr]">
                <div>
                  <span className="section-no">{i + 1}.</span> <span className="label">{scope}</span>
                </div>
                <p>
                  <span className="font-semibold">{name}.</span> {text}
                </p>
              </li>
            ))}
          </ol>
        </section>

        <footer className="mt-20 text-[13px] text-muted">
          Unaudited software. Testnet use only. This page describes intended behaviour of the contracts, not a guarantee.
        </footer>
      </main>
    </>
  );
}
