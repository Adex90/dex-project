"use client";

import { useMemo, useState, type FormEvent } from "react";
import { ArrowRight, ArrowRightLeft, CheckCircle2, Info } from "lucide-react";

import { HashText, Money, formatDate } from "@/components/ui/data";
import { Alert, Button, Card, CardHeader, Field, Input, Skeleton, cx } from "@/components/ui/primitives";
import { describeError } from "@/lib/api/client";
import { api, newIdempotencyKey } from "@/lib/api/endpoints";
import type { PlazoTasa, ResultadoMutuo } from "@/lib/api/types";
import { addDays, todayUtc, trimDecimal } from "@/lib/dates";
import { addUnits, compareAmounts, formatRate, interestAct360, parseAmountInput } from "@/lib/decimal";

/**
 * IFPE -> SOFOM bridge. Moves liquid vault funds into a fixed-term mutuum
 * contract. The idempotency key is created per (amount, term) and reused
 * if the same request is retried, so a double click or a network retry can
 * never create two contracts.
 */
export function BridgeForm({
  available,
  rates,
  token,
  disabled,
  onInvested,
}: {
  available: string | null;
  rates: PlazoTasa[];
  token: string;
  disabled?: boolean;
  onInvested: () => Promise<void>;
}) {
  const [amount, setAmount] = useState("");
  const [term, setTerm] = useState<number | null>(null);
  const [acknowledged, setAcknowledged] = useState(false);
  const [key, setKey] = useState(newIdempotencyKey);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [receipt, setReceipt] = useState<ResultadoMutuo | null>(null);

  const selected = rates.find((r) => r.plazo_dias === term) ?? null;
  const parsed = amount ? parseAmountInput(amount) : null;
  const exceeds = Boolean(parsed?.ok && available !== null && compareAmounts(parsed.value, available) > 0);
  const amountError = parsed && !parsed.ok ? parsed.error : exceeds ? "More than your available vault balance." : null;

  const preview = useMemo(() => {
    if (!parsed?.ok || !selected) return null;
    const interest = interestAct360(parsed.value, selected.tasa_anual, selected.plazo_dias);
    return {
      interest,
      total: addUnits(parsed.value, interest),
      maturity: addDays(todayUtc(), selected.plazo_dias),
    };
  }, [parsed, selected]);

  const canSubmit = Boolean(parsed?.ok && !exceeds && selected && acknowledged && !disabled);

  const changeAmount = (value: string) => {
    setAmount(value);
    setKey(newIdempotencyKey());
    setError(null);
  };
  const changeTerm = (value: number) => {
    setTerm(value);
    setKey(newIdempotencyKey());
    setError(null);
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!canSubmit || !parsed?.ok || !selected) return;
    setSubmitting(true);
    setError(null);
    try {
      const result = await api.invest(token, {
        monto: parsed.value,
        plazo_dias: selected.plazo_dias,
        clave_idempotencia: key,
      });
      setReceipt(result);
      setAmount("");
      setTerm(null);
      setAcknowledged(false);
      setKey(newIdempotencyKey());
      await onInvested();
    } catch (e) {
      // Keep the same key: retrying the same request is safe and idempotent.
      setError(describeError(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Card aria-labelledby="bridge-title">
      <CardHeader
        icon={<ArrowRightLeft className="size-4.5" aria-hidden />}
        eyebrow="IFPE → SOFOM bridge"
        title={<span id="bridge-title">Invest at a fixed term</span>}
      />
      <div className="p-5">
        {receipt ? (
          <Receipt receipt={receipt} onDone={() => setReceipt(null)} />
        ) : (
          <form onSubmit={submit} className="space-y-5" noValidate>
            <Field
              label="Amount to move from your vault"
              htmlFor="bridge-amount"
              error={amountError}
              hint={
                available !== null ? (
                  <>
                    Available: <Money value={available} />
                  </>
                ) : undefined
              }
            >
              <div className="flex gap-2">
                <div className="relative flex-1">
                  <span className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-sm text-muted">$</span>
                  <Input
                    id="bridge-amount"
                    inputMode="decimal"
                    autoComplete="off"
                    placeholder="0.00"
                    className="pl-6 tabular-nums"
                    value={amount}
                    invalid={Boolean(amountError)}
                    aria-describedby={amountError ? "bridge-amount-error" : undefined}
                    onChange={(e) => changeAmount(e.target.value)}
                  />
                </div>
                <Button
                  type="button"
                  variant="secondary"
                  onClick={() => available && changeAmount(trimDecimal(available))}
                  disabled={!available || compareAmounts(available, "0") <= 0}
                >
                  Max
                </Button>
              </div>
            </Field>

            <fieldset className="space-y-2">
              <legend className="text-xs font-medium text-ink-2">Term</legend>
              {rates.length === 0 ? (
                <Skeleton className="h-16 w-full" />
              ) : (
                <div className="grid grid-cols-3 gap-2 sm:grid-cols-5">
                  {rates.map((r) => (
                    <label
                      key={r.plazo_dias}
                      className={cx(
                        "cursor-pointer rounded-lg border px-2 py-2.5 text-center transition",
                        "focus-within:ring-2 focus-within:ring-accent",
                        term === r.plazo_dias
                          ? "border-accent bg-accent-soft"
                          : "border-border-strong hover:border-accent/60",
                      )}
                    >
                      <input
                        type="radio"
                        name="term"
                        value={r.plazo_dias}
                        checked={term === r.plazo_dias}
                        onChange={() => changeTerm(r.plazo_dias)}
                        className="sr-only"
                      />
                      <span className="block text-sm font-semibold">{r.plazo_dias}d</span>
                      <span className="block text-[11px] text-muted">{formatRate(r.tasa_anual)} APR</span>
                    </label>
                  ))}
                </div>
              )}
            </fieldset>

            {preview ? (
              <dl className="grid grid-cols-3 gap-3 rounded-lg bg-surface-2 p-3 text-xs animate-reveal">
                <div>
                  <dt className="text-muted">Matures</dt>
                  <dd className="mt-0.5 font-semibold">{formatDate(preview.maturity)}</dd>
                </div>
                <div>
                  <dt className="text-muted">Interest</dt>
                  <dd className="mt-0.5 font-semibold">
                    <Money value={preview.interest} />
                  </dd>
                </div>
                <div>
                  <dt className="text-muted">You receive</dt>
                  <dd className="mt-0.5 font-semibold">
                    <Money value={preview.total} />
                  </dd>
                </div>
              </dl>
            ) : null}

            <div className="flex gap-2 rounded-lg border border-info/25 bg-info-soft px-3 py-2.5 text-[12px] leading-relaxed text-info">
              <Info className="mt-0.5 size-3.5 shrink-0" aria-hidden />
              <p>
                These funds leave the 100% reserve vault and become a fixed-term loan (mutuum) to the credit division.
                They are lent only to borrowers whose loans mature on or before your contract, and return to your vault
                with interest at maturity.
              </p>
            </div>

            <label className="flex items-start gap-2 text-sm text-ink-2">
              <input
                type="checkbox"
                checked={acknowledged}
                onChange={(e) => setAcknowledged(e.target.checked)}
                className="mt-0.5 size-4 accent-[var(--accent)]"
              />
              <span>I understand there is no early withdrawal before the maturity date.</span>
            </label>

            {disabled ? (
              <Alert tone="warning">Money movements are available once your identity (KYC) is verified.</Alert>
            ) : null}
            {error ? <Alert tone="error">{error}</Alert> : null}

            <Button type="submit" size="lg" className="w-full" loading={submitting} disabled={!canSubmit}>
              Lock funds in a mutuum contract <ArrowRight className="size-4" aria-hidden />
            </Button>
          </form>
        )}
      </div>
    </Card>
  );
}

function Receipt({ receipt, onDone }: { receipt: ResultadoMutuo; onDone: () => void }) {
  const c = receipt.contrato;
  return (
    <div className="space-y-4 animate-reveal" role="status">
      <div className="flex items-start gap-3">
        <CheckCircle2 className="mt-0.5 size-5 shrink-0 text-verified" aria-hidden />
        <div>
          <p className="font-semibold">Contract #{String(c.folio).padStart(6, "0")} created</p>
          <p className="text-sm text-muted">
            <Money value={c.principal} /> at {formatRate(c.tasa_anual)} for {c.plazo_dias} days. Matures{" "}
            {formatDate(c.fecha_vencimiento)} with <Money value={receipt.interes_al_vencimiento} /> interest.
          </p>
        </div>
      </div>
      <dl className="space-y-2 rounded-lg bg-surface-2 p-3 text-xs">
        {(
          [
            ["Terms hash (SHA-256)", receipt.terminos.hash],
            ["Vault ledger entry", receipt.tx_ifpe.tx_hash],
            ["SOFOM ledger entry", receipt.tx_sofom.tx_hash],
          ] as const
        ).map(([label, hash]) => (
          <div key={label} className="flex items-center justify-between gap-3">
            <dt className="text-muted">{label}</dt>
            <dd className="min-w-0">
              <HashText value={hash} head={14} tail={8} label={label} />
            </dd>
          </div>
        ))}
      </dl>
      <p className="text-xs text-muted">
        Both entries reference each other: the money left the vault&apos;s custody and entered the SOFOM&apos;s, in a
        single atomic transaction.
      </p>
      <Button variant="secondary" onClick={onDone}>
        Make another investment
      </Button>
    </div>
  );
}
