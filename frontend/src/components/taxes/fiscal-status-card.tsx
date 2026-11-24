import Link from "next/link";
import { AlertTriangle, BadgeCheck, FileSignature, ShieldCheck } from "lucide-react";

import { truncateHash } from "@/components/ui/data";
import { Button, cx } from "@/components/ui/primitives";
import { ONBOARDING_PATH } from "@/hooks/use-client-home";
import { documentTime } from "@/lib/documents";
import {
  W8BEN_BLOCK_MESSAGES,
  W8BEN_STATUS_LABELS,
  countryName,
  formatRate,
  formatTaxDate,
  validThrough,
} from "@/lib/taxes";
import type { TaxProfile, W8benState } from "@/types/taxes";

/*
 * Fiscal status: what the identity file says for tax purposes (RFC, CURP,
 * residence, citizenship), and the W-8BEN: its status, validity window, what
 * it changes (the treaty rate on U.S.-source dividends) and the gold
 * Certify / Renew action. Every version is immutable and chained; the
 * server checks the chain on every read.
 */

const STATUS_STYLE: Record<W8benState["status"], string> = {
  active: "bg-synthetic-yellow text-black border-synthetic-yellow",
  outdated: "border-warning text-warning",
  expired: "border-warning text-warning",
  pending: "border-border-strong text-ink-2",
};

export function FiscalStatusCard({
  profile,
  w8ben,
  onCertify,
}: {
  profile: TaxProfile;
  w8ben: W8benState;
  onCertify: () => void;
}) {
  const current = w8ben.current;
  const renew = w8ben.status !== "pending";
  return (
    <section
      className="border border-border bg-surface"
      aria-labelledby="fiscal-status-title"
      data-testid="fiscal-status"
    >
      <header className="flex items-center justify-between gap-3 border-b border-border px-4 py-3 sm:px-5">
        <h3 id="fiscal-status-title" className="text-sm font-semibold text-ink">
          Fiscal status
        </h3>
        {profile.identity_verified ? (
          <span className="inline-flex items-center gap-1 text-[11px] font-semibold text-synthetic-yellow">
            <BadgeCheck className="size-3.5" aria-hidden /> Identity verified
          </span>
        ) : null}
      </header>

      <dl className="grid grid-cols-1 gap-px bg-border sm:grid-cols-2">
        {[
          ["RFC", profile.rfc ?? "Not on file", "fiscal-rfc", true],
          ["CURP", profile.curp ?? "Not on file", "fiscal-curp", true],
          ["Tax residence", countryName(profile.tax_residence), "fiscal-residence", false],
          ["Citizenship", countryName(profile.nationality), "fiscal-citizenship", false],
        ].map(([label, value, testId, mono]) => (
          <div key={String(label)} className="bg-surface px-4 py-3 sm:px-5">
            <dt className="text-[11px] font-semibold tracking-[0.14em] text-muted uppercase">{label}</dt>
            <dd className={cx("mt-1 text-sm font-semibold text-ink", mono && "font-mono")} data-testid={String(testId)}>
              {value}
            </dd>
          </div>
        ))}
      </dl>

      <div
        className="space-y-4 border-t border-border px-4 py-4 sm:px-5"
        data-testid="w8ben-panel"
        data-status={w8ben.status}
      >
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0 space-y-1">
            <p className="flex items-center gap-2 text-sm font-semibold text-ink">
              <FileSignature className="size-4 text-synthetic-yellow" aria-hidden /> Form W-8BEN
              <span
                className={cx(
                  "border px-1.5 py-px text-[10px] font-bold tracking-[0.14em] uppercase",
                  STATUS_STYLE[w8ben.status],
                )}
                data-testid="w8ben-status"
              >
                {W8BEN_STATUS_LABELS[w8ben.status]}
              </span>
            </p>
            {current ? (
              <p className="text-xs text-ink-2" data-testid="w8ben-validity">
                {w8ben.status === "expired" ? "Expired" : validThrough(current.valid_until)} · signed{" "}
                {documentTime(current.signed_at)} · version {current.version}
              </p>
            ) : (
              <p className="text-xs text-ink-2">Not certified yet.</p>
            )}
          </div>
          <Button
            size="lg"
            onClick={onCertify}
            disabled={!w8ben.can_certify}
            className="font-semibold"
            data-testid="w8ben-certify"
          >
            <FileSignature className="size-4" aria-hidden /> {renew ? "Renew W-8BEN" : "Certify W-8BEN"}
          </Button>
        </div>

        {w8ben.status === "outdated" ? (
          <p className="flex items-start gap-2 border border-warning px-3 py-2 text-xs text-warning">
            <AlertTriangle className="mt-px size-4 shrink-0" aria-hidden />
            Your details changed since you signed. Certify again so the form matches your file.
          </p>
        ) : null}
        {w8ben.blocked_reason ? (
          <p className="text-xs text-ink-2" data-testid="w8ben-blocked">
            {W8BEN_BLOCK_MESSAGES[w8ben.blocked_reason]}{" "}
            {w8ben.blocked_reason === "identity_unverified" ? (
              <Link href={ONBOARDING_PATH} className="font-semibold text-synthetic-yellow underline underline-offset-4">
                Verify your identity
              </Link>
            ) : null}
          </p>
        ) : null}

        <p className="text-sm text-ink-2">
          For U.S.-linked synthetic equities such as STSLA: with an active W-8BEN, U.S.-source dividends and dividend
          equivalents are withheld at the U.S.–Mexico treaty rate
          {w8ben.us_dividend_rate_treaty ? ` of ${formatRate(w8ben.us_dividend_rate_treaty)}` : ""} instead of
          {w8ben.us_dividend_rate_statutory
            ? ` ${formatRate(w8ben.us_dividend_rate_statutory)}`
            : " the statutory rate"}
          .
        </p>
        {w8ben.us_dividend_rate ? (
          <p className="text-sm">
            <span className="text-muted">U.S. withholding on dividends today: </span>
            <span className="font-semibold text-ink tabular-nums" data-testid="w8ben-us-rate">
              {formatRate(w8ben.us_dividend_rate)}
            </span>
          </p>
        ) : null}

        {w8ben.history.length > 0 ? (
          <div className="space-y-2">
            <p
              className={cx(
                "flex items-start gap-2 border px-3 py-2 text-xs",
                w8ben.chain_verified ? "border-synthetic-yellow text-ink-2" : "border-warning text-warning",
              )}
              data-testid="w8ben-chain"
              data-verified={w8ben.chain_verified}
            >
              {w8ben.chain_verified ? (
                <ShieldCheck className="mt-px size-4 shrink-0 text-synthetic-yellow" aria-hidden />
              ) : (
                <AlertTriangle className="mt-px size-4 shrink-0" aria-hidden />
              )}
              {w8ben.chain_verified ? (
                <span>
                  <span className="font-semibold text-synthetic-yellow">Chain verified.</span> Every certification is
                  kept, signed and linked to the one before.
                </span>
              ) : (
                "The certification history could not be confirmed. Please contact support."
              )}
            </p>
            <ol className="divide-y divide-border border border-border" data-testid="w8ben-history">
              {w8ben.history.map((h) => (
                <li
                  key={h.version}
                  className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-0.5 px-3 py-2"
                >
                  <span className="text-xs font-semibold text-ink">Version {h.version}</span>
                  <span className="text-xs text-ink-2">{documentTime(h.signed_at)}</span>
                  <span className="w-full truncate font-mono text-[11px] text-muted" title={h.hash}>
                    {truncateHash(h.hash, 10, 6)}
                    {h.device ? ` · ${h.device}` : ""} · valid through {formatTaxDate(h.valid_until)}
                  </span>
                </li>
              ))}
            </ol>
          </div>
        ) : null}
      </div>
    </section>
  );
}
