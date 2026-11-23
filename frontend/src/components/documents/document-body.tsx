import type { ReactNode } from "react";
import Link from "next/link";
import { Clock, ShieldCheck } from "lucide-react";

import { HashText, Money, truncateHash } from "@/components/ui/data";
import { cx } from "@/components/ui/primitives";
import { compareAmounts, formatMoney, isZero } from "@/lib/decimal";
import {
  ENTRY_LABELS,
  ORDER_TYPE_LABELS,
  PRICE_SOURCE_LABELS,
  SIDE_LABELS,
  STATEMENT_HOLD_LABEL,
  documentDate,
  documentTime,
  periodLabel,
} from "@/lib/documents";
import { formatPrice } from "@/lib/market";
import { formatUnits } from "@/lib/solvency";
import type { AccountStatement, FinancialDocument, ReserveSeal, TradeConfirmation } from "@/types/documents";

/*
 * A financial document as the client reads it, on screen and on paper.
 *
 * - "screen": inside the viewer, hashes truncated with a copy button.
 * - "print": the copy the browser prints (see `DocumentViewer`), every hash
 *   in full so the paper can be checked against the ledgers by hand.
 *
 * Every figure is the server's, formatted exactly (BigInt decimals): nothing
 * here computes money. The words are a bank's paperwork only: funds on hold
 * appear in the statement's closing breakdown with the neutral copy.
 */

type Variant = "screen" | "print";

export function DocumentBody({ doc, variant = "screen" }: { doc: FinancialDocument; variant?: Variant }) {
  return (
    <article className="print-document space-y-6 bg-surface text-ink" data-testid={`document-${variant}`}>
      <Letterhead doc={doc} />
      {doc.kind === "trade_confirmation" ? (
        <ConfirmationBody doc={doc} variant={variant} />
      ) : (
        <StatementBody doc={doc} variant={variant} />
      )}
      <SealPanel seal={doc.reserve_seal} variant={variant} />
      <p className="border-t border-border pt-4 text-[11px] leading-relaxed text-muted">
        Derived from Synthetic Capital&apos;s append-only, hash-chained ledgers. The reference hash identifies this
        exact document: if any figure changed, the hash would no longer match the ledger. Times are Mexico City time
        (UTC−6).
      </p>
    </article>
  );
}

function Letterhead({ doc }: { doc: FinancialDocument }) {
  const confirmation = doc.kind === "trade_confirmation";
  return (
    <header className="flex flex-wrap items-start justify-between gap-4 border-b-2 border-synthetic-yellow pb-4">
      <div className="min-w-0 space-y-1">
        <p className="text-[11px] font-bold tracking-[0.3em] text-synthetic-yellow uppercase">Synthetic Capital</p>
        <h3 className="text-xl font-semibold tracking-tight text-ink">
          {confirmation ? "Trade confirmation" : "Account statement"}
        </h3>
        <p className="text-xs text-muted">{confirmation ? "Comprobante de Operación" : "Estado de Cuenta"}</p>
      </div>
      <dl className="grid shrink-0 grid-cols-[auto_auto] gap-x-4 gap-y-1 text-xs">
        <dt className="text-muted">Folio</dt>
        <dd className="text-right font-mono font-semibold text-ink" data-testid="document-folio">
          {doc.folio}
        </dd>
        <dt className="text-muted">Holder</dt>
        <dd className="max-w-56 truncate text-right font-medium text-ink">{doc.holder.name}</dd>
        {doc.holder.rfc ? (
          <>
            <dt className="text-muted">RFC</dt>
            <dd className="text-right font-mono text-ink">{doc.holder.rfc}</dd>
          </>
        ) : null}
        <dt className="text-muted">Account</dt>
        <dd className="text-right font-mono text-ink">
          {doc.kind === "account_statement"
            ? doc.account_clabe
            : doc.account_clabe_last4
              ? `CLABE ···${doc.account_clabe_last4}`
              : "—"}
        </dd>
      </dl>
    </header>
  );
}

/* ------------------------------------------------------------------------ */
/* Trade confirmation                                                        */
/* ------------------------------------------------------------------------ */

function ConfirmationBody({ doc, variant }: { doc: TradeConfirmation; variant: Variant }) {
  const sell = doc.side === "sell";
  return (
    <>
      <section className="space-y-1">
        <p className="text-[11px] font-semibold tracking-[0.18em] text-muted uppercase">Executed</p>
        <p className="text-lg font-semibold text-ink" data-testid="confirmation-headline">
          {sell ? "Sold" : "Bought"} {formatUnits(doc.quantity)} {doc.ticker}{" "}
          <span className="font-normal text-muted">at</span> {formatPrice(doc.price)}{" "}
          <span className="text-xs font-medium text-muted">MXN</span>
        </p>
        <p className="text-sm text-ink-2">
          {doc.asset_name} · {documentTime(doc.executed_at)}
        </p>
      </section>

      <Section title="Execution">
        <Rows>
          <Row label="Side">{SIDE_LABELS[doc.side]}</Row>
          <Row label="Order type">
            {ORDER_TYPE_LABELS[doc.order_type]}
            {doc.limit_price ? ` · limit ${formatPrice(doc.limit_price)}` : ""}
            {doc.stop_price ? ` · stop ${formatPrice(doc.stop_price)}` : ""}
          </Row>
          <Row label="Amount requested">
            <Money value={doc.requested_amount} />
          </Row>
          <Row label="Quantity">
            <span className="tabular-nums" data-testid="confirmation-quantity">
              {formatUnits(doc.quantity)} {doc.ticker}
            </span>
          </Row>
          <Row label="Execution price">
            <span className="tabular-nums">{formatPrice(doc.price)}</span>{" "}
            <span className="text-[0.7em] font-medium text-muted">MXN</span>
          </Row>
          <Row label="Price source">{PRICE_SOURCE_LABELS[doc.price_source]}</Row>
        </Rows>
      </Section>

      <Section title="Settlement" testId="commission-breakdown">
        <Rows>
          <Row label="Gross amount">
            <Money value={doc.gross_amount} />
          </Row>
          <Row
            label="Commission"
            badge={
              isZero(doc.commission) ? (
                <span className="border border-synthetic-yellow px-1.5 py-px text-[10px] font-bold tracking-[0.14em] text-synthetic-yellow uppercase">
                  Zero commission
                </span>
              ) : null
            }
          >
            <span data-testid="confirmation-commission">
              <Money value={doc.commission} />
            </span>
          </Row>
          <Row label={sell ? "Net credited to your account" : "Net debited from your account"} strong>
            <span data-testid="confirmation-net">
              <Money value={doc.net_amount} />
            </span>
          </Row>
          {doc.cost_basis ? (
            <Row label="Cost basis of the units sold">
              <Money value={doc.cost_basis} />
            </Row>
          ) : null}
          {doc.realized_pnl ? (
            <Row label="Realized gain or loss">
              <PnL value={doc.realized_pnl} />
            </Row>
          ) : null}
        </Rows>
      </Section>

      <Section title="Ledger references">
        <Rows>
          <Row label="Market ledger entry">#{doc.market_sequence}</Row>
          <Row label="Reference hash">
            <Hash value={doc.reference} variant={variant} label="reference hash" testId="document-reference" />
          </Row>
          <Row label="Previous hash">
            <Hash value={doc.previous_hash} variant={variant} label="previous hash" />
          </Row>
          {doc.settlement_sequence !== null ? <Row label="Cash ledger entry">#{doc.settlement_sequence}</Row> : null}
          {doc.settlement_hash ? (
            <Row label="Cash ledger hash">
              <Hash value={doc.settlement_hash} variant={variant} label="cash ledger hash" />
            </Row>
          ) : null}
        </Rows>
      </Section>
    </>
  );
}

/* ------------------------------------------------------------------------ */
/* Account statement                                                         */
/* ------------------------------------------------------------------------ */

function StatementBody({ doc, variant }: { doc: AccountStatement; variant: Variant }) {
  const { available, on_hold: onHold, held_for_orders: heldForOrders } = doc.closing_breakdown;
  return (
    <>
      <section className="space-y-1">
        <p className="text-[11px] font-semibold tracking-[0.18em] text-muted uppercase">Period</p>
        <p className="text-lg font-semibold text-ink">
          {periodLabel(doc.period)}
          {doc.provisional ? (
            <span className="ml-2 border border-border-strong px-1.5 py-px align-middle text-[10px] font-bold tracking-[0.14em] text-ink-2 uppercase">
              In progress
            </span>
          ) : null}
        </p>
        <p className="text-sm text-ink-2">
          {documentDate(doc.period_start)} – {documentDate(doc.period_end)}
          {doc.provisional ? " · figures update until the month closes" : ""}
        </p>
      </section>

      <Section title="Summary">
        <Rows>
          <Row label="Opening balance">
            <Money value={doc.opening_balance} />
          </Row>
          <Row label="Credits">
            <Money value={doc.credits} sign="+" />
          </Row>
          <Row label="Debits">
            <Money value={doc.debits} sign="-" />
          </Row>
          <Row label="Closing balance" strong>
            <span data-testid="statement-closing">
              <Money value={doc.closing_balance} />
            </span>
          </Row>
          <Row label="Available" sub>
            <Money value={available} />
          </Row>
          {isZero(onHold) ? null : (
            <Row label={STATEMENT_HOLD_LABEL} sub>
              <Money value={onHold} />
            </Row>
          )}
          {isZero(heldForOrders) ? null : (
            <Row label="Held for open orders" sub>
              <Money value={heldForOrders} />
            </Row>
          )}
        </Rows>
      </Section>

      <Section title="Trading" testId="commission-breakdown">
        <Rows>
          <Row label={`Buys (${doc.trades.buys})`}>
            <Money value={doc.trades.bought} />
          </Row>
          <Row label={`Sells (${doc.trades.sells})`}>
            <Money value={doc.trades.sold} />
          </Row>
          <Row
            label="Commissions"
            badge={
              isZero(doc.trades.commission) ? (
                <span className="border border-synthetic-yellow px-1.5 py-px text-[10px] font-bold tracking-[0.14em] text-synthetic-yellow uppercase">
                  Zero commission
                </span>
              ) : null
            }
          >
            <span data-testid="statement-commission">
              <Money value={doc.trades.commission} />
            </span>
          </Row>
        </Rows>
      </Section>

      <Section title={`Movements (${doc.entries.length})`}>
        {doc.entries.length === 0 ? (
          <p className="text-sm text-muted">No movements this month.</p>
        ) : (
          <div className="-mx-1 overflow-x-auto">
            <table className="w-full min-w-[520px] text-left text-xs" data-testid="statement-entries">
              <thead>
                <tr className="border-b border-border text-[10px] tracking-[0.12em] text-muted uppercase">
                  <th scope="col" className="px-1 py-2 font-semibold">
                    Date
                  </th>
                  <th scope="col" className="px-1 py-2 font-semibold">
                    Description
                  </th>
                  <th scope="col" className="px-1 py-2 text-right font-semibold">
                    Amount
                  </th>
                  <th scope="col" className="px-1 py-2 text-right font-semibold">
                    Balance
                  </th>
                </tr>
              </thead>
              <tbody>
                {doc.entries.map((entry) => {
                  const negative = entry.amount.startsWith("-");
                  return (
                    <tr key={entry.sequence} className="border-b border-border/60 align-top">
                      <td className="px-1 py-2 whitespace-nowrap text-ink-2">{documentDate(entry.date)}</td>
                      <td className="px-1 py-2 text-ink">
                        {ENTRY_LABELS[entry.kind]}
                        {entry.ticker ? ` · ${entry.ticker}` : ""}
                        <span className="block font-mono text-[10px] text-muted">
                          #{entry.sequence} · {variant === "print" ? entry.hash : truncateHash(entry.hash, 8, 4)}
                          {entry.tracking_key ? ` · ${entry.tracking_key}` : ""}
                        </span>
                      </td>
                      <td
                        className={cx(
                          "px-1 py-2 text-right whitespace-nowrap tabular-nums",
                          negative ? "text-ink" : "text-gain",
                        )}
                      >
                        {negative ? "−" : "+"}
                        {formatMoney(entry.amount.replace(/^-/, ""))}
                      </td>
                      <td className="px-1 py-2 text-right whitespace-nowrap text-ink-2 tabular-nums">
                        {formatMoney(entry.balance)}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </Section>

      <Section title="Positions at the end of the period">
        {doc.positions.length === 0 ? (
          <p className="text-sm text-muted">No positions.</p>
        ) : (
          <Rows>
            {doc.positions.map((p) => (
              <Row key={p.ticker} label={`${p.ticker} · ${p.asset_name}`}>
                <span className="tabular-nums">{formatUnits(p.quantity)} units</span>
              </Row>
            ))}
          </Rows>
        )}
      </Section>

      <Section title="Reference">
        <Rows>
          <Row label="Statement hash">
            <Hash value={doc.reference} variant={variant} label="statement hash" testId="document-reference" />
          </Row>
        </Rows>
      </Section>
    </>
  );
}

/* ------------------------------------------------------------------------ */
/* Reserve verification seal                                                 */
/* ------------------------------------------------------------------------ */

function SealPanel({ seal, variant }: { seal: ReserveSeal; variant: Variant }) {
  const sealed = seal.status === "sealed";
  const covers = `cash ledger entry #${seal.covers_ledger_sequence}${
    seal.covers_market_sequence > 0 ? ` and market ledger entry #${seal.covers_market_sequence}` : ""
  }`;
  return (
    <section
      className={cx("print-seal border-2 p-4", sealed ? "border-synthetic-yellow" : "border-border-strong")}
      data-testid="reserve-seal"
      data-status={seal.status}
    >
      <div className="flex items-start gap-3">
        <span
          className={cx(
            "grid size-10 shrink-0 place-items-center",
            sealed ? "bg-synthetic-yellow text-black" : "bg-surface-2 text-muted",
          )}
          aria-hidden
        >
          {sealed ? <ShieldCheck className="size-5" strokeWidth={2.25} /> : <Clock className="size-5" />}
        </span>
        <div className="min-w-0 flex-1 space-y-2">
          <p
            className={cx(
              "text-[11px] font-bold tracking-[0.2em] uppercase",
              sealed ? "text-synthetic-yellow" : "text-ink-2",
            )}
          >
            {sealed ? `Reserve verified · ${seal.reserve_ratio}% backed` : "Reserve seal pending"}
          </p>
          {sealed && seal.root && seal.taken_at && seal.snapshot_id ? (
            <>
              <p className="text-sm text-ink-2">
                Covered by the Proof of Reserve snapshot taken {documentTime(seal.taken_at)}, which includes {covers}.
              </p>
              <dl className="grid gap-x-4 gap-y-1 text-xs sm:grid-cols-[auto_1fr]">
                <dt className="text-muted">Snapshot</dt>
                <dd className="min-w-0 font-mono text-ink break-all">{seal.snapshot_id}</dd>
                <dt className="text-muted">Merkle root</dt>
                <dd className="min-w-0 text-ink">
                  <Hash value={seal.root} variant={variant} label="Merkle root" testId="seal-root" />
                </dd>
                <dt className="text-muted">Anchors</dt>
                <dd className="text-ink">
                  Cash ledger #{seal.ledger_anchor}
                  {seal.market_anchor !== null ? ` · market ledger #${seal.market_anchor}` : ""}
                </dd>
              </dl>
              {variant === "screen" ? (
                <Link
                  href="/dashboard/reserve"
                  className="inline-flex text-xs font-semibold text-synthetic-yellow underline-offset-4 hover:underline focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
                >
                  Verify it on the Reserve tab →
                </Link>
              ) : null}
            </>
          ) : (
            <p className="text-sm text-ink-2">
              The next Proof of Reserve snapshot that includes {covers} seals this document. Its figures are final
              already; the seal adds the snapshot&apos;s Merkle root.
            </p>
          )}
        </div>
      </div>
    </section>
  );
}

/* ------------------------------------------------------------------------ */

/** A gain in green with "+", a loss in red with "−", nothing in plain ink. */
function PnL({ value }: { value: string }) {
  const sign = compareAmounts(value, "0");
  return (
    <span className={sign < 0 ? "text-loss" : sign > 0 ? "text-gain" : "text-ink"}>
      <Money value={value.replace(/^-/, "")} sign={sign < 0 ? "-" : sign > 0 ? "+" : undefined} />
    </span>
  );
}

function Section({ title, children, testId }: { title: string; children: ReactNode; testId?: string }) {
  return (
    <section className="space-y-2" data-testid={testId}>
      <h4 className="border-b border-border pb-1.5 text-[11px] font-semibold tracking-[0.18em] text-muted uppercase">
        {title}
      </h4>
      {children}
    </section>
  );
}

function Rows({ children }: { children: ReactNode }) {
  return <dl className="divide-y divide-border/60">{children}</dl>;
}

function Row({
  label,
  children,
  badge,
  strong = false,
  sub = false,
}: {
  label: string;
  children: ReactNode;
  badge?: ReactNode;
  strong?: boolean;
  sub?: boolean;
}) {
  return (
    <div className={cx("flex items-baseline justify-between gap-4 py-1.5 text-sm", sub && "pl-4 text-xs")}>
      <dt className={cx("flex min-w-0 items-center gap-2", strong ? "font-semibold text-ink" : "text-ink-2")}>
        <span className="min-w-0">{label}</span>
        {badge}
      </dt>
      <dd className={cx("min-w-0 text-right", strong ? "font-semibold text-ink" : "text-ink")}>{children}</dd>
    </div>
  );
}

function Hash({ value, variant, label, testId }: { value: string; variant: Variant; label: string; testId?: string }) {
  if (variant === "print") {
    return (
      <span className="font-mono text-[10px] break-all" data-testid={testId}>
        {value}
      </span>
    );
  }
  return (
    <span data-testid={testId} data-value={value} className="inline-flex max-w-full justify-end">
      <HashText value={value} label={label} />
    </span>
  );
}
