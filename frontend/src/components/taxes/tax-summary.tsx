import type { ReactNode } from "react";

import { PnL } from "@/components/documents/document-body";
import { Money } from "@/components/ui/data";
import { cx } from "@/components/ui/primitives";
import { formatMoney } from "@/lib/decimal";
import { documentDate } from "@/lib/documents";
import { ASSET_CLASS_LABELS, formatRate, treatmentLabel } from "@/lib/taxes";
import type { TaxYearSummary } from "@/types/taxes";

/*
 * Realized gains and withholding for one fiscal year (the Mexico City
 * calendar year; the current one to date). Every figure is the server's,
 * derived from the ledgers: gross sales and the average cost they released,
 * the net result, fixed-term interest in the year it was paid, what was
 * withheld, and an ISR preview from the year's published rates.
 */

export function TaxSummary({
  summary,
  years,
  onYear,
  loading,
}: {
  summary: TaxYearSummary;
  years: number[];
  onYear: (year: number) => void;
  loading: boolean;
}) {
  const { market, fixed_term: fixedTerm, isr } = summary;
  const rates = isr.rates;
  const options = years.includes(summary.year) ? years : [summary.year, ...years].sort((a, b) => b - a);
  return (
    <section className="border border-border bg-surface" aria-labelledby="tax-summary-title" data-testid="tax-summary">
      <header className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-4 py-3 sm:px-5">
        <div className="min-w-0">
          <h3 id="tax-summary-title" className="text-sm font-semibold text-ink">
            Realized gains &amp; withholding
          </h3>
          <p className="text-xs text-muted" data-testid="tax-period">
            {documentDate(summary.period_start)} – {documentDate(summary.period_end)}
            {summary.in_progress ? " · year in progress, figures to date" : " · year closed"}
          </p>
        </div>
        <label className="flex items-center gap-2 text-xs text-muted">
          Fiscal year
          <select
            value={summary.year}
            onChange={(e) => onYear(Number(e.target.value))}
            disabled={loading}
            className="h-10 border border-border-strong bg-bg px-3 text-sm font-semibold text-ink focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none"
            data-testid="tax-year"
          >
            {options.map((y) => (
              <option key={y} value={y}>
                {y}
              </option>
            ))}
          </select>
        </label>
      </header>

      <dl
        className={cx("grid grid-cols-2 gap-px bg-border lg:grid-cols-3", loading && "opacity-60")}
        aria-busy={loading}
      >
        <Tile label="Gross sales" hint={`${market.sells} sale${market.sells === 1 ? "" : "s"}`} testId="tax-proceeds">
          <Money value={market.proceeds} />
        </Tile>
        <Tile label="Cost basis" hint="Average cost of the units sold" testId="tax-cost-basis">
          <Money value={market.cost_basis} />
        </Tile>
        <Tile
          label="Net realized result"
          hint={`Commissions ${formatMoney(market.commissions)}`}
          testId="tax-net-result"
        >
          <PnL value={market.net_result} />
        </Tile>
        <Tile
          label="Fixed-term interest"
          hint={`${fixedTerm.contracts_paid} paid · ${fixedTerm.contracts_open} open`}
          testId="tax-interest"
        >
          <Money value={fixedTerm.interest_paid} />
        </Tile>
        <Tile label="ISR withheld" hint="Recorded in the ledgers" testId="tax-withheld">
          <Money value={isr.withheld} />
        </Tile>
        <Tile label="Estimated ISR" hint="Preview, not withheld" testId="tax-estimated" accent>
          {isr.estimated ? (
            <Money value={isr.estimated.total} />
          ) : (
            <span className="text-sm text-muted">No rates yet</span>
          )}
        </Tile>
      </dl>

      <div className="space-y-2 border-t border-border px-4 py-4 sm:px-5" data-testid="isr-preview">
        <p className="text-[11px] font-semibold tracking-[0.18em] text-muted uppercase">ISR preview</p>
        {isr.estimated && rates ? (
          <dl className="divide-y divide-border/60 text-sm">
            <PreviewRow
              label={`Withholding on fixed-term capital, ${formatRate(rates.interest_withholding_annual)} a year by days held`}
              basis={rates.interest_basis}
            >
              <Money value={isr.estimated.interest_withholding} />
            </PreviewRow>
            <PreviewRow
              label={`Listed shares, ${formatRate(rates.listed_shares)} of the year's net gain`}
              basis={rates.listed_shares_basis}
            >
              <Money value={isr.estimated.listed_shares_tax} />
            </PreviewRow>
          </dl>
        ) : (
          <p className="text-sm text-ink-2">
            The rates for {summary.year} are not published yet, so there is no estimate.
          </p>
        )}
        <p className="text-xs text-muted">
          Commodities and fixed income:{" "}
          <span className="font-semibold text-ink-2">
            <PnL value={isr.annual_return_result} />
          </span>{" "}
          net, added to your annual return. A net loss on listed shares pays nothing this year.
        </p>
      </div>

      {market.by_asset.length > 0 ? (
        <div className="overflow-x-auto border-t border-border">
          <table className="w-full min-w-[640px] text-left text-xs" data-testid="tax-assets">
            <thead>
              <tr className="border-b border-border text-[10px] tracking-[0.12em] text-muted uppercase">
                <th scope="col" className="px-4 py-2 font-semibold sm:px-5">
                  Asset
                </th>
                <th scope="col" className="px-2 py-2 text-right font-semibold">
                  Sales
                </th>
                <th scope="col" className="px-2 py-2 text-right font-semibold">
                  Gross sales
                </th>
                <th scope="col" className="px-2 py-2 text-right font-semibold">
                  Cost basis
                </th>
                <th scope="col" className="px-2 py-2 text-right font-semibold">
                  Net result
                </th>
                <th scope="col" className="px-4 py-2 font-semibold sm:px-5">
                  Treatment
                </th>
              </tr>
            </thead>
            <tbody>
              {market.by_asset.map((a) => (
                <tr
                  key={a.ticker}
                  className="border-b border-border/60 align-top last:border-b-0"
                  data-testid="tax-asset-row"
                >
                  <td className="px-4 py-2.5 sm:px-5">
                    <span className="font-semibold text-ink">{a.ticker}</span>
                    <span className="block text-[11px] text-muted">
                      {a.asset_name} · {ASSET_CLASS_LABELS[a.asset_class]}
                    </span>
                  </td>
                  <td className="px-2 py-2.5 text-right text-ink-2 tabular-nums">{a.sells}</td>
                  <td className="px-2 py-2.5 text-right whitespace-nowrap">
                    <Money value={a.proceeds} />
                  </td>
                  <td className="px-2 py-2.5 text-right whitespace-nowrap">
                    <Money value={a.cost_basis} />
                  </td>
                  <td className="px-2 py-2.5 text-right whitespace-nowrap">
                    <PnL value={a.net_result} />
                  </td>
                  <td className="px-4 py-2.5 text-ink-2 sm:px-5">{treatmentLabel(a.treatment, rates)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <p className="border-t border-border px-4 py-4 text-sm text-muted sm:px-5">No sales in {summary.year}.</p>
      )}

      <p className="border-t border-border px-4 py-3 text-[11px] leading-relaxed text-muted sm:px-5">
        An estimate from the year&apos;s published rates, not a tax determination. Your annual return and the Constancia
        de Retenciones issued for the year determine the tax.
      </p>
    </section>
  );
}

function Tile({
  label,
  hint,
  testId,
  accent = false,
  children,
}: {
  label: string;
  hint?: string;
  testId: string;
  accent?: boolean;
  children: ReactNode;
}) {
  return (
    <div
      className={cx("bg-surface px-4 py-4 sm:px-5", accent && "outline-2 -outline-offset-2 outline-synthetic-yellow")}
    >
      <dt className="text-[11px] font-semibold tracking-[0.14em] text-muted uppercase">{label}</dt>
      <dd className="mt-1.5 text-lg font-semibold text-ink tabular-nums" data-testid={testId}>
        {children}
      </dd>
      {hint ? <dd className="mt-0.5 text-[11px] text-muted">{hint}</dd> : null}
    </div>
  );
}

function PreviewRow({ label, basis, children }: { label: string; basis: string; children: ReactNode }) {
  return (
    <div className="flex items-baseline justify-between gap-4 py-1.5">
      <dt className="min-w-0 text-ink-2">
        {label}
        <span className="block text-[11px] text-muted">{basis}</span>
      </dt>
      <dd className="shrink-0 font-semibold text-ink">{children}</dd>
    </div>
  );
}
