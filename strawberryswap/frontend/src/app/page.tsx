import Link from "next/link";
import { IconBank, IconShield, IconVault, IconZap } from "@/components/ui/icons";
import { Logo, Mascot } from "@/components/ui/Mascot";
import { SiteFooter } from "@/components/shell/Footer";

const PRINCIPLES = [
  { name: "Full reserve", scope: "Demand deposits", Icon: IconBank, text: "Held in a contract whose code contains no function to lend, stake or rehypothecate them. This is a property of the bytecode, not a setting." },
  { name: "Fixed supply", scope: "Berrycoin", Icon: IconShield, text: "The entire supply is created once, in the genesis block. The deployed contract has no mint function and no administrator." },
  { name: "Zero retail fee", scope: "Dark pool", Icon: IconZap, text: "Arbitrageurs pay the Treasury for the right to rebalance the pool toward the oracle price. Retail traders pay no trading fee." },
  { name: "No custody risk", scope: "Strategy vaults", Icon: IconVault, text: "A manager may trade through audited adapters under per-trade limits. No function exists by which a manager can withdraw fund assets." },
] as const;

const FLOW: [string, string][] = [
  ["Shield", "Deposit tokens into private notes. Only this edge is public."],
  ["Swap privately", "Your browser proves the trade; a relay batches it with others at one uniform price."],
  ["Unshield", "Withdraw your notes to any address, whenever you like."],
];

export default function Home() {
  return (
    <>
      <header className="mx-auto flex max-w-6xl items-center justify-between px-4 py-4 sm:px-6">
        <Logo />
        <Link href="/trade/" className="btn-primary !px-5 !py-2.5 text-sm no-underline">
          Launch app
        </Link>
      </header>

      <main className="mx-auto max-w-6xl px-4 pb-16 sm:px-6">
        {/* ------------------------------------------------------------------------------------------------ hero */}
        <section className="grid items-center gap-10 pb-16 pt-8 md:grid-cols-[1.2fr_1fr] md:pt-16">
          <div>
            <span className="pill-mint">0% retail fee · 100% reserve</span>
            <h1 className="mt-5 text-balance text-4xl font-extrabold leading-[1.05] tracking-tight sm:text-6xl">
              A broker that <span className="text-gradient">cannot</span> lend your deposit, print money, or be switched off.
            </h1>
            <p className="mt-5 max-w-xl text-lg text-lavender">
              StrawberrySwap is a trading venue built from immutable smart contracts: a zero-fee private dark pool, fully collateralised derivatives,
              a segregated deposit bank and non-custodial strategy vaults.
            </p>
            <div className="mt-8 flex flex-wrap gap-3">
              <Link href="/trade/" className="btn-primary px-8 py-4 text-lg no-underline">
                Start trading privately
              </Link>
              <Link href="/earn/" className="btn-secondary px-6 py-4 text-base no-underline">
                Explore Earn
              </Link>
            </div>
          </div>
          <div className="relative mx-auto grid h-72 w-72 place-items-center sm:h-80 sm:w-80">
            <div className="absolute inset-6 rounded-full bg-gradient-to-br from-fuchsia/40 via-berry/20 to-plum-500/30 blur-3xl" aria-hidden />
            <div className="absolute inset-0 rounded-full border border-fuchsia/20" aria-hidden />
            <div className="absolute inset-10 rounded-full border border-lavender/10" aria-hidden />
            <Mascot className="relative h-56 w-56 drop-shadow-[0_20px_40px_rgba(255,42,133,0.45)] sm:h-64 sm:w-64" float title="Berri, the StrawberrySwap mascot" />
          </div>
        </section>

        {/* ------------------------------------------------------------------------------------------------ flow */}
        <section aria-labelledby="flow-title" className="card">
          <h2 id="flow-title" className="text-2xl font-bold">How a private swap works</h2>
          <ol className="mt-5 grid gap-4 md:grid-cols-3">
            {FLOW.map(([title, text], i) => (
              <li key={title} className="card-inset">
                <span className="grid h-9 w-9 place-items-center rounded-full bg-gradient-to-br from-berry-deep to-fuchsia-deep font-bold text-white shadow-glow-fuchsia">{i + 1}</span>
                <h3 className="mt-3 text-lg font-bold">{title}</h3>
                <p className="mt-1 text-sm text-lavender">{text}</p>
              </li>
            ))}
          </ol>
        </section>

        {/* ------------------------------------------------------------------------------------------------ principles */}
        <section aria-labelledby="principles-title" className="mt-12">
          <h2 id="principles-title" className="text-2xl font-bold">Design principles</h2>
          <div className="mt-5 grid gap-4 sm:grid-cols-2">
            {PRINCIPLES.map(({ name, scope, text, Icon }) => (
              <article key={name} className="card transition duration-300 hover:-translate-y-1 hover:shadow-glow-fuchsia">
                <div className="flex items-center gap-3">
                  <span className="grid h-11 w-11 place-items-center rounded-2xl bg-plum-700 text-fuchsia shadow-inset">
                    <Icon />
                  </span>
                  <div>
                    <h3 className="text-lg font-bold">{name}</h3>
                    <p className="eyebrow">{scope}</p>
                  </div>
                </div>
                <p className="mt-3 text-lavender">{text}</p>
              </article>
            ))}
          </div>
        </section>

        <p className="mt-16 text-center text-xs text-haze">This page describes intended behaviour of the contracts, not a guarantee.</p>
      </main>
      <SiteFooter />
    </>
  );
}
