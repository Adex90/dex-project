"use client";

import { useState, type FormEvent } from "react";
import { Landmark, Plus, ShieldAlert, ShieldCheck, X } from "lucide-react";

import { KycLockNote, MovementErrorAlert, kycBlockCode } from "@/components/dashboard/kyc-status";
import { HashText, Money } from "@/components/ui/data";
import { Alert, Badge, Button, Card, CardHeader, Input, Skeleton, Stat } from "@/components/ui/primitives";
import { ApiError } from "@/lib/api/client";
import { api, newIdempotencyKey } from "@/lib/api/endpoints";
import type { CuentaIfpe, EstadoReservaVista } from "@/lib/api/types";
import { HOLD_COPY } from "@/lib/aml";
import { addUnits, formatMoney, isZero, parseAmountInput } from "@/lib/decimal";

function formatClabe(clabe: string): string {
  return `${clabe.slice(0, 3)} ${clabe.slice(3, 6)} ${clabe.slice(6, 17)} ${clabe.slice(17)}`;
}

/** IFPE sight vault: 100% liquid, 100% backed, never lent. */
export function VaultCard({
  account,
  reserve,
  token,
  kycLocked = false,
  onChanged,
}: {
  account: CuentaIfpe | null;
  reserve: EstadoReservaVista | null;
  token: string;
  /** Identity not verified yet: deposits are refused by the bank (and its database). */
  kycLocked?: boolean;
  onChanged: () => Promise<void>;
}) {
  const backed = reserve?.reserva_100 ?? false;

  return (
    <Card className="flex flex-col" aria-labelledby="vault-title">
      <CardHeader
        icon={<Landmark className="size-4.5" aria-hidden />}
        eyebrow="IFPE · Sight vault"
        title={<span id="vault-title">Vault balance</span>}
        action={
          reserve ? (
            backed ? (
              <Badge tone="verified" icon={<ShieldCheck className="size-3" aria-hidden />}>
                {/* Short label on phones: the full one does not fit next to the title. */}
                <span className="sm:hidden">100% backed</span>
                <span className="hidden sm:inline">100% Fully Backed in Custody</span>
              </Badge>
            ) : (
              <Badge tone="danger" icon={<ShieldAlert className="size-3" aria-hidden />}>
                Reserve mismatch
              </Badge>
            )
          ) : null
        }
      />
      <div className="flex flex-1 flex-col gap-5 p-5">
        {account ? (
          <>
            <div>
              <p className="text-xs text-muted">Available now, 100% liquid</p>
              <p className="mt-1 text-3xl font-semibold tracking-tight sm:text-4xl" data-testid="vault-available">
                <Money value={account.saldo_disponible} currencyClassName="text-base" />
              </p>
              {/* Neutral wording: a hold must never disclose why it exists (no tipping off). */}
              {!isZero(account.saldo_bloqueado) ? (
                <p className="mt-1 text-xs text-warning">
                  {formatMoney(account.saldo_bloqueado)} MXN {HOLD_COPY}
                </p>
              ) : null}
            </div>
            {/* One column on phones: the CLABE must be readable in full, never ellipsized. */}
            <dl className="grid grid-cols-1 gap-4 rounded-lg bg-surface-2 p-3 sm:grid-cols-2">
              <Stat
                label="Your CLABE"
                value={<span className="font-mono text-[13px]">{formatClabe(account.clabe)}</span>}
                hint={kycLocked ? "SPEI transfers are returned to the sender until you are verified" : undefined}
              />
              <Stat
                label="Held for you"
                value={<Money value={addUnits(account.saldo_disponible, account.saldo_bloqueado)} />}
                hint="Matched 1:1 by cash in custody"
              />
            </dl>
            {/* Only the disclosed legal freeze (CONGELADA) shows here: the preventive restriction arrives as ACTIVA. */}
            {account.estado !== "ACTIVA" ? (
              <Alert tone="warning" title={`Account ${account.estado.toLowerCase()}`}>
                Outgoing movements are disabled.
              </Alert>
            ) : null}
            {kycLocked ? <LockedDeposit /> : <DemoDeposit token={token} onDone={onChanged} />}
          </>
        ) : (
          <div className="space-y-3">
            <Skeleton className="h-10 w-2/3" />
            <Skeleton className="h-16 w-full" />
          </div>
        )}

        {reserve ? (
          <div className="mt-auto space-y-1.5 border-t border-border pt-4 text-xs text-muted">
            <p className="flex justify-between gap-2">
              <span>Bank-wide client deposits</span>
              <span className="font-medium text-ink-2 tabular-nums">{formatMoney(reserve.pasivo_clientes)}</span>
            </p>
            <p className="flex justify-between gap-2">
              <span>Cash in custody{reserve.custodia[0] ? ` · ${reserve.custodia[0].institucion}` : ""}</span>
              <span className="font-medium text-ink-2 tabular-nums">{formatMoney(reserve.activo_reserva)}</span>
            </p>
            <p className="flex items-center justify-between gap-2">
              <span>Ledger height #{reserve.ledger.ultima_secuencia}</span>
              <HashText value={reserve.ledger.ultimo_hash} head={8} tail={6} label="ledger head hash" className="text-[11px]" />
            </p>
          </div>
        ) : null}
      </div>
    </Card>
  );
}

/**
 * The deposit control while the identity is not verified: visibly disabled
 * but still focusable (aria-disabled), so the reason can be read.
 */
function LockedDeposit() {
  return (
    <div className="space-y-1.5">
      <Button
        type="button"
        variant="secondary"
        size="sm"
        className="cursor-not-allowed opacity-50 hover:bg-surface"
        aria-disabled="true"
        aria-describedby="deposit-locked-note"
        data-testid="deposit-locked"
      >
        <Plus className="size-3.5" aria-hidden /> Add funds (demo SPEI)
      </Button>
      <KycLockNote id="deposit-locked-note">Deposits unlock once your identity is verified.</KycLockNote>
    </div>
  );
}

/** Simulated SPEI deposit. Only works when the backend runs with MODO_DEMO=true. */
function DemoDeposit({ token, onDone }: { token: string; onDone: () => Promise<void> }) {
  const [open, setOpen] = useState(false);
  const [amount, setAmount] = useState("");
  const [key, setKey] = useState(newIdempotencyKey);
  const [error, setError] = useState<Error | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const parsed = parseAmountInput(amount);
    if (!parsed.ok) {
      setError(new Error(parsed.error));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api.simulatedDeposit(token, parsed.value, key);
      setAmount("");
      setKey(newIdempotencyKey());
      setOpen(false);
      await onDone();
    } catch (e) {
      setError(
        e instanceof ApiError && e.status === 404
          ? new Error(
              "Simulated deposits are disabled on this server (MODO_DEMO=false). Fund your account by SPEI to your CLABE.",
            )
          : e instanceof Error
            ? e
            : new Error(String(e)),
      );
      // The status changed behind our back (e.g. verification revoked): refresh it so the KYC banner shows.
      if (kycBlockCode(e)) void onDone();
    } finally {
      setBusy(false);
    }
  };

  if (!open) {
    return (
      <Button variant="secondary" size="sm" className="self-start" onClick={() => setOpen(true)}>
        <Plus className="size-3.5" aria-hidden /> Add funds (demo SPEI)
      </Button>
    );
  }
  return (
    <form onSubmit={submit} className="space-y-2 rounded-lg border border-dashed border-border-strong p-3">
      <div className="flex items-center justify-between">
        <label htmlFor="demo-amount" className="text-xs font-medium text-ink-2">
          Simulated incoming SPEI transfer
        </label>
        <button
          type="button"
          onClick={() => setOpen(false)}
          className="rounded p-0.5 text-muted hover:text-ink"
          aria-label="Close"
        >
          <X className="size-4" aria-hidden />
        </button>
      </div>
      <div className="flex gap-2">
        <Input
          id="demo-amount"
          inputMode="decimal"
          placeholder="10,000.00"
          value={amount}
          onChange={(e) => {
            setAmount(e.target.value);
            setKey(newIdempotencyKey());
          }}
        />
        <Button type="submit" loading={busy}>
          Deposit
        </Button>
      </div>
      <p className="text-[11px] text-muted">The cash in custody rises by the same amount, in the same transaction.</p>
      {error ? <MovementErrorAlert error={error} /> : null}
    </form>
  );
}
