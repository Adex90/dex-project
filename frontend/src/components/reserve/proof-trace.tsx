"use client";

import type { ReactNode } from "react";
import { ArrowUp, GitBranch, ShieldCheck, ShieldX } from "lucide-react";

import { HashText, Money } from "@/components/ui/data";
import { cx } from "@/components/ui/primitives";
import type { VerificationResult } from "@/lib/merkle-verifier";
import { formatUnits } from "@/lib/solvency";

/** One tree's path, as the browser recomputed it. */
export interface Trace {
  /** "cash" or the asset's ticker. */
  key: string;
  label: string;
  kind: "cash" | "position";
  result: VerificationResult;
  /** The root the bank published for this tree, from its public copy when it could be fetched. */
  publishedRoot: string | null;
  /** The computed root is the public one and every check on this tree passed. */
  match: boolean;
}

/**
 * The hash chain from the customer's leaf to the published root, one level at
 * a time: at each level the two children (in hashing order, with their sums)
 * and the node their SHA-256 produces. It ends on the cryptographic seal: the
 * root computed here against the root the bank published.
 */
export function ProofTrace({
  traces,
  selected,
  onSelect,
  revealed,
}: {
  traces: Trace[];
  selected: string;
  onSelect: (key: string) => void;
  /** Nodes of the selected trace shown so far (leaf, levels, root), for the reveal. */
  revealed: number;
}) {
  const trace = traces.find((t) => t.key === selected) ?? traces[0] ?? null;
  return (
    <section aria-labelledby="trace-title" className="border border-border bg-surface" data-testid="proof-trace">
      <div className="space-y-5 p-5 sm:p-7">
        <div className="space-y-1">
          <p className="text-[11px] font-semibold tracking-[0.24em] text-synthetic-yellow uppercase">
            Step-by-step proof trace
          </p>
          <h3 id="trace-title" className="text-lg font-semibold text-ink">
            From your leaf to the published root
          </h3>
        </div>

        {trace === null ? (
          <div
            className="space-y-3 border border-dashed border-border-strong p-5 text-sm text-ink-2"
            data-testid="trace-idle"
          >
            <GitBranch className="size-5 text-synthetic-yellow" aria-hidden />
            <p>
              Press <span className="font-semibold text-ink">Verify My Inclusion</span>. Your browser hashes your leaf,
              then each level with its sibling, and compares the root it reaches with the one we published.
            </p>
            <p className="text-xs text-muted">
              Each parent commits to both children and their sums, so a hidden or altered balance anywhere on the way
              changes the root.
            </p>
          </div>
        ) : (
          <>
            {traces.length > 1 ? <Tabs traces={traces} selected={trace.key} onSelect={onSelect} /> : null}
            <Chain trace={trace} revealed={revealed} />
          </>
        )}
      </div>
    </section>
  );
}

function Tabs({ traces, selected, onSelect }: { traces: Trace[]; selected: string; onSelect: (key: string) => void }) {
  return (
    <div role="tablist" aria-label="Trees" className="flex flex-wrap gap-px border border-border bg-border">
      {traces.map((t) => (
        <button
          key={t.key}
          type="button"
          role="tab"
          aria-selected={t.key === selected}
          aria-controls="trace-chain"
          onClick={() => onSelect(t.key)}
          className={cx(
            "flex flex-1 items-center justify-center gap-1.5 px-3 py-2 text-xs font-semibold tracking-[0.14em] uppercase transition-colors",
            "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none focus-visible:ring-inset",
            t.key === selected ? "bg-synthetic-yellow text-black" : "bg-surface text-ink-2 hover:bg-surface-2",
          )}
          data-testid="trace-tab"
          data-key={t.key}
        >
          {t.match ? <ShieldCheck className="size-3.5" aria-hidden /> : <ShieldX className="size-3.5" aria-hidden />}
          {t.label}
        </button>
      ))}
    </div>
  );
}

function Amount({ kind, value }: { kind: Trace["kind"]; value: string }) {
  return kind === "cash" ? (
    <Money value={value} />
  ) : (
    <span className="font-mono tabular-nums">{formatUnits(value)}</span>
  );
}

function Chain({ trace, revealed }: { trace: Trace; revealed: number }) {
  const { result, kind } = trace;
  const steps = result.steps;
  return (
    <div id="trace-chain" role="tabpanel" aria-label={`${trace.label} path`} className="space-y-0">
      <ol aria-label={`Hash chain from your ${kind === "cash" ? "cash" : trace.label} leaf to the root`}>
        <Node visible={revealed >= 1} last={false}>
          <div className="border-2 border-synthetic-yellow bg-synthetic-yellow/[0.05] p-3" data-testid="trace-leaf">
            <NodeHeader title={kind === "cash" ? "Leaf · your cash" : `Leaf · your ${trace.label}`}>
              Σ <Amount kind={kind} value={result.leafSum} />
            </NodeHeader>
            <p className="mt-1 font-mono text-[11px] leading-relaxed break-all text-ink">{result.leafHash}</p>
            <p className="mt-1 text-[11px] text-muted">
              SHA-256 of {kind === "cash" ? "“SC-POR-v1:hoja”" : "“SC-POR-v1:posicion” and the ticker"}, your account
              id, the amount and your salt
            </p>
          </div>
        </Node>
        {steps.map((step, i) => {
          const ownIsLeft = step.side === "DERECHA";
          return (
            <Node key={step.level} visible={revealed >= i + 2} last={false}>
              <div className="border border-border bg-surface-2 p-3" data-testid="trace-level">
                <NodeHeader title={`Level ${step.level}`}>
                  Σ <Amount kind={kind} value={step.nodeSum} />
                </NodeHeader>
                <dl className="mt-2 grid gap-1 text-[11px]">
                  <Child label="Left" own={ownIsLeft} hash={step.leftHash} sum={step.leftSum} kind={kind} />
                  <Child label="Right" own={!ownIsLeft} hash={step.rightHash} sum={step.rightSum} kind={kind} />
                </dl>
                <p className="mt-2 flex items-center gap-1.5 text-[11px] text-muted">
                  <ArrowUp className="size-3 text-synthetic-yellow" aria-hidden />
                  SHA-256(“SC-POR-v1:nodo” ‖ left ‖ Σ ‖ right ‖ Σ) =
                </p>
                <p className="font-mono text-[11px] break-all text-ink">{step.nodeHash}</p>
              </div>
            </Node>
          );
        })}
        <Node visible={revealed >= steps.length + 2} last>
          <Seal trace={trace} />
        </Node>
      </ol>
    </div>
  );
}

function Node({ visible, last, children }: { visible: boolean; last: boolean; children: ReactNode }) {
  if (!visible) {
    return <li className="mb-4 h-16 border border-dashed border-border" aria-hidden />;
  }
  return (
    <li className={cx("relative animate-reveal", !last && "pb-4")}>
      {children}
      {!last ? <span aria-hidden className="absolute bottom-0 left-6 h-4 w-px bg-synthetic-yellow/60" /> : null}
    </li>
  );
}

function NodeHeader({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
      <span className="text-[11px] font-semibold tracking-[0.18em] text-ink uppercase">{title}</span>
      <span className="text-xs text-ink-2 tabular-nums">{children}</span>
    </div>
  );
}

function Child({
  label,
  own,
  hash,
  sum,
  kind,
}: {
  label: string;
  own: boolean;
  hash: string;
  sum: string;
  kind: Trace["kind"];
}) {
  return (
    <div className="grid grid-cols-[3.25rem_minmax(0,1fr)_auto] items-center gap-2">
      <dt className="font-semibold tracking-[0.12em] text-muted uppercase">{label}</dt>
      <dd className="min-w-0">
        <span className={cx("inline-flex items-center gap-1.5", own ? "text-synthetic-yellow" : "text-ink-2")}>
          <HashText value={hash} head={12} tail={6} label={`${label.toLowerCase()} child hash`} />
          <span className="shrink-0 text-[10px] tracking-[0.12em] uppercase">{own ? "from below" : "sibling"}</span>
        </span>
      </dd>
      <dd className="text-right text-ink-2">
        <Amount kind={kind} value={sum} />
      </dd>
    </div>
  );
}

function Seal({ trace }: { trace: Trace }) {
  const { result, match } = trace;
  return (
    <div className="space-y-3" data-testid="trace-root">
      <div className={cx("border-2 p-3", match ? "border-synthetic-yellow" : "border-danger")}>
        <NodeHeader title="Computed root">
          Σ <Amount kind={trace.kind} value={result.rootSum} />
        </NodeHeader>
        <p className="mt-1 font-mono text-[11px] break-all text-ink" data-testid="computed-root">
          {result.rootHash}
        </p>
        <p className="mt-2 text-[11px] font-semibold tracking-[0.18em] text-muted uppercase">Published root</p>
        <p className="mt-1 font-mono text-[11px] break-all text-ink-2" data-testid="published-root">
          {trace.publishedRoot ?? "None published for this asset"}
        </p>
      </div>
      <div
        className={cx(
          "flex items-center gap-4 border-2 p-4",
          match ? "border-synthetic-yellow bg-synthetic-yellow/[0.08]" : "border-danger bg-danger-soft",
        )}
        data-testid="match-seal"
        data-status={match ? "match" : "mismatch"}
        role="status"
      >
        <span
          className={cx(
            "grid size-12 shrink-0 place-items-center",
            match ? "bg-synthetic-yellow text-black" : "bg-danger text-white",
          )}
          aria-hidden
        >
          {match ? <ShieldCheck className="size-6" /> : <ShieldX className="size-6" />}
        </span>
        <div className="min-w-0">
          <p
            className={cx(
              "text-[11px] font-bold tracking-[0.22em] uppercase",
              match ? "text-synthetic-yellow" : "text-danger",
            )}
          >
            {match ? "Cryptographic match" : "No match"}
          </p>
          <p className="text-sm font-semibold text-ink">
            {match ? "Computed root = published root" : "The computed root is not the published one"}
          </p>
          <p className="text-xs text-muted">
            {result.steps.length === 0
              ? "This tree has a single leaf, yours: its hash is the root."
              : `${result.steps.length} ${result.steps.length === 1 ? "level" : "levels"} hashed in your browser${
                  result.shapeMatches ? ", along the exact path your leaf's position dictates" : ""
                }.`}{" "}
            No server answer was trusted.
          </p>
        </div>
      </div>
    </div>
  );
}
