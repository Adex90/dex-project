"use client";

import { useEffect, useState } from "react";
import { ShieldAlert, ShieldCheck } from "lucide-react";

import { api } from "@/lib/api/endpoints";
import type { EstadoReservaVista } from "@/lib/api/types";
import { formatMoney } from "@/lib/decimal";
import { Skeleton } from "./ui/primitives";

/** Public, live view of the bank's reserve: liabilities vs. custody. */
export function ReserveTicker() {
  const [status, setStatus] = useState<EstadoReservaVista | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    const controller = new AbortController();
    const load = () =>
      api
        .reserveStatus(controller.signal)
        .then((s) => {
          setStatus(s);
          setFailed(false);
        })
        .catch((e: unknown) => {
          if (!(e instanceof DOMException && e.name === "AbortError")) setFailed(true);
        });
    load();
    const id = setInterval(load, 20_000);
    return () => {
      controller.abort();
      clearInterval(id);
    };
  }, []);

  if (failed) {
    return <p className="text-sm text-muted">Live reserve data is unavailable right now.</p>;
  }
  if (!status) return <Skeleton className="h-16 w-full max-w-xl" />;

  const ok = status.reserva_100;
  return (
    <div className="grid w-full max-w-xl grid-cols-3 divide-x divide-border rounded-xl border border-border bg-surface text-left shadow-sm">
      <div className="p-4">
        <p className="text-[11px] tracking-wide text-muted uppercase">Client deposits</p>
        <p className="mt-1 text-sm font-semibold tabular-nums">{formatMoney(status.pasivo_clientes)}</p>
      </div>
      <div className="p-4">
        <p className="text-[11px] tracking-wide text-muted uppercase">Cash in custody</p>
        <p className="mt-1 text-sm font-semibold tabular-nums">{formatMoney(status.activo_reserva)}</p>
      </div>
      <div className="p-4">
        <p className="text-[11px] tracking-wide text-muted uppercase">Reserve ratio</p>
        <p className={`mt-1 inline-flex items-center gap-1 text-sm font-semibold ${ok ? "text-verified" : "text-danger"}`}>
          {ok ? <ShieldCheck className="size-4" aria-hidden /> : <ShieldAlert className="size-4" aria-hidden />}
          {ok ? "100%" : "Mismatch"}
        </p>
      </div>
    </div>
  );
}
