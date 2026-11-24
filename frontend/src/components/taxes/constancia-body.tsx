import { Hash, Row, Rows, Section, type Variant } from "@/components/documents/document-body";
import { Money } from "@/components/ui/data";
import { isZero } from "@/lib/decimal";
import { documentDate } from "@/lib/documents";
import { constanciaFigures, countryName, formatRate } from "@/lib/taxes";
import type { TaxCenter } from "@/types/taxes";

/*
 * The Constancia de Retenciones, as a preview the client can print: the
 * year's interest, its withholding, the result of selling listed shares and
 * the other results that go to the annual return, from the server's summary.
 *
 * It is not a CFDI: the real constancia is issued with the SAT's fiscal stamp
 * once the year closes, and it reports real interest, which needs the INPC.
 * The page says so. Estimates are shown apart from what was withheld.
 */

export function ConstanciaBody({ center, variant = "screen" }: { center: TaxCenter; variant?: Variant }) {
  const { profile, summary } = center;
  const figures = constanciaFigures(summary);
  const estimate = summary.isr.estimated;
  const rates = summary.isr.rates;
  const period = `${documentDate(summary.period_start)} – ${documentDate(summary.period_end)}`;
  return (
    <article className="print-document space-y-6 bg-surface text-ink" data-testid={`constancia-${variant}`}>
      <header className="flex flex-wrap items-start justify-between gap-4 border-b-2 border-synthetic-yellow pb-4">
        <div className="min-w-0 space-y-1">
          <p className="text-[11px] font-bold tracking-[0.3em] text-synthetic-yellow uppercase">Synthetic Capital</p>
          <h3 className="text-xl font-semibold tracking-tight text-ink">Constancia de Retenciones</h3>
          <p className="text-xs text-muted">Annual withholding and income summary · Preview</p>
        </div>
        <dl className="grid shrink-0 grid-cols-[auto_auto] gap-x-4 gap-y-1 text-xs">
          <dt className="text-muted">Fiscal year</dt>
          <dd className="text-right font-mono font-semibold text-ink" data-testid="constancia-year">
            {summary.year}
          </dd>
          <dt className="text-muted">Period</dt>
          <dd className="text-right text-ink">{period}</dd>
          <dt className="text-muted">Status</dt>
          <dd className="text-right font-semibold text-ink">
            {summary.in_progress ? "Year in progress" : "Year closed"}
          </dd>
        </dl>
      </header>

      <Section title="Recipient (receptor)">
        <Rows>
          <Row label="Name">{profile.name}</Row>
          <Row label="RFC">
            <span className="font-mono">{profile.rfc ?? "—"}</span>
          </Row>
          <Row label="CURP">
            <span className="font-mono">{profile.curp ?? "—"}</span>
          </Row>
          <Row label="Nationality">{countryName(profile.nationality)}</Row>
          <Row label="Tax residence">{countryName(profile.tax_residence)}</Row>
        </Rows>
      </Section>

      <Section title="Interest · Complemento de intereses" testId="constancia-interest">
        <Rows>
          <Row label="Paid by a member of the financial system">Yes (SOFOM)</Row>
          <Row label="Nominal interest paid">
            <span data-testid="constancia-nominal-interest">
              <Money value={figures.nominalInterest} />
            </span>
          </Row>
          <Row label="Real interest">
            <span className="text-ink-2">Determined with the INPC when the CFDI is issued</span>
          </Row>
          <Row label="ISR withheld" strong>
            <span data-testid="constancia-withheld">
              <Money value={figures.withheld} />
            </span>
          </Row>
        </Rows>
      </Section>

      <Section title="Sale of shares · Complemento de enajenación de acciones" testId="constancia-shares">
        <Rows>
          <Row label="Gain">
            <span data-testid="constancia-gain">
              <Money value={figures.listedGain} />
            </span>
          </Row>
          <Row label="Loss">
            <span data-testid="constancia-loss">
              <Money value={figures.listedLoss} />
            </span>
          </Row>
        </Rows>
        {isZero(figures.listedLoss) ? null : (
          <p className="text-xs text-muted">A net loss can be applied against gains of the same kind in later years.</p>
        )}
      </Section>

      <Section title="Other results for the annual return">
        <Rows>
          <Row label="Commodities and fixed income (net)">
            <Money
              value={figures.otherResult.replace(/^-/, "")}
              sign={figures.otherResult.startsWith("-") ? "-" : undefined}
            />
          </Row>
        </Rows>
      </Section>

      <Section title="Estimated ISR · not withheld" testId="constancia-estimate">
        {estimate && rates ? (
          <Rows>
            <Row label={`Withholding on fixed-term capital (${formatRate(rates.interest_withholding_annual)} a year)`}>
              <Money value={estimate.interest_withholding} />
            </Row>
            <Row label={`Listed shares (${formatRate(rates.listed_shares)} of the net gain)`}>
              <Money value={estimate.listed_shares_tax} />
            </Row>
            <Row label="Estimated total" strong>
              <span data-testid="constancia-estimated-total">
                <Money value={estimate.total} />
              </span>
            </Row>
          </Rows>
        ) : (
          <p className="text-sm text-ink-2">No rates are published for {summary.year} yet, so there is no estimate.</p>
        )}
        {rates ? (
          <p className="text-[11px] text-muted">
            {rates.interest_basis} · {rates.listed_shares_basis}
          </p>
        ) : null}
      </Section>

      <Section title="Reference">
        <Rows>
          <Row label="Summary hash">
            <Hash value={summary.reference} variant={variant} label="summary hash" testId="constancia-reference" />
          </Row>
        </Rows>
      </Section>

      <p className="border-t border-border pt-4 text-[11px] leading-relaxed text-muted">
        Preview from Synthetic Capital&apos;s hash-chained ledgers. It is not a CFDI: the Constancia de Retenciones is
        issued as a CFDI with its SAT fiscal stamp once the year closes. Estimates are not a tax determination; your
        annual return determines the tax.
      </p>
    </article>
  );
}
