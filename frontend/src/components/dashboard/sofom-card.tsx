"use client";

import { useMemo, useState } from "react";
import { CalendarClock, CheckCircle2, Coins, Lock } from "lucide-react";

import { HashText, Money, formatDate } from "@/components/ui/data";
import { Alert, Badge, Button, Card, CardHeader, Skeleton, Stat, type Tone } from "@/components/ui/primitives";
import { describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import type { ContratoMutuo, EstadoMutuo, Posicion } from "@/lib/api/types";
import { daysBetween, todayUtc } from "@/lib/dates";
import { addUnits, formatRate, interestAct360 } from "@/lib/decimal";

const STATUS: Record<EstadoMutuo, { label: string; tone: Tone }> = {
  VIGENTE: { label: "Active", tone: "info" },
  VENCIDO: { label: "Matured", tone: "warning" },
  LIQUIDADO: { label: "Settled", tone: "verified" },
};

/** Interest earned so far on an active contract (ACT/360, same formula as the backend). */
function accrued(c: ContratoMutuo, today: string): string {
  const elapsed = Math.min(Math.max(daysBetween(c.fecha_inicio, today), 0), c.plazo_dias);
  return interestAct360(c.principal, c.tasa_anual, elapsed);
}

/** SOFOM fixed-term investments (mutuum contracts). */
export function SofomCard({
  position,
  token,
  onChanged,
}: {
  position: Posicion | null;
  token: string;
  onChanged: () => Promise<void>;
}) {
  const today = todayUtc();
  const active = useMemo(() => position?.contratos.filter((c) => c.estado === "VIGENTE") ?? [], [position]);
  const accruedTotal = useMemo(
    () => (active.length ? addUnits(...active.map((c) => accrued(c, today))) : "0"),
    [active, today],
  );

  return (
    <Card className="flex flex-col" aria-labelledby="sofom-title">
      <CardHeader
        icon={<Lock className="size-4.5" aria-hidden />}
        eyebrow="SOFOM · Fixed-term investments"
        title={<span id="sofom-title">Mutuum contracts</span>}
        action={
          <Badge tone="accent" icon={<CalendarClock className="size-3" aria-hidden />}>
            Maturity-matched
          </Badge>
        }
      />
      {position ? (
        <div className="flex flex-1 flex-col gap-5 p-5">
          <dl className="grid grid-cols-2 gap-4 sm:grid-cols-3">
            <div className="col-span-2 sm:col-span-1">
              <dt className="text-xs text-muted">Locked in contracts</dt>
              <dd className="mt-1 text-2xl font-semibold tracking-tight" data-testid="sofom-locked">
                <Money value={position.total_invertido} currencyClassName="text-sm" />
              </dd>
            </div>
            <Stat label="Accrued to date" value={<Money value={accruedTotal} />} hint="ACT/360, simple interest" />
            <Stat label="Interest at maturity" value={<Money value={position.intereses_por_recibir} />} />
          </dl>

          {position.contratos.length === 0 ? (
            <div className="flex flex-1 flex-col items-center justify-center rounded-lg border border-dashed border-border-strong p-6 text-center">
              <Coins className="size-6 text-muted" aria-hidden />
              <p className="mt-2 text-sm font-medium">No contracts yet</p>
              <p className="mt-1 max-w-sm text-xs text-muted">
                Move funds from your vault to a fixed-term contract to earn interest. Your money is lent only to borrowers
                whose loans mature on or before your contract.
              </p>
            </div>
          ) : (
            <ul className="divide-y divide-border rounded-lg border border-border">
              {position.contratos.map((c) => (
                <ContractRow key={c.id} contract={c} today={today} token={token} onChanged={onChanged} />
              ))}
            </ul>
          )}
        </div>
      ) : (
        <div className="space-y-3 p-5">
          <Skeleton className="h-10 w-1/2" />
          <Skeleton className="h-24 w-full" />
        </div>
      )}
    </Card>
  );
}

function ContractRow({
  contract: c,
  today,
  token,
  onChanged,
}: {
  contract: ContratoMutuo;
  today: string;
  token: string;
  onChanged: () => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const elapsed = Math.min(Math.max(daysBetween(c.fecha_inicio, today), 0), c.plazo_dias);
  const progress = Math.round((elapsed / c.plazo_dias) * 100);
  const matured = c.estado === "VIGENTE" && today >= c.fecha_vencimiento;
  const remaining = Math.max(daysBetween(today, c.fecha_vencimiento), 0);
  const status = STATUS[c.estado];

  const settle = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.settle(token, c.id);
      await onChanged();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <li className="space-y-2.5 p-3.5">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <span className="font-mono text-xs text-muted">#{String(c.folio).padStart(6, "0")}</span>
          <Money value={c.principal} className="text-sm font-semibold" />
          <Badge tone={status.tone}>{status.label}</Badge>
        </div>
        <div className="flex items-center gap-3 text-xs text-ink-2">
          <span>
            {formatRate(c.tasa_anual)} · {c.plazo_dias} days
          </span>
          {matured ? (
            <Button size="sm" onClick={settle} loading={busy}>
              <CheckCircle2 className="size-3.5" aria-hidden />
              Collect to vault
            </Button>
          ) : null}
        </div>
      </div>
      {c.estado === "VIGENTE" ? (
        <div>
          <div
            className="h-1.5 overflow-hidden rounded-full bg-surface-2"
            role="progressbar"
            aria-valuenow={progress}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-label={`Contract ${c.folio} term elapsed`}
          >
            <div className="h-full rounded-full bg-accent transition-all" style={{ width: `${progress}%` }} />
          </div>
          <div className="mt-1.5 flex justify-between text-[11px] text-muted">
            <span>{formatDate(c.fecha_inicio)}</span>
            <span>{matured ? "Matured, ready to collect" : `${remaining} days to maturity`}</span>
            <span>{formatDate(c.fecha_vencimiento)}</span>
          </div>
        </div>
      ) : (
        <p className="text-[11px] text-muted">
          {formatDate(c.fecha_inicio)} → {formatDate(c.fecha_vencimiento)}
          {c.estado === "LIQUIDADO" ? <> · interest paid <Money value={c.interes_pagado} /></> : null}
        </p>
      )}
      <div className="flex items-center gap-1.5 text-[11px] text-muted">
        <span>Terms hash</span>
        <HashText value={c.hash_terminos} head={12} tail={8} label="terms hash" className="text-[11px]" />
      </div>
      {error ? <Alert tone="error">{error}</Alert> : null}
    </li>
  );
}
