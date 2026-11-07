import type { ReactNode } from "react";

/** A numbered, ruled section (§ n Title). `tag` is a right-aligned caption. */
export function Panel({ title, tag, no, children, className = "" }: { title: string; tag?: string; no?: string; children: ReactNode; className?: string }) {
  return (
    <section className={className}>
      <header className="section-title">
        {no ? <span className="section-no">§ {no}</span> : null}
        <h2 className="font-normal">{title}</h2>
        {tag ? <span className="label ml-auto">{tag}</span> : null}
      </header>
      <div className="pt-4">{children}</div>
    </section>
  );
}

type Tone = "default" | "pos" | "neg" | "info" | "warn";

/** One figure: small-caps label, tabular value, optional sub-caption. */
export function Stat({ label, value, sub, tone = "default" }: { label: string; value: string; sub?: string; tone?: Tone }) {
  const color = { default: "text-text", pos: "text-pos", neg: "text-neg", info: "text-accent", warn: "text-warn" }[tone];
  return (
    <div>
      <div className="label">{label}</div>
      <div className={`num break-all text-xl ${color}`}>{value}</div>
      {sub ? <div className="num text-xs text-muted">{sub}</div> : null}
    </div>
  );
}

/** Marginal note: thin coloured rule on the left, small body text. */
export function Notice({ tone, children }: { tone: "warn" | "err" | "ok"; children: ReactNode }) {
  const style = {
    warn: "border-warn text-warn",
    err: "border-neg text-neg",
    ok: "border-pos text-pos",
  }[tone];
  return (
    <p role={tone === "err" ? "alert" : "status"} className={`mt-4 break-words border-l-2 py-0.5 pl-3 text-[14px] leading-snug ${style}`}>
      {children}
    </p>
  );
}
