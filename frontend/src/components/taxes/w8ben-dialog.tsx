"use client";

import { useState } from "react";
import { Check, FileSignature, X } from "lucide-react";

import { DocumentDialog } from "@/components/documents/document-dialog";
import { Alert, Button, Field, Input, cx } from "@/components/ui/primitives";
import { describeError } from "@/lib/api/client";
import { formatDate } from "@/components/ui/data";
import { todayInMexicoCity } from "@/lib/kyc";
import { certificationRequest, countryName, formatTaxDate, signatureMatches } from "@/lib/taxes";
import type { TaxProfile, W8benCertification, W8benState } from "@/types/taxes";

/*
 * Certifying (or renewing) Form W-8BEN, in three steps:
 *
 * 1. Part I, the beneficial owner: what the verified identity file says. The
 *    client does not type it; the database copies it when the form is saved.
 * 2. Parts II and III: the treaty claim and the three statements, each
 *    confirmed on its own.
 * 3. The signature: the full legal name, typed, compared as the server
 *    compares it. The server stamps the date and the validity (December 31
 *    of the third year after signing).
 */

type Step = "details" | "statements" | "sign" | "done";

const STATEMENTS = [
  {
    key: "beneficialOwner",
    testId: "w8ben-check-beneficial-owner",
    text: "I am the individual who is the beneficial owner of the income this form relates to.",
  },
  {
    key: "notUsPerson",
    testId: "w8ben-check-not-us-person",
    text: "I am not a U.S. person: neither a citizen nor a resident of the United States for tax purposes.",
  },
  {
    key: "treatyResident",
    testId: "w8ben-check-treaty-resident",
    text: "I am a resident of Mexico within the meaning of the income tax treaty between the United States and Mexico.",
  },
] as const;

type Statements = { beneficialOwner: boolean; notUsPerson: boolean; treatyResident: boolean };

export function W8benDialog({
  profile,
  renewing,
  treatyRate,
  onSubmit,
  onClose,
}: {
  profile: TaxProfile;
  renewing: boolean;
  /** The treaty's rate on US-source dividends ("10.00"), if published. */
  treatyRate: string | null;
  onSubmit: (body: W8benCertification) => Promise<W8benState>;
  onClose: () => void;
}) {
  const [step, setStep] = useState<Step>("details");
  const [checked, setChecked] = useState<Statements>({
    beneficialOwner: false,
    notUsPerson: false,
    treatyResident: false,
  });
  const [signature, setSignature] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [result, setResult] = useState<W8benState | null>(null);
  const allChecked = checked.beneficialOwner && checked.notUsPerson && checked.treatyResident;
  const matches = signatureMatches(signature, profile.name);
  const validUntil = `${Number(todayInMexicoCity().slice(0, 4)) + 3}-12-31`;

  async function sign() {
    if (!allChecked || !matches || busy) return;
    setBusy(true);
    setError(null);
    try {
      setResult(await onSubmit(certificationRequest(signature, checked)));
      setStep("done");
    } catch (cause) {
      setError(cause);
    } finally {
      setBusy(false);
    }
  }

  const steps: { id: Step; label: string }[] = [
    { id: "details", label: "Details" },
    { id: "statements", label: "Certify" },
    { id: "sign", label: "Sign" },
  ];
  const current = steps.findIndex((s) => s.id === step);

  return (
    <DocumentDialog
      eyebrow="Form W-8BEN"
      title={renewing ? "Renew your foreign status" : "Certify your foreign status"}
      testId="w8ben-dialog"
      onClose={onClose}
      footer={
        step === "done" ? (
          <Button size="lg" onClick={onClose} data-testid="w8ben-done" className="min-w-40">
            Done
          </Button>
        ) : (
          <>
            {step !== "details" ? (
              <Button
                variant="ghost"
                size="lg"
                onClick={() => setStep(step === "sign" ? "statements" : "details")}
                disabled={busy}
              >
                Back
              </Button>
            ) : null}
            {step === "sign" ? (
              <Button
                size="lg"
                onClick={() => void sign()}
                disabled={!allChecked || !matches}
                loading={busy}
                className="min-w-56 font-semibold"
                data-testid="w8ben-sign"
              >
                <FileSignature className="size-4" aria-hidden /> Sign and certify
              </Button>
            ) : (
              <Button
                size="lg"
                onClick={() => setStep(step === "details" ? "statements" : "sign")}
                disabled={step === "statements" && !allChecked}
                className="min-w-40"
                data-testid="w8ben-continue"
              >
                Continue
              </Button>
            )}
          </>
        )
      }
    >
      {step !== "done" ? (
        <ol className="mb-5 grid grid-cols-3 border border-border" aria-label="Steps">
          {steps.map((s, i) => (
            <li
              key={s.id}
              aria-current={i === current ? "step" : undefined}
              className={cx(
                "border-r border-border px-2 py-2 text-center text-[11px] font-semibold tracking-[0.12em] uppercase last:border-r-0",
                i === current ? "bg-synthetic-yellow text-black" : i < current ? "text-synthetic-yellow" : "text-muted",
              )}
            >
              {i + 1} · {s.label}
            </li>
          ))}
        </ol>
      ) : null}

      {step === "details" ? (
        <div className="space-y-4" data-testid="w8ben-step-details">
          <p className="text-sm text-ink-2">
            Part I identifies you as the beneficial owner. It comes from your verified identity: if anything here
            changes, certify again.
          </p>
          <dl className="divide-y divide-border border border-border">
            {[
              ["Name", profile.name],
              ["Country of citizenship", countryName(profile.nationality)],
              ["Permanent residence address", profile.address],
              ["Foreign tax identifying number (RFC)", profile.rfc ?? "—"],
              ["Date of birth", profile.birth_date ? formatDate(profile.birth_date) : "—"],
            ].map(([label, value]) => (
              <div key={label} className="grid gap-1 px-4 py-3 sm:grid-cols-[14rem_1fr] sm:gap-4">
                <dt className="text-xs text-muted">{label}</dt>
                <dd className="min-w-0 text-sm font-medium break-words text-ink">{value}</dd>
              </div>
            ))}
          </dl>
        </div>
      ) : null}

      {step === "statements" ? (
        <div className="space-y-4" data-testid="w8ben-step-statements">
          <p className="text-sm text-ink-2">
            Part II claims the U.S.–Mexico treaty: U.S.-source dividends, and dividend equivalents on U.S.-linked
            synthetic equities, are withheld at {treatyRate ? `${Number(treatyRate)}%` : "the treaty rate"} instead of
            the statutory 30%. Part III is what you certify. Confirm each statement.
          </p>
          <fieldset className="space-y-2">
            <legend className="sr-only">Statements</legend>
            {STATEMENTS.map((s) => (
              <label
                key={s.key}
                className={cx(
                  "flex cursor-pointer items-start gap-3 border px-4 py-3 text-sm transition-colors",
                  checked[s.key]
                    ? "border-synthetic-yellow bg-synthetic-yellow/5 text-ink"
                    : "border-border text-ink-2",
                )}
              >
                <input
                  type="checkbox"
                  checked={checked[s.key]}
                  onChange={(e) => setChecked((c) => ({ ...c, [s.key]: e.target.checked }))}
                  className="mt-0.5 size-4 shrink-0 accent-[#eab308]"
                  data-testid={s.testId}
                />
                <span>{s.text}</span>
              </label>
            ))}
          </fieldset>
        </div>
      ) : null}

      {step === "sign" ? (
        <div className="space-y-4" data-testid="w8ben-step-sign">
          <p className="border-l-2 border-synthetic-yellow pl-3 text-sm text-ink-2">
            Under penalties of perjury, I declare that I have examined the information on this form and to the best of
            my knowledge and belief it is true, correct, and complete.
          </p>
          <Field
            label="Signature: type your full legal name"
            htmlFor="w8ben-signature"
            hint={
              <span>
                As in your file: <span className="font-semibold text-ink">{profile.name}</span>
              </span>
            }
          >
            <Input
              id="w8ben-signature"
              value={signature}
              onChange={(e) => setSignature(e.target.value)}
              autoComplete="off"
              spellCheck={false}
              maxLength={200}
              className="h-12 text-base"
              data-testid="w8ben-signature"
              aria-describedby="w8ben-signature-hint w8ben-signature-match"
            />
          </Field>
          <p
            id="w8ben-signature-match"
            className={cx("flex items-center gap-1.5 text-xs", matches ? "text-synthetic-yellow" : "text-muted")}
            data-testid="w8ben-signature-match"
            data-matches={matches}
          >
            {matches ? <Check className="size-3.5" aria-hidden /> : <X className="size-3.5" aria-hidden />}
            {matches ? "Matches your legal name" : "Does not match your legal name yet"}
          </p>
          <p className="text-xs text-muted">
            Signed today, valid through {formatTaxDate(validUntil)}, unless your details change.
          </p>
          {error ? (
            <Alert tone="error" title="Not certified">
              {describeError(error)}
            </Alert>
          ) : null}
        </div>
      ) : null}

      {step === "done" && result?.current ? (
        <div className="space-y-4" data-testid="w8ben-step-done">
          <Alert tone="success" title="W-8BEN certified">
            Version {result.current.version} is on record, valid through {formatTaxDate(result.current.valid_until)}.
          </Alert>
          <p className="text-sm text-ink-2">
            U.S.-source dividends are now withheld at{" "}
            {result.us_dividend_rate ? `${Number(result.us_dividend_rate)}%` : "the treaty rate"}. The certification is
            also in your security log.
          </p>
        </div>
      ) : null}
    </DocumentDialog>
  );
}
