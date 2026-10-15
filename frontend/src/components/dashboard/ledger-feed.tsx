"use client";

import { Fragment, useState } from "react";
import {
  ArrowDownLeft,
  ArrowLeftRight,
  ArrowUpRight,
  ChevronRight,
  Link2,
  Lock,
  ScrollText,
  ShieldAlert,
  ShieldCheck,
  Unlock,
  type LucideIcon,
} from "lucide-react";

import { HashText, Money, formatDateTime } from "@/components/ui/data";
import { Badge, Button, Card, CardHeader, Skeleton, cx, type Tone } from "@/components/ui/primitives";
import type { TipoTxIfpe, TransaccionIfpe } from "@/lib/api/types";

type Direction = "in" | "out" | "internal";

interface Kind {
  label: string;
  icon: LucideIcon;
  tone: Tone;
  direction: Direction;
}

function describe(tx: TransaccionIfpe, accountId: string | null): Kind {
  const map: Record<TipoTxIfpe, Kind> = {
    DEPOSITO: { label: "Deposit", icon: ArrowDownLeft, tone: "verified", direction: "in" },
    RETIRO: { label: "Withdrawal", icon: ArrowUpRight, tone: "neutral", direction: "out" },
    TRANSFERENCIA:
      tx.cuenta_origen_id === accountId
        ? { label: "Transfer out", icon: ArrowLeftRight, tone: "neutral", direction: "out" }
        : { label: "Transfer in", icon: ArrowLeftRight, tone: "verified", direction: "in" },
    TRASPASO_A_SOFOM: { label: "To fixed term", icon: Lock, tone: "accent", direction: "out" },
    RETORNO_DE_SOFOM: { label: "Contract matured", icon: Unlock, tone: "verified", direction: "in" },
    RETENCION: { label: "Compliance hold", icon: ShieldAlert, tone: "warning", direction: "internal" },
    LIBERACION_RETENCION: { label: "Hold released", icon: ShieldCheck, tone: "info", direction: "internal" },
  };
  return map[tx.tipo];
}

/** Your entries in the bank's append-only, SHA-256-chained IFPE ledger. */
export function LedgerFeed({
  movements,
  accountId,
  loading,
  hasMore,
  loadingMore,
  onLoadMore,
}: {
  movements: TransaccionIfpe[];
  accountId: string | null;
  loading: boolean;
  hasMore: boolean;
  loadingMore: boolean;
  onLoadMore: () => void;
}) {
  const [expanded, setExpanded] = useState<string | null>(null);

  return (
    <Card aria-labelledby="ledger-title">
      <CardHeader
        icon={<ScrollText className="size-4.5" aria-hidden />}
        eyebrow="Immutable ledger"
        title={<span id="ledger-title">Transaction history</span>}
        action={
          <Badge tone="neutral" icon={<Link2 className="size-3" aria-hidden />}>
            SHA-256 chained · append-only
          </Badge>
        }
      />
      {loading && movements.length === 0 ? (
        <div className="space-y-2 p-5">
          {Array.from({ length: 4 }, (_, i) => (
            <Skeleton key={i} className="h-8 w-full" />
          ))}
        </div>
      ) : movements.length === 0 ? (
        <p className="p-8 text-center text-sm text-muted">No movements yet. Fund your vault to get started.</p>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full min-w-[760px] border-collapse text-left text-[13px]">
            <thead>
              <tr className="border-b border-border bg-surface-2/60 text-[11px] tracking-wide text-muted uppercase">
                <th scope="col" className="w-8 py-2 pl-5" aria-label="Details" />
                <th scope="col" className="py-2 pr-3 font-medium">Seq</th>
                <th scope="col" className="py-2 pr-3 font-medium">Time</th>
                <th scope="col" className="py-2 pr-3 font-medium">Type</th>
                <th scope="col" className="py-2 pr-3 font-medium">Description</th>
                <th scope="col" className="py-2 pr-3 text-right font-medium">Amount</th>
                <th scope="col" className="py-2 pr-5 font-medium">tx_hash</th>
              </tr>
            </thead>
            <tbody>
              {movements.map((tx) => {
                const kind = describe(tx, accountId);
                const Icon = kind.icon;
                const open = expanded === tx.id;
                return (
                  <Fragment key={tx.id}>
                    <tr
                      className={cx(
                        "cursor-pointer border-b border-border/70 transition hover:bg-surface-2/60",
                        open && "bg-surface-2/60",
                      )}
                      onClick={() => setExpanded(open ? null : tx.id)}
                    >
                      <td className="py-2 pl-5">
                        <button
                          type="button"
                          className="rounded p-0.5 text-muted hover:text-ink focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none"
                          aria-expanded={open}
                          aria-label={`${open ? "Hide" : "Show"} details for movement ${tx.secuencia}`}
                          onClick={(e) => {
                            e.stopPropagation();
                            setExpanded(open ? null : tx.id);
                          }}
                        >
                          <ChevronRight className={cx("size-3.5 transition", open && "rotate-90")} aria-hidden />
                        </button>
                      </td>
                      <td className="py-2 pr-3 font-mono text-xs text-muted">#{tx.secuencia}</td>
                      <td className="py-2 pr-3 whitespace-nowrap text-ink-2">{formatDateTime(tx.creado_en)}</td>
                      <td className="py-2 pr-3">
                        <Badge tone={kind.tone} icon={<Icon className="size-3" aria-hidden />}>
                          {kind.label}
                        </Badge>
                      </td>
                      <td className="max-w-[260px] truncate py-2 pr-3 text-ink-2" title={tx.concepto ?? undefined}>
                        {tx.concepto ?? <span className="text-muted">—</span>}
                      </td>
                      <td
                        className={cx(
                          "py-2 pr-3 text-right font-semibold whitespace-nowrap",
                          kind.direction === "in" && "text-verified",
                          kind.direction === "internal" && "text-muted",
                        )}
                      >
                        <Money
                          value={tx.monto}
                          sign={kind.direction === "in" ? "+" : kind.direction === "out" ? "-" : undefined}
                        />
                      </td>
                      <td className="py-2 pr-5" onClick={(e) => e.stopPropagation()}>
                        <HashText value={tx.tx_hash} head={10} tail={6} label="transaction hash" />
                      </td>
                    </tr>
                    {open ? (
                      <tr className="border-b border-border bg-surface-2/40">
                        <td />
                        <td colSpan={6} className="py-3 pr-5">
                          <MovementDetails tx={tx} />
                        </td>
                      </tr>
                    ) : null}
                  </Fragment>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
      {hasMore ? (
        <div className="border-t border-border p-3 text-center">
          <Button variant="ghost" size="sm" onClick={onLoadMore} loading={loadingMore}>
            Load older movements
          </Button>
        </div>
      ) : null}
    </Card>
  );
}

function MovementDetails({ tx }: { tx: TransaccionIfpe }) {
  const rows: [string, React.ReactNode][] = [
    ["Movement ID", <span key="id" className="font-mono">{tx.id}</span>],
    ["tx_hash", <HashText key="h" value={tx.tx_hash} head={32} tail={32} label="transaction hash" />],
    ["Previous hash (chain link)", <HashText key="p" value={tx.hash_anterior} head={32} tail={32} label="previous hash" />],
  ];
  if (tx.referencia_externa) rows.push(["External reference", <span key="r" className="font-mono">{tx.referencia_externa}</span>]);
  if (tx.ref_movimiento_espejo)
    rows.push(["Mirror entry (SOFOM ledger)", <span key="m" className="font-mono">{tx.ref_movimiento_espejo}</span>]);
  if (tx.reserva_id) rows.push(["Custody account moved", <span key="c" className="font-mono">{tx.reserva_id}</span>]);
  return (
    <dl className="grid gap-x-6 gap-y-1.5 text-xs sm:grid-cols-[max-content_1fr]">
      {rows.map(([label, value]) => (
        <Fragment key={label}>
          <dt className="text-muted">{label}</dt>
          <dd className="min-w-0 break-all text-ink-2">{value}</dd>
        </Fragment>
      ))}
      <dt className="text-muted">Integrity</dt>
      <dd className="text-ink-2">
        tx_hash = SHA-256 of this entry&apos;s canonical fields plus the previous hash. The database computes it,
        the backend recomputes it before commit, and edits or deletions are rejected.
      </dd>
    </dl>
  );
}
