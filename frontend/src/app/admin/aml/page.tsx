"use client";

import { Suspense, useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { usePathname, useRouter, useSearchParams } from "next/navigation";

import { AlertDrawer } from "@/components/admin/aml/alert-drawer";
import { AlertFeed } from "@/components/admin/aml/alert-feed";
import { AlertFilterBar } from "@/components/admin/aml/alert-filters";
import { ApprovalsQueue } from "@/components/admin/aml/approvals-queue";
import { FreezeDrawer, FreezesTable } from "@/components/admin/aml/freezes-table";
import { useNow } from "@/components/admin/aml/parts";
import { SummaryBar } from "@/components/admin/aml/summary-bar";
import { Alert, Badge, Button, Card, Skeleton, cx } from "@/components/ui/primitives";
import { useAmlFeed, type ConsoleTab, type FreezeListState } from "@/hooks/use-aml-feed";
import { useSession } from "@/hooks/use-session";
import { describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import type { AlertaAml, CongelamientoAml, ParametrosAml, Uuid } from "@/lib/api/types";
import {
  filtersFromSearch,
  filtersToQuery,
  matchesFilters,
  matchesSearch,
  sortAlerts,
  thresholdsFromParams,
  type AlertFilters,
} from "@/lib/aml";
import { useAuthStore } from "@/store/auth";

const TABS: { id: ConsoleTab; label: string }[] = [
  { id: "alertas", label: "Alerts" },
  { id: "aprobaciones", label: "Approvals" },
  { id: "congelamientos", label: "Frozen clients" },
];

function parseTab(value: string | null): ConsoleTab {
  return TABS.some((t) => t.id === value) ? (value as ConsoleTab) : "alertas";
}

/** How long a page-level confirmation stays on screen. */
const NOTICE_MS = 8000;

function TabList({ tab, onSelect, badges }: { tab: ConsoleTab; onSelect: (tab: ConsoleTab) => void; badges: Partial<Record<ConsoleTab, number>> }) {
  const refs = useRef<Record<string, HTMLButtonElement | null>>({});
  // Arrow keys move between tabs (WAI-ARIA tabs pattern, automatic activation).
  const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number) => {
    const step = event.key === "ArrowRight" ? 1 : event.key === "ArrowLeft" ? -1 : 0;
    if (step === 0) return;
    event.preventDefault();
    const next = TABS[(index + step + TABS.length) % TABS.length]!;
    onSelect(next.id);
    refs.current[next.id]?.focus();
  };
  return (
    <div role="tablist" aria-label="Console sections" className="flex gap-1 overflow-x-auto border-b border-border px-3 sm:px-4">
      {TABS.map((t, i) => {
        const selected = t.id === tab;
        const badge = badges[t.id];
        return (
          <button
            key={t.id}
            ref={(el) => {
              refs.current[t.id] = el;
            }}
            type="button"
            role="tab"
            id={`aml-tab-${t.id}`}
            aria-selected={selected}
            aria-controls={`aml-panel-${t.id}`}
            tabIndex={selected ? 0 : -1}
            onClick={() => onSelect(t.id)}
            onKeyDown={(e) => onKeyDown(e, i)}
            className={cx(
              "-mb-px inline-flex h-11 items-center gap-2 border-b-2 px-3 text-sm font-medium whitespace-nowrap transition",
              "focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none focus-visible:ring-inset",
              selected ? "border-accent text-ink" : "border-transparent text-ink-2 hover:text-ink",
            )}
          >
            {t.label}
            {badge ? (
              <span className="rounded-full bg-accent px-1.5 py-0.5 text-[10px] leading-none font-semibold text-accent-ink tabular-nums">
                {badge}
                <span className="sr-only"> pending</span>
              </span>
            ) : null}
          </button>
        );
      })}
    </div>
  );
}

function TabPanel({ id, tab, children }: { id: ConsoleTab; tab: ConsoleTab; children: ReactNode }) {
  if (id !== tab) return null;
  return (
    <div role="tabpanel" id={`aml-panel-${id}`} aria-labelledby={`aml-tab-${id}`}>
      {children}
    </div>
  );
}

/**
 * The compliance console (/admin/aml): summary bar, then the Alerts,
 * Approvals and Frozen clients tabs. The tab, the feed filters and the open
 * case file live in the URL (`?tab=&estado=&nivel=&regla=&asignada=&alerta=`)
 * so any view is deep-linkable. Auditors see everything without the actions.
 */
function AmlConsole() {
  const router = useRouter();
  const pathname = usePathname();
  const params = useSearchParams();
  const { token, role } = useSession();
  const me: Uuid = useAuthStore((s) => s.clientId) ?? "";
  const now = useNow();

  const tab = parseTab(params.get("tab"));
  const alertId = params.get("alerta");
  const estado = params.get("estado");
  const nivel = params.get("nivel");
  const regla = params.get("regla");
  const asignada = params.get("asignada");
  const filters = useMemo(() => {
    const p = new URLSearchParams();
    if (estado !== null) p.set("estado", estado);
    if (nivel !== null) p.set("nivel", nivel);
    if (regla !== null) p.set("regla", regla);
    if (asignada !== null) p.set("asignada", asignada);
    return filtersFromSearch(p);
  }, [estado, nivel, regla, asignada]);

  const [search, setSearch] = useState("");
  const [freezeState, setFreezeState] = useState<FreezeListState>("ACTIVO");
  const [freezeOpen, setFreezeOpen] = useState<CongelamientoAml | null>(null);
  const [freezeNotice, setFreezeNotice] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [parametros, setParametros] = useState<ParametrosAml | null>(null);

  const feed = useAmlFeed({ token, filters, tab, freezeState });
  const { refresh, upsert } = feed;

  // Thresholds for the score meters and the UIF notice window. GET /aml/reglas
  // may be unavailable (it is optional); the seeded defaults are used then.
  useEffect(() => {
    if (!token) return;
    const controller = new AbortController();
    api.amlRules(token, {}, controller.signal).then(
      (r) => setParametros(r.parametros),
      () => undefined,
    );
    return () => controller.abort();
  }, [token]);

  useEffect(() => {
    if (!notice) return;
    const id = setTimeout(() => setNotice(null), NOTICE_MS);
    return () => clearTimeout(id);
  }, [notice]);

  const updateQuery = useCallback(
    (changes: Record<string, string | null | undefined>) => {
      const next = new URLSearchParams(params.toString());
      for (const [key, value] of Object.entries(changes)) {
        if (value === null || value === undefined || value === "") next.delete(key);
        else next.set(key, value);
      }
      const qs = next.toString();
      router.replace(qs ? `${pathname}?${qs}` : pathname, { scroll: false });
    },
    [params, pathname, router],
  );

  const setFilters = (f: AlertFilters) => updateQuery({ ...filtersToQuery(f) });
  const selectTab = (t: ConsoleTab) => updateQuery({ tab: t === "alertas" ? null : t });
  const openAlert = (id: Uuid) => updateQuery({ alerta: id });
  const closeAlert = () => updateQuery({ alerta: null });

  const afterPost = useCallback(
    (alerts: AlertaAml[]) => {
      if (alerts.length > 0) upsert(alerts);
      refresh();
    },
    [refresh, upsert],
  );

  const thresholds = thresholdsFromParams(parametros);
  const visible = useMemo(
    () => sortAlerts(feed.rows.filter((a) => matchesFilters(a, filters, me) && matchesSearch(a, search))),
    [feed.rows, filters, me, search],
  );
  const pendingCount = feed.pending.filter((a) => matchesFilters(a, filters, me)).length;
  // Four-eyes checks need every loaded alert, including new ones still behind the pill.
  const knownAlerts = useMemo(() => [...feed.rows, ...feed.pending], [feed.rows, feed.pending]);
  const rules = useMemo(() => {
    const byCode = new Map<string, string>();
    for (const a of [...feed.rows, ...feed.pending]) if (!byCode.has(a.regla.codigo)) byCode.set(a.regla.codigo, a.regla.nombre);
    return [...byCode.entries()].map(([codigo, nombre]) => ({ codigo, nombre })).sort((x, y) => x.codigo.localeCompare(y.codigo));
  }, [feed.rows, feed.pending]);
  const narrowed =
    search.trim() !== "" || filters.niveles.length > 0 || filters.regla !== null || filters.asignada !== null ||
    filters.estados.length !== 2 || !filters.estados.includes("PENDIENTE") || !filters.estados.includes("EN_REVISION");

  // The open restriction follows the polled list; the last copy stays while it is out of the list.
  const shownFreeze = freezeOpen ? (feed.freezes?.find((f) => f.id === freezeOpen.id) ?? freezeOpen) : null;

  if (!token) return null;
  const auditor = role === "AUDITOR";

  return (
    <main className="mx-auto max-w-[96rem] space-y-5 px-4 py-6 sm:px-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="min-w-0">
          <h1 className="text-lg font-semibold tracking-tight text-ink">AML/PLD monitoring</h1>
          <p className="text-xs text-muted">Alerts, restrictions and four-eyes approvals. Times are Mexico City.</p>
        </div>
        {auditor ? <Badge tone="info">Read-only (auditor)</Badge> : null}
      </div>

      <SummaryBar
        summary={feed.summary}
        lastUpdated={feed.lastUpdated}
        refreshing={feed.loading}
        stale={Boolean(feed.error)}
        daysNotice={parametros?.dias_aviso_plazo ?? 7}
        onRefresh={refresh}
      />

      {notice ? <Alert tone="success">{notice}</Alert> : null}
      {feed.error && feed.rows.length === 0 && !feed.loading ? (
        <Alert tone="error" title="Could not load the console">
          {describeError(feed.error)}{" "}
          <Button variant="ghost" size="sm" onClick={refresh}>
            Retry
          </Button>
        </Alert>
      ) : null}
      {feed.error && feed.rows.length > 0 ? (
        <Alert tone="warning">Showing the last data received. {describeError(feed.error)}</Alert>
      ) : null}

      <Card className="overflow-hidden">
        <TabList tab={tab} onSelect={selectTab} badges={{ aprobaciones: feed.summary?.solicitudes_pendientes }} />

        <TabPanel id="alertas" tab={tab}>
          <AlertFilterBar filters={filters} onChange={setFilters} rules={rules} search={search} onSearch={setSearch} />
          <AlertFeed
            rows={visible}
            pendingCount={pendingCount}
            onMergePending={feed.mergePending}
            highlighted={feed.highlighted}
            thresholds={thresholds}
            me={me}
            now={now}
            selectedId={alertId}
            onOpen={openAlert}
            loading={feed.loading}
            hasMore={feed.nextBeforeFolio !== null}
            loadingMore={feed.loadingMore}
            onLoadMore={feed.loadMore}
            filtered={narrowed}
          />
        </TabPanel>

        <TabPanel id="aprobaciones" tab={tab}>
          <ApprovalsQueue
            requests={feed.requests}
            freezes={feed.freezes}
            alerts={knownAlerts}
            role={role}
            me={me}
            token={token}
            onOpenAlert={openAlert}
            onChanged={(message, alerts) => {
              setNotice(message);
              afterPost(alerts);
            }}
          />
        </TabPanel>

        <TabPanel id="congelamientos" tab={tab}>
          <FreezesTable
            freezes={feed.freezes}
            state={freezeState}
            onStateChange={setFreezeState}
            onOpen={(id) => {
              setFreezeNotice(null);
              setFreezeOpen(feed.freezes?.find((f) => f.id === id) ?? null);
            }}
          />
        </TabPanel>
      </Card>

      {shownFreeze ? (
        <FreezeDrawer
          congelamiento={shownFreeze}
          role={role}
          me={me}
          token={token}
          notice={freezeNotice}
          onClose={() => setFreezeOpen(null)}
          onOpenAlert={(id) => {
            setFreezeOpen(null);
            openAlert(id);
          }}
          onChanged={(message) => {
            setFreezeNotice(message);
            afterPost([]);
          }}
        />
      ) : null}

      {alertId ? (
        <AlertDrawer
          alertId={alertId}
          token={token}
          role={role}
          me={me}
          thresholds={thresholds}
          onClose={closeAlert}
          onOpenAlert={openAlert}
          onChanged={afterPost}
        />
      ) : null}
    </main>
  );
}

export default function AmlConsolePage() {
  return (
    <Suspense
      fallback={
        <main className="mx-auto max-w-[96rem] space-y-5 px-4 py-6 sm:px-6" aria-busy="true">
          <Skeleton className="h-6 w-48" />
          <Skeleton className="h-20 w-full" />
          <Skeleton className="h-96 w-full" />
        </main>
      }
    >
      <AmlConsole />
    </Suspense>
  );
}
