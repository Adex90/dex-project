"use client";

import { useState, type FormEvent } from "react";
import { Landmark, Plus, ShieldAlert, ShieldCheck, X } from "lucide-react";

import { HashText, Money } from "@/components/ui/data";
import { Alert, Badge, Button, Card, CardHeader, Input, Skeleton, Stat } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { api, newIdempotencyKey } from "@/lib/api/endpoints";
import type { CuentaIfpe, EstadoReservaVista } from "@/lib/api/types";
import { addUnits, formatMoney, isZero, parseAmountInput } from "@/lib/decimal";

function formatClabe(clabe: string): string {
  return `${clabe.slice(0, 3)} ${clabe.slice(3, 6)} ${clabe.slice(6, 17)} ${clabe.slice(17)}`;
}

/** IFPE sight vault: 100% liquid, 100% backed, never lent. */
export function VaultCard({
  account,
  reserve,
  token,
  onChanged,
}: {
  account: CuentaIfpe | null;
  reserve: EstadoReservaVista | null;
  token: string;
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
                100% Fully Backed in Custody
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
              {!isZero(account.saldo_bloqueado) ? (
                <p className="mt-1 text-xs text-warning">
                  {formatMoney(account.saldo_bloqueado)} MXN on compliance hold (still fully backed)
                </p>
              ) : null}
            </div>
            <dl className="grid grid-cols-2 gap-4 rounded-lg bg-surface-2 p-3">
              <Stat label="Your CLABE" value={<span className="font-mono text-[13px]">{formatClabe(account.clabe)}</span>} />
              <Stat
                label="Held for you"
                value={<Money value={addUnits(account.saldo_disponible, account.saldo_bloqueado)} />}
                hint="Matched 1:1 by cash in custody"
              />
            </dl>
            {account.estado !== "ACTIVA" ? (
              <Alert tone="warning" title={`Account ${account.estado.toLowerCase()}`}>
                Outgoing movements are disabled.
              </Alert>
            ) : null}
            <DemoDeposit token={token} onDone={onChanged} />
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

/** Simulated SPEI deposit. Only works when the backend runs with MODO_DEMO=true. */
function DemoDeposit({ token, onDone }: { token: string; onDone: () => Promise<void> }) {
  const [open, setOpen] = useState(false);
  const [amount, setAmount] = useState("");
  const [key, setKey] = useState(newIdempotencyKey);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const parsed = parseAmountInput(amount);
    if (!parsed.ok) {
      setError(parsed.error);
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
          ? "Simulated deposits are disabled on this server (MODO_DEMO=false). Fund your account by SPEI to your CLABE."
          : describeError(e),
      );
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
      {error ? <Alert tone="error">{error}</Alert> : null}
    </form>
  );
}
