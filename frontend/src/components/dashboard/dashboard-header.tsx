"use client";

import { useEffect, useState } from "react";
import { LogOut, RefreshCw } from "lucide-react";

import { Brand } from "@/components/brand";
import { KycHeaderBadge } from "@/components/dashboard/kyc-status";
import { Button } from "@/components/ui/primitives";
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
  title,
  client,
  lastUpdated,
  refreshing,
  onRefresh,
  onLogout,
}: {
  /** The section on screen (the page's heading). */
  title: string;
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
    <header className="sticky top-0 z-20 border-b border-border bg-bg/85 backdrop-blur-md">
      <div className="mx-auto flex h-16 max-w-7xl items-center justify-between gap-4 px-4 sm:px-6">
        <div className="flex min-w-0 items-center gap-3">
          {/* From lg up the sidebar carries the brand: the header names the section. */}
          <span className="shrink-0 lg:hidden">
            <Brand href="/dashboard" compact />
          </span>
          <h1 className="truncate text-lg font-semibold tracking-tight text-ink">{title}</h1>
        </div>
        <div className="flex items-center gap-2 sm:gap-3">
          <span className="hidden items-center gap-1.5 text-xs text-muted sm:inline-flex" aria-live="polite">
            <span className="relative flex size-2">
              <span className="absolute inline-flex size-full rounded-full bg-synthetic-yellow opacity-60 motion-safe:animate-ping" />
              <span className="relative inline-flex size-2 rounded-full bg-synthetic-yellow" />
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
          {client ? <KycHeaderBadge status={client.estatus_kyc} /> : null}
          {/* Named for screen readers on phones too, where only the icon shows. */}
          <Button variant="secondary" size="sm" onClick={onLogout} aria-label="Sign out">
            <LogOut className="size-3.5" aria-hidden />
            <span className="hidden sm:inline">Sign out</span>
          </Button>
        </div>
      </div>
    </header>
  );
}
