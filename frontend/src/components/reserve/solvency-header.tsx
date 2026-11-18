"use client";

import { useState } from "react";
import { Check, Copy, Landmark, Layers } from "lucide-react";

import { HashText, Money, formatDateTime } from "@/components/ui/data";
import { Alert, Button, Skeleton, cx } from "@/components/ui/primitives";
import type { ActivoCorte, CorteReserva } from "@/lib/api/types";
import { writeToClipboard } from "@/lib/clipboard";
import { barShare, formatUnits, reserveRatioPct } from "@/lib/solvency";

/**
 * The bank's public commitment, before any login matters: the latest
 * snapshot's reserve root, when it was taken, and the reserve ratio, custody
 * over what the bank owes its clients. The same snapshot is public at
 * `GET /api/v1/reserva/cortes/{id}`, with its anchors in the cash and market
 * ledgers and one root per asset clients hold.
 */
export function SolvencyHeader({
  cut,
  missing,
  error,
  onRetry,
}: {
  cut: CorteReserva | null;
  /** No snapshot has been published yet. */
  missing: boolean;
  error: string | null;
  onRetry: () => void;
}) {
  return (
    <section
      aria-labelledby="solvency-title"
      className="border border-border bg-surface"
      data-testid="solvency-header"
      data-status={cut ? "loaded" : missing ? "missing" : error ? "error" : "loading"}
    >
      <div className="space-y-6 p-5 sm:p-7">
        <div className="space-y-2">
          <p className="text-[11px] font-semibold tracking-[0.24em] text-synthetic-yellow uppercase">
            Proof of Reserve · public snapshot
          </p>
          <h2 id="solvency-title" className="text-2xl font-semibold tracking-tight text-ink sm:text-3xl">
            Solvency Explorer
          </h2>
          <p className="max-w-3xl text-sm leading-relaxed text-ink-2">
            Every peso we owe a client is a leaf in a Merkle sum tree, and we publish its root. Your browser can rebuild
            the path from your own leaf to that root, so you never have to take our word for it.
          </p>
        </div>

        {cut ? <Snapshot cut={cut} /> : null}
        {!cut && missing ? (
          <Alert tone="info" title="No snapshot yet">
            The first Proof of Reserve snapshot has not been published. It covers every account as soon as it is.
          </Alert>
        ) : null}
        {!cut && error ? (
          <Alert tone="error" title="Could not load the published snapshot">
            <p>{error}</p>
            <Button type="button" variant="secondary" size="sm" className="mt-2" onClick={onRetry}>
              Retry
            </Button>
          </Alert>
        ) : null}
        {!cut && !missing && !error ? (
          <div className="space-y-3" role="status" aria-busy="true">
            <span className="sr-only">Loading the published snapshot…</span>
            <Skeleton className="h-16 w-full" />
            <Skeleton className="h-24 w-full" />
          </div>
        ) : null}
      </div>
    </section>
  );
}

function Snapshot({ cut }: { cut: CorteReserva }) {
  const ratio = reserveRatioPct(cut.total_reserva, cut.total_pasivo);
  return (
    <>
      <div className="grid grid-cols-1 gap-6 lg:grid-cols-[minmax(0,1fr)_auto] lg:items-end">
        <div className="min-w-0 space-y-3">
          <p className="text-[11px] font-semibold tracking-[0.2em] text-muted uppercase">Reserve root</p>
          <p className="font-mono text-sm leading-relaxed break-all text-ink sm:text-base" data-testid="reserve-root">
            {cut.merkle_root}
          </p>
          <div className="flex flex-wrap items-center gap-x-5 gap-y-2 text-xs text-muted">
            <CopyRoot root={cut.merkle_root} />
            <span data-testid="snapshot-time">
              Snapshot <span className="text-ink-2 tabular-nums">{formatDateTime(cut.creado_en)}</span>
            </span>
            <span>
              <span className="text-ink-2 tabular-nums">{cut.num_hojas.toLocaleString("en-US")}</span> accounts in the
              tree
            </span>
          </div>
        </div>
        <div className="border-l-2 border-synthetic-yellow pl-5" data-testid="reserve-ratio-block">
          <p className="text-[11px] font-semibold tracking-[0.2em] text-muted uppercase">Reserve ratio</p>
          <p
            className="font-mono text-5xl font-semibold tracking-tight text-synthetic-yellow tabular-nums"
            data-testid="reserve-ratio"
          >
            {ratio === null ? "—" : `${ratio}%`}
          </p>
          <p className="text-xs text-muted">Cash in custody ÷ what we owe clients</p>
        </div>
      </div>

      <dl className="grid gap-4 sm:grid-cols-2" aria-label="Custody against liabilities">
        <Bar
          label="Total custody assets"
          testId="custody-total"
          amount={cut.total_reserva}
          share={barShare(cut.total_reserva, cut.total_pasivo)}
        />
        <Bar
          label="Total client liabilities"
          testId="liabilities-total"
          amount={cut.total_pasivo}
          share={barShare(cut.total_pasivo, cut.total_reserva)}
        />
      </dl>

      <dl className="grid gap-px bg-border ring-1 ring-border sm:grid-cols-3">
        <Anchor label="Cash ledger anchor" sequence={cut.secuencia_ledger} hash={cut.hash_ledger} />
        <Anchor label="Market ledger anchor" sequence={cut.secuencia_mercado ?? null} hash={cut.hash_mercado ?? null} />
        <div className="bg-surface-2 px-4 py-3">
          <dt className="text-[11px] font-medium tracking-[0.16em] text-muted uppercase">Algorithm</dt>
          <dd className="mt-1 font-mono text-xs text-ink-2">{cut.algoritmo}</dd>
        </div>
      </dl>

      {cut.activos?.length ? <AssetRoots activos={cut.activos} /> : null}
    </>
  );
}

function Bar({ label, testId, amount, share }: { label: string; testId: string; amount: string; share: number }) {
  return (
    <div className="space-y-2">
      <dt className="flex items-center gap-1.5 text-[11px] font-semibold tracking-[0.16em] text-muted uppercase">
        <Landmark className="size-3.5 text-synthetic-yellow" aria-hidden />
        {label}
      </dt>
      <dd className="space-y-2">
        <Money value={amount} className="text-xl font-semibold text-ink" />
        <span className="block h-2 bg-surface-2" aria-hidden data-testid={testId} data-amount={amount}>
          <span className="block h-full bg-synthetic-yellow" style={{ width: `${share}%` }} />
        </span>
      </dd>
    </div>
  );
}

function Anchor({ label, sequence, hash }: { label: string; sequence: number | null; hash: string | null }) {
  return (
    <div className="min-w-0 bg-surface-2 px-4 py-3">
      <dt className="text-[11px] font-medium tracking-[0.16em] text-muted uppercase">{label}</dt>
      <dd className="mt-1 flex min-w-0 items-center gap-2 text-xs text-ink-2">
        {sequence !== null && hash ? (
          <>
            <span className="font-mono tabular-nums">#{sequence}</span>
            <HashText value={hash} head={8} tail={4} label={label.toLowerCase()} />
          </>
        ) : (
          <span className="text-muted">Not anchored (older snapshot)</span>
        )}
      </dd>
    </div>
  );
}

function AssetRoots({ activos }: { activos: ActivoCorte[] }) {
  return (
    <section aria-labelledby="asset-roots-title" className="space-y-3" data-testid="asset-roots">
      <div className="space-y-1">
        <h3 id="asset-roots-title" className="flex items-center gap-2 text-sm font-semibold text-ink">
          <Layers className="size-4 text-synthetic-yellow" aria-hidden />
          Asset roots
        </h3>
        <p className="max-w-3xl text-xs leading-relaxed text-muted">
          Each asset clients hold has its own tree, committed in the same snapshot. Its root totals the units owed to
          clients. Custody of the underlying assets is not attested yet, so these roots prove that your positions are
          included and counted, not that they are backed.
        </p>
      </div>
      {/* Rows that stack on a phone and line up as columns from sm up. */}
      <ul className="divide-y divide-border border border-border">
        <li
          aria-hidden
          className="hidden grid-cols-[5rem_minmax(0,1fr)_4rem_minmax(0,15rem)] gap-3 bg-surface-2 px-4 py-2 text-[11px] font-semibold tracking-[0.14em] text-muted uppercase sm:grid"
        >
          <span>Asset</span>
          <span className="text-right">Owed to clients</span>
          <span className="text-right">Leaves</span>
          <span>Root</span>
        </li>
        {activos.map((a) => (
          <li
            key={a.ticker}
            className="grid grid-cols-[minmax(0,1fr)_auto] gap-x-3 gap-y-1 px-4 py-2.5 text-sm sm:grid-cols-[5rem_minmax(0,1fr)_4rem_minmax(0,15rem)] sm:items-center"
            data-testid="asset-root"
            data-ticker={a.ticker}
          >
            <span className="font-mono font-semibold text-ink">{a.ticker}</span>
            <span className="text-right font-mono text-ink-2 tabular-nums">
              <span className="font-sans text-xs text-muted sm:sr-only">Owed </span>
              {formatUnits(a.total_cantidad)}
            </span>
            <span className="text-xs text-muted tabular-nums sm:text-right sm:text-sm sm:text-ink-2">
              <span className="sm:sr-only">Leaves </span>
              {a.num_hojas}
            </span>
            <span className="min-w-0 text-right sm:text-left">
              <HashText value={a.merkle_root} head={10} tail={6} label={`${a.ticker} root`} />
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}

function CopyRoot({ root }: { root: string }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    // The Clipboard API, or the textarea fallback where the browser refuses it.
    if (!(await writeToClipboard(root))) return;
    setCopied(true);
    setTimeout(() => setCopied(false), 1600);
  };
  return (
    <button
      type="button"
      onClick={copy}
      className={cx(
        "inline-flex h-9 items-center gap-2 border px-3 text-[11px] font-semibold tracking-[0.16em] uppercase transition-colors",
        "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none",
        copied
          ? "border-synthetic-yellow bg-synthetic-yellow text-black"
          : "border-synthetic-yellow/60 text-synthetic-yellow hover:border-synthetic-yellow hover:bg-synthetic-yellow/10",
      )}
      data-testid="copy-root"
      aria-live="polite"
    >
      {copied ? <Check className="size-3.5" aria-hidden /> : <Copy className="size-3.5" aria-hidden />}
      {copied ? "Copied" : "Copy root"}
    </button>
  );
}
