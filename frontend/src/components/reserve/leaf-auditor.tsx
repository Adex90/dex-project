"use client";

import { CheckCircle2, Clock, Download, Fingerprint, Loader2, Server, ShieldCheck, XCircle } from "lucide-react";

import { HashText, Money } from "@/components/ui/data";
import { Alert, Button, Skeleton, cx } from "@/components/ui/primitives";
import type { PruebaReserva, ResultadoVerificacionPrueba } from "@/lib/api/types";
import { compareAmounts } from "@/lib/decimal";
import type { Engine, VerificationResult } from "@/lib/merkle-verifier";
import { ISSUE_MESSAGES, type Verdict } from "@/lib/proof-verdict";
import { formatUnits, includedBalances, positionRows } from "@/lib/solvency";
import type { Holding } from "@/types/vault";

export type ProofLoad =
  | { status: "loading" }
  | { status: "ready"; proof: PruebaReserva }
  | { status: "pending"; message: string }
  | { status: "error"; message: string };

/** What the browser established, once the customer asked. */
export interface AuditOutcome {
  proof: PruebaReserva;
  cash: VerificationResult;
  verdict: Verdict;
  engine: Engine;
  publicCopy: "matches" | "differs" | "unavailable";
}

/**
 * The customer's own leaf: its hash, the balances it commits to (cash, line
 * by line, and each asset position in its own tree), and the gold button that
 * checks it all in the browser. Funds on hold appear with the neutral copy
 * every client screen uses.
 */
export function LeafAuditor({
  load,
  holdings,
  accountId,
  audit,
  server,
  verifying,
  verifyError,
  onVerify,
  onRetry,
}: {
  load: ProofLoad;
  holdings: readonly Holding[];
  accountId: string | null;
  audit: AuditOutcome | null;
  server: ResultadoVerificacionPrueba | null;
  verifying: boolean;
  verifyError: string | null;
  onVerify: () => void;
  onRetry: () => void;
}) {
  const proof = audit?.proof ?? (load.status === "ready" ? load.proof : null);
  return (
    <section aria-labelledby="leaf-title" className="border border-border bg-surface" data-testid="leaf-auditor">
      <div className="space-y-6 p-5 sm:p-7">
        <div className="flex items-start justify-between gap-4">
          <div className="space-y-1">
            <p className="text-[11px] font-semibold tracking-[0.24em] text-synthetic-yellow uppercase">Your leaf</p>
            <h3 id="leaf-title" className="text-lg font-semibold text-ink">
              Leaf node auditor
            </h3>
            <p className="text-xs leading-relaxed text-muted">
              What the snapshot committed for your account. Verify it against the public root.
            </p>
          </div>
          {proof ? <DownloadProof proof={proof} /> : null}
        </div>

        {load.status === "loading" && !proof ? (
          <div className="space-y-3" role="status" aria-busy="true">
            <span className="sr-only">Loading your leaf…</span>
            <Skeleton className="h-14 w-full" />
            <Skeleton className="h-28 w-full" />
          </div>
        ) : null}
        {load.status === "pending" && !proof ? (
          <Alert tone="info" title="Your account joins the next snapshot">
            {load.message}
          </Alert>
        ) : null}
        {load.status === "error" && !proof ? (
          <Alert tone="error" title="Could not load your leaf">
            <p>{load.message}</p>
            <Button type="button" variant="secondary" size="sm" className="mt-2" onClick={onRetry}>
              Retry
            </Button>
          </Alert>
        ) : null}

        {proof ? <Leaf proof={proof} accountId={accountId} audit={audit} holdings={holdings} /> : null}

        <button
          type="button"
          onClick={onVerify}
          disabled={verifying || load.status === "loading"}
          className={cx(
            "inline-flex h-14 w-full items-center justify-center gap-2 bg-synthetic-yellow text-sm font-bold tracking-[0.18em] text-black uppercase transition-colors",
            "hover:bg-synthetic-yellow-hover active:bg-synthetic-yellow-pressed",
            "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
            "disabled:cursor-not-allowed disabled:opacity-50",
          )}
          data-testid="verify-inclusion"
        >
          {verifying ? (
            <Loader2 className="size-5 animate-spin" aria-hidden />
          ) : (
            <ShieldCheck className="size-5" aria-hidden />
          )}
          {verifying ? "Verifying…" : audit || verifyError ? "Verify again" : "Verify My Inclusion"}
        </button>

        {verifyError ? <Alert tone="error">{verifyError}</Alert> : null}
        {audit ? <Outcome audit={audit} server={server} /> : null}
      </div>
    </section>
  );
}

function Leaf({
  proof,
  accountId,
  audit,
  holdings,
}: {
  proof: PruebaReserva;
  accountId: string | null;
  audit: AuditOutcome | null;
  holdings: readonly Holding[];
}) {
  const lines = includedBalances(proof.desglose ?? null);
  const changed = compareAmounts(proof.saldo_actual, proof.hoja.saldo) !== 0;
  const yours = accountId !== null && proof.hoja.cuenta_id.toLowerCase() === accountId.toLowerCase();
  const rows = positionRows(proof.posiciones ?? [], holdings);
  const leafHash = audit?.cash.leafHash ?? proof.hoja.hash ?? "—";
  const recomputed = audit !== null && audit.cash.leafHashMatches !== false;

  return (
    <div className="space-y-6">
      <div className="space-y-2 border-l-2 border-synthetic-yellow pl-4">
        <p className="flex flex-wrap items-center gap-2 text-[11px] font-semibold tracking-[0.2em] text-muted uppercase">
          <Fingerprint className="size-3.5 text-synthetic-yellow" aria-hidden />
          Leaf hash
          <span
            className={cx(
              "border px-1.5 py-px text-[10px] tracking-[0.14em]",
              audit === null
                ? "border-border-strong text-muted"
                : recomputed
                  ? "border-synthetic-yellow text-synthetic-yellow"
                  : "border-danger text-danger",
            )}
            data-testid="leaf-hash-source"
          >
            {audit === null ? "As published" : recomputed ? "Recomputed in your browser" : "Does not match"}
          </span>
        </p>
        <p className="font-mono text-xs leading-relaxed break-all text-ink sm:text-sm" data-testid="leaf-hash">
          {leafHash}
        </p>
        <p className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs text-muted">
          <span>
            Leaf index <span className="font-mono text-ink-2">{proof.indice}</span> ·{" "}
            {proof.corte.num_hojas.toLocaleString("en-US")} leaves
          </span>
          <span>
            Account <span className="font-mono text-ink-2">{proof.hoja.cuenta_id.slice(0, 8)}…</span>
            {yours ? (
              <span className="text-ink-2"> (yours)</span>
            ) : (
              <span className="text-danger"> (not your account)</span>
            )}
          </span>
          <span className="inline-flex items-center gap-1">
            Salt <HashText value={proof.hoja.sal} head={6} tail={4} label="salt" />
          </span>
        </p>
      </div>

      <section aria-labelledby="balances-title" className="space-y-2" data-testid="included-balances">
        <h4 id="balances-title" className="text-[11px] font-semibold tracking-[0.2em] text-ink-2 uppercase">
          Included balances · cash
        </h4>
        <dl className="divide-y divide-border border border-border">
          {lines.map((line) => (
            <div
              key={line.key}
              className="flex items-baseline justify-between gap-4 px-4 py-2.5 text-sm"
              data-testid="balance-line"
              data-key={line.key}
            >
              <dt className="text-ink-2">{line.label}</dt>
              <dd>
                <Money value={line.amount} className="text-ink" />
              </dd>
            </div>
          ))}
          <div className="flex items-baseline justify-between gap-4 border-t-2 border-synthetic-yellow bg-surface-2 px-4 py-3 text-sm">
            <dt className="font-semibold text-ink">Committed in your leaf</dt>
            <dd data-testid="leaf-total">
              <Money value={proof.hoja.saldo} className="font-semibold text-ink" />
            </dd>
          </div>
        </dl>
        {lines.length === 0 ? (
          <p className="text-xs text-muted">This snapshot predates the breakdown: your leaf commits to the total.</p>
        ) : null}
        {changed ? (
          <p className="flex items-start gap-1.5 text-xs text-muted" data-testid="balance-changed">
            <Clock className="mt-0.5 size-3.5 shrink-0" aria-hidden />
            <span>
              Your cash has changed since this snapshot (now <Money value={proof.saldo_actual} />
              ). The next snapshot includes it.
            </span>
          </p>
        ) : null}
      </section>

      <section aria-labelledby="positions-title" className="space-y-2" data-testid="included-positions">
        <h4 id="positions-title" className="text-[11px] font-semibold tracking-[0.2em] text-ink-2 uppercase">
          Included balances · asset positions
        </h4>
        {rows.length === 0 ? (
          <p className="text-xs text-muted">No asset positions: nothing else in this snapshot is yours.</p>
        ) : (
          // Rows that stack on a phone and line up as columns from sm up.
          <ul className="divide-y divide-border border border-border">
            <li
              aria-hidden
              className="hidden grid-cols-[minmax(0,1fr)_6rem_6rem_7.5rem] gap-3 bg-surface-2 px-3 py-2 text-[10px] font-semibold tracking-[0.14em] text-muted uppercase sm:grid"
            >
              <span>Asset</span>
              <span className="text-right">In snapshot</span>
              <span className="text-right">Now</span>
              <span>Status</span>
            </li>
            {rows.map((row) => {
              const checked = audit?.verdict.positions.find((p) => p.ticker === row.ticker);
              return (
                <li
                  key={row.ticker}
                  className="grid grid-cols-[minmax(0,1fr)_auto] gap-x-3 gap-y-1 px-3 py-2.5 text-sm sm:grid-cols-[minmax(0,1fr)_6rem_6rem_7.5rem] sm:items-center"
                  data-testid="position-row"
                  data-ticker={row.ticker}
                  data-status={row.status}
                >
                  <span className="min-w-0">
                    <span className="block font-mono font-semibold text-ink">{row.ticker}</span>
                    {row.name ? <span className="block truncate text-xs text-muted">{row.name}</span> : null}
                  </span>
                  <span className="text-right text-xs sm:order-last sm:text-left">
                    <PositionStatus status={row.status} checked={checked?.ok ?? null} />
                  </span>
                  <span className="font-mono text-xs text-ink-2 tabular-nums sm:text-right sm:text-sm">
                    <span className="font-sans text-muted sm:sr-only">In snapshot </span>
                    {row.atSnapshot === null ? "—" : formatUnits(row.atSnapshot)}
                  </span>
                  <span className="text-right font-mono text-xs text-ink-2 tabular-nums sm:text-sm">
                    <span className="font-sans text-muted sm:sr-only">Now </span>
                    {row.now === null ? "—" : formatUnits(row.now)}
                  </span>
                </li>
              );
            })}
          </ul>
        )}
        {rows.length > 0 ? (
          <p className="text-xs leading-relaxed text-muted">
            Each position is a leaf in its asset&apos;s own tree, under the same account. A change since the snapshot is
            included in the next one.
          </p>
        ) : null}
      </section>
    </div>
  );
}

function PositionStatus({ status, checked }: { status: "included" | "changed" | "after"; checked: boolean | null }) {
  if (status === "after") {
    return <span className="text-muted">Next snapshot</span>;
  }
  if (checked === true) {
    return (
      <span className="inline-flex items-center gap-1 font-semibold text-synthetic-yellow">
        <CheckCircle2 className="size-3.5" aria-hidden /> Verified
      </span>
    );
  }
  if (checked === false) {
    return (
      <span className="inline-flex items-center gap-1 font-semibold text-danger">
        <XCircle className="size-3.5" aria-hidden /> Failed
      </span>
    );
  }
  return <span className="text-ink-2">{status === "changed" ? "Included · changed since" : "Included"}</span>;
}

function Outcome({ audit, server }: { audit: AuditOutcome; server: ResultadoVerificacionPrueba | null }) {
  const { verdict, proof } = audit;
  const ok = verdict.ok;
  const failedPositions = verdict.positions.filter((p) => !p.ok);
  const count = proof.posiciones?.length ?? 0;
  return (
    <div className="space-y-3" aria-live="polite">
      <div
        className={cx(
          "flex gap-3 border-2 p-4",
          ok ? "border-synthetic-yellow bg-synthetic-yellow/[0.06]" : "border-danger bg-danger-soft",
        )}
        data-testid="proof-verdict"
        data-status={ok ? "verified" : "failed"}
      >
        {ok ? (
          <CheckCircle2 className="size-6 shrink-0 text-synthetic-yellow" aria-hidden />
        ) : (
          <XCircle className="size-6 shrink-0 text-danger" aria-hidden />
        )}
        <div className="min-w-0 space-y-1 text-sm">
          <p className={cx("font-semibold", ok ? "text-synthetic-yellow" : "text-danger")}>
            {ok ? "Verified: you are included in the published reserve" : "Verification failed"}
          </p>
          {ok ? (
            <p className="text-ink-2">
              Your cash of <Money value={proof.hoja.saldo} />
              {count > 0
                ? ` and your ${count === 1 ? "position" : `${count} positions`} hash up to the roots`
                : " hashes up to the root"}{" "}
              the bank published, identical to its public copy, and all balances together equal the cash held in
              custody.
            </p>
          ) : (
            <>
              <ul className="list-disc space-y-0.5 pl-4 text-ink-2" data-testid="proof-issues">
                {verdict.issues.map((issue) => (
                  <li key={issue}>{ISSUE_MESSAGES[issue]}</li>
                ))}
                {failedPositions.flatMap((p) =>
                  p.issues.map((issue) => (
                    <li key={`${p.ticker}-${issue}`}>
                      {p.ticker}: {ISSUE_MESSAGES[issue]}
                    </li>
                  )),
                )}
              </ul>
              <p className="text-ink-2">Download the proof and contact the bank.</p>
            </>
          )}
        </div>
      </div>
      <ul className="flex flex-wrap gap-2 text-[11px]" aria-label="How the check ran">
        <Chip>Hashed in your browser · {audit.engine === "webcrypto" ? "WebCrypto SHA-256" : "JS SHA-256"}</Chip>
        <Chip tone={audit.publicCopy === "matches" ? "ok" : "bad"}>
          Public snapshot copy:{" "}
          {audit.publicCopy === "matches" ? "matches" : audit.publicCopy === "differs" ? "differs" : "unavailable"}
        </Chip>
        {server ? (
          <Chip tone={server.valida ? "ok" : "bad"} icon={<Server className="size-3" aria-hidden />}>
            Server check: {server.valida ? "agrees" : "disagrees"}
          </Chip>
        ) : null}
      </ul>
    </div>
  );
}

function Chip({
  children,
  tone = "neutral",
  icon,
}: {
  children: React.ReactNode;
  tone?: "neutral" | "ok" | "bad";
  icon?: React.ReactNode;
}) {
  return (
    <li
      className={cx(
        "inline-flex items-center gap-1.5 border px-2 py-1 font-medium",
        tone === "ok"
          ? "border-synthetic-yellow/60 text-synthetic-yellow"
          : tone === "bad"
            ? "border-danger text-danger"
            : "border-border-strong text-ink-2",
      )}
    >
      {icon}
      {children}
    </li>
  );
}

function DownloadProof({ proof }: { proof: PruebaReserva }) {
  const download = () => {
    const blob = new Blob([JSON.stringify(proof, null, 2)], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `proof-of-reserve-${proof.corte.id}.json`;
    a.click();
    URL.revokeObjectURL(url);
  };
  return (
    <button
      type="button"
      onClick={download}
      className="inline-flex h-9 shrink-0 items-center gap-1.5 border border-border-strong px-3 text-[11px] font-semibold tracking-[0.14em] text-ink-2 uppercase hover:border-synthetic-yellow hover:text-synthetic-yellow focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
      aria-label="Download your proof as JSON"
    >
      <Download className="size-3.5" aria-hidden />
      JSON
    </button>
  );
}
