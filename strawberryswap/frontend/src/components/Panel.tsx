import type { ReactNode } from "react";

export function Panel({ title, tag, children, className = "" }: { title: string; tag?: string; children: ReactNode; className?: string }) {
  return (
    <section className={`panel ${className}`}>
      <header className="panel-head">
        <h2>{title}</h2>
        {tag ? <span className="opacity-70">{tag}</span> : null}
      </header>
      <div className="p-4">{children}</div>
    </section>
  );
}

export function Stat({ label, value, sub, tone = "paper" }: { label: string; value: string; sub?: string; tone?: "paper" | "acid" | "berry" | "ice" }) {
  const color = { paper: "text-paper", acid: "text-acid", berry: "text-berry", ice: "text-ice" }[tone];
  return (
    <div className="border-l-4 border-rule pl-3">
      <div className="label">{label}</div>
      <div className={`num text-xl font-bold ${color}`}>{value}</div>
      {sub ? <div className="num text-xs text-ash">{sub}</div> : null}
    </div>
  );
}

export function Notice({ tone, children }: { tone: "warn" | "err" | "ok"; children: ReactNode }) {
  const style = {
    warn: "border-acid text-acid",
    err: "border-berry text-berry",
    ok: "border-ice text-ice",
  }[tone];
  return <p role={tone === "err" ? "alert" : "status"} className={`mt-3 break-words border-2 border-dashed px-3 py-2 font-mono text-xs ${style}`}>{children}</p>;
}
