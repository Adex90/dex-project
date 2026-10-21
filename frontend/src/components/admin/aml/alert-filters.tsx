"use client";

import { Search, X } from "lucide-react";

import { cx } from "@/components/ui/primitives";
import type { EstadoAlertaAml, NivelRiesgoAml } from "@/lib/api/types";
import { ALERT_STATES, LEVELS, LEVEL_LABEL, STATE_LABEL, type AlertFilters } from "@/lib/aml";

function Chip({ pressed, onClick, children }: { pressed: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      type="button"
      aria-pressed={pressed}
      onClick={onClick}
      className={cx(
        "h-7 rounded-full border px-2.5 text-xs font-medium transition",
        "focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none",
        pressed ? "border-accent bg-accent-soft text-accent" : "border-border bg-surface text-ink-2 hover:bg-surface-2",
      )}
    >
      {children}
    </button>
  );
}

function toggle<T>(list: T[], value: T): T[] {
  return list.includes(value) ? list.filter((v) => v !== value) : [...list, value];
}

const selectClass =
  "h-8 rounded-lg border border-border-strong bg-surface px-2 text-xs text-ink focus:border-accent focus:ring-2 focus:ring-accent/25 focus:outline-none";

/**
 * Feed filters. State, level, rule and assignee live in the URL (deep links);
 * the search box filters the loaded rows by folio or client name/id.
 */
export function AlertFilterBar({
  filters,
  onChange,
  rules,
  search,
  onSearch,
}: {
  filters: AlertFilters;
  onChange: (next: AlertFilters) => void;
  /** Rule codes present in the loaded rows. */
  rules: { codigo: string; nombre: string }[];
  search: string;
  onSearch: (text: string) => void;
}) {
  const noState = filters.estados.length === 0;
  return (
    <div className="flex flex-col gap-3 border-b border-border px-4 py-3 sm:px-5">
      <div className="flex flex-wrap items-center gap-x-5 gap-y-2">
        <div role="group" aria-label="Alert state" className="flex flex-wrap items-center gap-1.5">
          <span className="mr-1 text-[11px] font-semibold tracking-wide text-muted uppercase">State</span>
          {ALERT_STATES.map((s: EstadoAlertaAml) => (
            <Chip key={s} pressed={filters.estados.includes(s)} onClick={() => onChange({ ...filters, estados: toggle(filters.estados, s) })}>
              {STATE_LABEL[s]}
            </Chip>
          ))}
          {noState ? <span className="text-[11px] text-muted">(all states)</span> : null}
        </div>
        <div role="group" aria-label="Risk level" className="flex flex-wrap items-center gap-1.5">
          <span className="mr-1 text-[11px] font-semibold tracking-wide text-muted uppercase">Level</span>
          {LEVELS.map((l: NivelRiesgoAml) => (
            <Chip key={l} pressed={filters.niveles.includes(l)} onClick={() => onChange({ ...filters, niveles: toggle(filters.niveles, l) })}>
              {LEVEL_LABEL[l]}
            </Chip>
          ))}
        </div>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <label className="sr-only" htmlFor="aml-rule">
          Rule
        </label>
        <select
          id="aml-rule"
          className={cx(selectClass, "max-w-64")}
          value={filters.regla ?? ""}
          onChange={(e) => onChange({ ...filters, regla: e.target.value || null })}
        >
          <option value="">All rules</option>
          {filters.regla && !rules.some((r) => r.codigo === filters.regla) ? <option value={filters.regla}>{filters.regla}</option> : null}
          {rules.map((r) => (
            <option key={r.codigo} value={r.codigo}>
              {r.codigo} · {r.nombre}
            </option>
          ))}
        </select>
        <div role="group" aria-label="Assignee" className="inline-flex overflow-hidden rounded-lg border border-border-strong">
          {(
            [
              [null, "Anyone"],
              ["yo", "Assigned to me"],
              ["nadie", "Unassigned"],
            ] as const
          ).map(([value, label]) => (
            <button
              key={label}
              type="button"
              aria-pressed={filters.asignada === value}
              onClick={() => onChange({ ...filters, asignada: value })}
              className={cx(
                "h-8 border-r border-border-strong px-2.5 text-xs font-medium transition last:border-r-0",
                "focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none focus-visible:ring-inset",
                filters.asignada === value ? "bg-accent-soft text-accent" : "bg-surface text-ink-2 hover:bg-surface-2",
              )}
            >
              {label}
            </button>
          ))}
        </div>
        <div className="relative ml-auto w-full sm:w-64">
          <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted" aria-hidden />
          <input
            type="search"
            value={search}
            onChange={(e) => onSearch(e.target.value)}
            placeholder="Folio, client name or id"
            aria-label="Search loaded alerts by folio, client name or id"
            className="h-8 w-full rounded-lg border border-border-strong bg-surface pr-7 pl-8 text-xs text-ink placeholder:text-muted/70 focus:border-accent focus:ring-2 focus:ring-accent/25 focus:outline-none"
          />
          {search ? (
            <button
              type="button"
              onClick={() => onSearch("")}
              className="absolute top-1/2 right-1.5 -translate-y-1/2 rounded p-0.5 text-muted hover:text-ink"
              aria-label="Clear search"
            >
              <X className="size-3.5" aria-hidden />
            </button>
          ) : null}
        </div>
      </div>
    </div>
  );
}
