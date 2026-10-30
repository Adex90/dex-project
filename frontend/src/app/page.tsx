import Link from "next/link";
import { ArrowRight, CalendarClock, Fingerprint, Landmark } from "lucide-react";

import { Brand } from "@/components/brand";
import { ReserveTicker } from "@/components/reserve-ticker";

const PRINCIPLES = [
  {
    icon: Landmark,
    title: "100% reserve vault",
    body: "Every peso you deposit stays in custody as a real, liquid peso. Vault money is never lent, and the database rejects any transaction that would break that rule.",
  },
  {
    icon: CalendarClock,
    title: "Lending only from time deposits",
    body: "Loans are funded only by money you choose to lock in a fixed-term mutuum contract, and only to borrowers whose loans mature on or before your contract does.",
  },
  {
    icon: Fingerprint,
    title: "Proof of Reserve you can check",
    body: "Every balance is a leaf in a Merkle sum tree. Your browser recomputes the published root from your own leaf, without trusting us.",
  },
];

export default function Home() {
  return (
    <div className="mx-auto flex min-h-dvh max-w-6xl flex-col px-5 sm:px-8">
      <header className="flex items-center justify-between py-6">
        <Brand />
        <nav className="flex items-center gap-2 text-sm">
          <Link href="/auth/login" className="rounded-lg px-3 py-2 text-ink-2 hover:bg-surface-2 hover:text-ink">
            Sign in
          </Link>
          <Link
            href="/auth/register"
            className="rounded-lg bg-accent px-3.5 py-2 font-medium text-accent-ink shadow-sm hover:brightness-110"
          >
            Open an account
          </Link>
        </nav>
      </header>

      <main className="flex flex-1 flex-col justify-center gap-14 py-12">
        <section className="max-w-3xl space-y-6">
          <p className="text-xs font-semibold tracking-[0.18em] text-accent uppercase">
            Banking on the Austrian school
          </p>
          <h1 className="text-4xl font-semibold tracking-tight text-balance text-ink sm:text-5xl">
            Your money is actually there.
            <span className="text-muted"> All of it, all the time.</span>
          </h1>
          <p className="max-w-2xl text-lg leading-relaxed text-ink-2">
            Synthetic Capital is a 100% reserve bank in Mexico: no fractional reserve and no maturity mismatch, plus
            cryptographic proof that your balance is backed.
          </p>
          <div className="flex flex-wrap items-center gap-3">
            <Link
              href="/auth/register"
              className="inline-flex h-11 items-center gap-2 rounded-lg bg-accent px-5 text-sm font-medium text-accent-ink shadow-sm hover:brightness-110"
            >
              Open an account <ArrowRight className="size-4" aria-hidden />
            </Link>
            <Link
              href="/auth/login"
              className="inline-flex h-11 items-center rounded-lg border border-border-strong bg-surface px-5 text-sm font-medium hover:bg-surface-2"
            >
              Sign in
            </Link>
          </div>
          <ReserveTicker />
        </section>

        <section className="grid gap-4 md:grid-cols-3">
          {PRINCIPLES.map(({ icon: Icon, title, body }) => (
            <article key={title} className="rounded-xl border border-border bg-surface p-5 shadow-sm">
              <Icon className="size-5 text-accent" aria-hidden />
              <h2 className="mt-3 text-sm font-semibold text-ink">{title}</h2>
              <p className="mt-1.5 text-sm leading-relaxed text-ink-2">{body}</p>
            </article>
          ))}
        </section>
      </main>

      <footer className="border-t border-border py-6 text-xs text-muted">
        IFPE (electronic payment fund institution) and SOFOM (multiple-purpose financial company) operate as segregated
        entities.
      </footer>
    </div>
  );
}
