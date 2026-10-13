"use client";

import { useEffect, useState } from "react";
import { LogOut, RefreshCw } from "lucide-react";

import { Brand } from "@/components/brand";
import { Badge, Button } from "@/components/ui/primitives";
import type { Cliente } from "@/lib/api/types";

function useSecondsSince(timestamp: number | null): number | null {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);
  return timestamp === null ? null : Math.max(0, Math.round((now - timestamp) / 1000));
}

export function DashboardHeader({
  client,
  lastUpdated,
  refreshing,
  onRefresh,
  onLogout,
}: {
  client: Cliente | null;
  lastUpdated: number | null;
  refreshing: boolean;
  onRefresh: () => void;
  onLogout: () => void;
}) {
  const seconds = useSecondsSince(lastUpdated);
  const name = client
    ? [client.nombre, client.apellido_paterno].filter(Boolean).join(" ")
    : null;

  return (
    <header className="sticky top-0 z-20 border-b border-border bg-bg/85 backdrop-blur">
      <div className="mx-auto flex h-14 max-w-7xl items-center justify-between gap-4 px-4 sm:px-6">
        <Brand href="/dashboard" />
        <div className="flex items-center gap-2 sm:gap-3">
          <span className="hidden items-center gap-1.5 text-xs text-muted sm:inline-flex" aria-live="polite">
            <span className="relative flex size-2">
              <span className="absolute inline-flex size-full animate-ping rounded-full bg-verified opacity-60" />
              <span className="relative inline-flex size-2 rounded-full bg-verified" />
            </span>
            Live{seconds !== null ? ` · updated ${seconds}s ago` : ""}
          </span>
          <Button variant="ghost" size="sm" onClick={onRefresh} aria-label="Refresh balances" disabled={refreshing}>
            <RefreshCw className={`size-4 ${refreshing ? "animate-spin" : ""}`} aria-hidden />
          </Button>
          {name ? (
            <div className="hidden text-right md:block">
              <p className="text-sm leading-tight font-medium">{name}</p>
              <p className="text-[11px] leading-tight text-muted">{client?.email}</p>
            </div>
          ) : null}
          {client && client.estatus_kyc !== "VERIFICADO" ? <Badge tone="warning">KYC {client.estatus_kyc}</Badge> : null}
          <Button variant="secondary" size="sm" onClick={onLogout}>
            <LogOut className="size-3.5" aria-hidden />
            <span className="hidden sm:inline">Sign out</span>
          </Button>
        </div>
      </div>
    </header>
  );
}
