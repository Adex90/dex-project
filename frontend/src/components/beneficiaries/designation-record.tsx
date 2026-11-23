import { AlertTriangle, History, ShieldCheck } from "lucide-react";

import { truncateHash } from "@/components/ui/data";
import { cx } from "@/components/ui/primitives";
import { RELATIONSHIP_LABELS, centsToPercent, percentToCents } from "@/lib/beneficiaries";
import { documentTime } from "@/lib/documents";
import type { BeneficiariesView } from "@/types/beneficiaries";

/*
 * What the server holds: the designation on record and every earlier
 * version. Versions are immutable and hash-chained: each one's hash covers
 * its beneficiaries and the previous version's hash, and the server
 * recomputes the whole chain on every read (`chainVerified`).
 */

export function DesignationRecord({ view }: { view: BeneficiariesView }) {
  const current = view.current;
  return (
    <div className="space-y-6">
      <section className="border border-border bg-surface" data-testid="designation-record">
        <header className="flex items-center justify-between gap-3 border-b border-border px-4 py-3">
          <h3 className="text-sm font-semibold text-ink">On record</h3>
          {current ? (
            <span className="font-mono text-xs text-muted" data-testid="designation-version">
              Version {current.version}
            </span>
          ) : null}
        </header>
        <div className="space-y-4 px-4 py-4">
          {!current || current.beneficiaries.length === 0 ? (
            <p className="text-sm text-muted">
              {current
                ? `No beneficiaries on record since version ${current.version}.`
                : "No beneficiaries on record yet."}
            </p>
          ) : (
            <ul className="divide-y divide-border" data-testid="record-beneficiaries">
              {current.beneficiaries.map((b) => (
                <li key={b.position} className="flex items-baseline justify-between gap-3 py-2">
                  <span className="min-w-0">
                    <span className="block truncate text-sm font-medium text-ink">{b.fullName}</span>
                    <span className="block text-xs text-muted">
                      {RELATIONSHIP_LABELS[b.relationship]}
                      {b.curp ? ` · ${b.curp}` : b.birthDate ? ` · born ${b.birthDate}` : ""}
                    </span>
                  </span>
                  <span className="shrink-0 text-sm font-semibold text-synthetic-yellow tabular-nums">
                    {centsToPercent(percentToCents(b.percent) ?? 0)}%
                  </span>
                </li>
              ))}
            </ul>
          )}
          {current ? (
            <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 border-t border-border pt-3 text-xs">
              <dt className="text-muted">Saved</dt>
              <dd className="text-ink-2">{documentTime(current.savedAt)}</dd>
              {current.device ? (
                <>
                  <dt className="text-muted">From</dt>
                  <dd className="truncate text-ink-2">{current.device}</dd>
                </>
              ) : null}
              <dt className="text-muted">Hash</dt>
              <dd className="truncate font-mono text-ink-2" title={current.hash}>
                {truncateHash(current.hash, 12, 8)}
              </dd>
            </dl>
          ) : null}
          <ChainSeal verified={view.chainVerified} versions={view.version} />
        </div>
      </section>

      <section className="border border-border bg-surface" data-testid="designation-history">
        <header className="flex items-center gap-2 border-b border-border px-4 py-3">
          <History className="size-4 text-muted" aria-hidden />
          <h3 className="text-sm font-semibold text-ink">Version history</h3>
        </header>
        {view.history.length === 0 ? (
          <p className="px-4 py-4 text-sm text-muted">Every save will appear here, signed and chained.</p>
        ) : (
          <ol className="divide-y divide-border">
            {view.history.map((v) => (
              <li key={v.version} className="space-y-0.5 px-4 py-3" data-testid="history-row">
                <p className="flex items-baseline justify-between gap-3 text-sm">
                  <span className="font-semibold text-ink">Version {v.version}</span>
                  <span className="text-xs text-muted">
                    {v.count === 0 ? "No beneficiaries" : `${v.count} beneficiar${v.count === 1 ? "y" : "ies"}`}
                  </span>
                </p>
                <p className="text-xs text-ink-2">{documentTime(v.savedAt)}</p>
                <p className="truncate font-mono text-[11px] text-muted" title={v.hash}>
                  {truncateHash(v.hash, 10, 6)}
                  {v.device ? ` · ${v.device}` : ""}
                </p>
              </li>
            ))}
          </ol>
        )}
      </section>
    </div>
  );
}

function ChainSeal({ verified, versions }: { verified: boolean; versions: number }) {
  if (versions === 0) return null;
  return (
    <p
      className={cx(
        "flex items-start gap-2 border px-3 py-2 text-xs",
        verified ? "border-synthetic-yellow text-ink-2" : "border-warning text-warning",
      )}
      data-testid="chain-seal"
      data-verified={verified}
    >
      {verified ? (
        <ShieldCheck className="mt-px size-4 shrink-0 text-synthetic-yellow" aria-hidden />
      ) : (
        <AlertTriangle className="mt-px size-4 shrink-0" aria-hidden />
      )}
      <span>
        {verified ? (
          <>
            <span className="font-semibold text-synthetic-yellow">Chain verified.</span> All {versions} version
            {versions === 1 ? "" : "s"} recomputed and linked, hash by hash.
          </>
        ) : (
          "The version history could not be confirmed. Please contact support before making changes."
        )}
      </span>
    </p>
  );
}
