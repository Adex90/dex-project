"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useRouter } from "next/navigation";

import { ApiError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import type { AlertaAml, CongelamientoAml, ResumenAml, SolicitudAml } from "@/lib/api/types";
import {
  FEED_PAGE_SIZE,
  POLL_OVERLAP_MS,
  filtersToQuery,
  mergeFeed,
  nextPollCursor,
  nextPollDelay,
  retriesFeedLoad,
  type AlertFilters,
} from "@/lib/aml";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { useAuthStore } from "@/store/auth";

export type ConsoleTab = "alertas" | "aprobaciones" | "congelamientos";
export type FreezeListState = "ACTIVO" | "LEVANTADO" | "TODOS";

/** How long a row updated by a poll stays highlighted. */
const HIGHLIGHT_MS = 2500;
/** Follow-up polls allowed in one cycle when every page comes back full. */
const MAX_FOLLOW_UPS = 10;

export interface AmlFeed {
  /** Every alert loaded so far, kept current by polling (filter with `matchesFilters`, order with `sortAlerts`). */
  rows: AlertaAml[];
  /** New alerts that arrived by polling: shown behind the "N new alerts" pill, never inserted under the cursor. */
  pending: AlertaAml[];
  /** Rows changed in place by the last polls (briefly highlighted). */
  highlighted: ReadonlySet<string>;
  summary: ResumenAml | null;
  /** Pending four-eyes requests (loaded while the Approvals tab is visible). */
  requests: SolicitudAml[] | null;
  /** Freezes of the selected state (Frozen clients tab), or the ACTIVO ones (Approvals tab, for the SLA). */
  freezes: CongelamientoAml[] | null;
  /** Page-mode cursor of the current filters (null when the last page was not full). */
  nextBeforeFolio: number | null;
  /** Date.now() of the last successful poll. */
  lastUpdated: number | null;
  loading: boolean;
  loadingMore: boolean;
  /** Last error of the feed (null after a successful poll). */
  error: unknown;
  /** Polls now (after any successful POST). */
  refresh: () => void;
  /** Merges the pending new alerts into the feed. */
  mergePending: () => void;
  /** Older alerts of the current filters. */
  loadMore: () => void;
  /** Puts alerts returned by a POST into the feed at once (the next poll confirms them). */
  upsert: (alerts: AlertaAml[]) => void;
}

interface Engine {
  refresh: () => void;
  reload: () => void;
  loadMore: () => void;
}

function isAbort(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

/**
 * Data of the compliance console, polled every POLL_INTERVAL_MS (§5.4):
 *
 * - the first page of the current filters (page mode), then every 10 s the
 *   alerts updated since the previous response's `servidor_en` minus 60 s (poll
 *   mode, all states), merged by id keeping the newest copy; a full poll page
 *   triggers an immediate follow-up from its last row;
 * - the summary counters, and the requests / freezes while their tab is visible;
 * - pauses while the tab is hidden, backs off on errors (10 s doubling to 60 s)
 *   and signs out on a 401; one AbortController per request, aborted on unmount
 *   and when the filters change.
 */
export function useAmlFeed({
  token,
  filters,
  tab,
  freezeState,
}: {
  token: string | null;
  filters: AlertFilters;
  tab: ConsoleTab;
  freezeState: FreezeListState;
}): AmlFeed {
  const router = useRouter();
  const [rows, setRows] = useState<AlertaAml[]>([]);
  const [pending, setPending] = useState<AlertaAml[]>([]);
  const [highlighted, setHighlighted] = useState<ReadonlySet<string>>(() => new Set());
  const [summary, setSummary] = useState<ResumenAml | null>(null);
  const [requests, setRequests] = useState<SolicitudAml[] | null>(null);
  const [freezes, setFreezes] = useState<CongelamientoAml[] | null>(null);
  const [nextBeforeFolio, setNextBeforeFolio] = useState<number | null>(null);
  const [lastUpdated, setLastUpdated] = useState<number | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<unknown>(null);

  // The engine reads the latest props and data through refs: its timers outlive renders.
  const rowsRef = useRef<AlertaAml[]>([]);
  const pendingRef = useRef<AlertaAml[]>([]);
  const filtersRef = useRef(filters);
  const tabRef = useRef(tab);
  const freezeStateRef = useRef(freezeState);
  const nextBeforeRef = useRef<number | null>(null);
  const engineRef = useRef<Engine | null>(null);
  const requestedKeyRef = useRef<string | null>(null);
  const lastTabKeyRef = useRef<string | null>(null);

  const filtersKey = JSON.stringify(filtersToQuery(filters));
  const tabKey = `${tab}:${freezeState}`;

  useEffect(() => {
    filtersRef.current = filters;
    tabRef.current = tab;
    freezeStateRef.current = freezeState;
  }, [filters, tab, freezeState]);

  const publish = useCallback(() => {
    setRows(rowsRef.current);
    setPending(pendingRef.current);
  }, []);

  const flash = useCallback((ids: string[]) => {
    if (ids.length === 0) return;
    setHighlighted((prev) => new Set([...prev, ...ids]));
    setTimeout(() => {
      setHighlighted((prev) => {
        const next = new Set(prev);
        for (const id of ids) next.delete(id);
        return next;
      });
    }, HIGHLIGHT_MS);
  }, []);

  useEffect(() => {
    if (!token) return;
    const auth: string = token;
    let alive = true;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let running = false;
    let again = false;
    let paused = false;
    let failures = 0;
    let inReviewLoaded = false;
    let cursor: string | null = null; // servidor_en of the last complete poll
    let pageKey: string | null = null; // filters whose first page is loaded
    // Filters whose first page the API rejected as invalid (400, 404, 422): it would fail the
    // same way on every cycle, so it is not retried until the filters change or Refresh is pressed.
    let rejectedKey: string | null = null;
    const controllers = new Set<AbortController>();
    const currentKey = () => JSON.stringify(filtersToQuery(filtersRef.current));

    const fetchWith = async <T>(call: (signal: AbortSignal) => Promise<T>): Promise<T> => {
      const controller = new AbortController();
      controllers.add(controller);
      try {
        return await call(controller.signal);
      } finally {
        controllers.delete(controller);
      }
    };
    const abortAll = () => {
      for (const controller of controllers) controller.abort();
      controllers.clear();
    };
    const clearTimer = () => {
      if (timer) clearTimeout(timer);
      timer = null;
    };
    const schedule = (ms: number) => {
      clearTimer();
      timer = setTimeout(() => void tick(), ms);
    };
    /** Records a failure; a 401 ends the session. Returns false when the error was an abort. */
    const fail = (e: unknown): boolean => {
      if (isAbort(e)) return false;
      if (e instanceof ApiError && e.isUnauthorized) {
        alive = false;
        abortAll();
        clearTimer();
        useAuthStore.getState().expire();
        router.replace(EXPIRED_LOGIN_PATH);
        return true;
      }
      setError(e);
      return true;
    };

    /** New alerts go behind the pill; known ones are updated in place. */
    const applyPoll = (incoming: AlertaAml[]) => {
      const known = new Set(rowsRef.current.map((a) => a.id));
      const merged = mergeFeed(
        rowsRef.current,
        incoming.filter((a) => known.has(a.id)),
      );
      rowsRef.current = merged.alertas;
      pendingRef.current = mergeFeed(
        pendingRef.current,
        incoming.filter((a) => !known.has(a.id)),
      ).alertas;
      flash(merged.actualizadas);
      publish();
    };

    const pollAlerts = async (from: string) => {
      let since = nextPollCursor(from);
      let complete = false;
      let servidorEn = from;
      const collected: AlertaAml[] = [];
      for (let i = 0; i <= MAX_FOLLOW_UPS; i++) {
        const page = await fetchWith((signal) => api.amlAlerts(auth, { actualizadas_desde: since, limite: FEED_PAGE_SIZE }, signal));
        collected.push(...page.alertas);
        servidorEn = page.servidor_en;
        const last = page.alertas.at(-1);
        if (page.alertas.length < FEED_PAGE_SIZE || !last) {
          complete = true;
          break;
        }
        if (last.actualizado_en === since) break; // a whole page at one instant: give up for this cycle
        since = last.actualizado_en;
      }
      if (!alive) return;
      applyPoll(collected);
      // An incomplete chain resumes from its last row (the next cursor subtracts the overlap again).
      cursor = complete ? servidorEn : new Date(Date.parse(since) + POLL_OVERLAP_MS).toISOString();
    };

    async function tick(options: { skipAlerts?: boolean } = {}) {
      if (!alive) return;
      if (running) {
        again = true;
        return;
      }
      if (document.hidden) {
        paused = true;
        clearTimer();
        return;
      }
      clearTimer();
      running = true;
      const currentTab = tabRef.current;
      const jobs: Promise<unknown>[] = [fetchWith((signal) => api.amlSummary(auth, signal)).then((r) => alive && setSummary(r))];
      if (!options.skipAlerts) {
        if (cursor) jobs.push(pollAlerts(cursor));
        // The first page of these filters failed earlier: retry it with the poll.
        if (pageKey !== currentKey() && rejectedKey !== currentKey()) jobs.push(loadFirstPage());
      }
      if (currentTab === "aprobaciones") {
        jobs.push(fetchWith((signal) => api.amlRequests(auth, { limite: 200 }, signal)).then((r) => alive && setRequests(r.solicitudes)));
        if (!inReviewLoaded) {
          // Four-eyes needs every false-positive proposal (they live on EN_REVISION alerts).
          jobs.push(
            fetchWith((signal) => api.amlAlerts(auth, { estado: "EN_REVISION", limite: FEED_PAGE_SIZE }, signal)).then((r) => {
              if (!alive) return;
              inReviewLoaded = true;
              rowsRef.current = mergeFeed(rowsRef.current, r.alertas).alertas;
              publish();
            }),
          );
        }
      }
      if (currentTab !== "alertas") {
        const estado = currentTab === "congelamientos" ? freezeStateRef.current : "ACTIVO";
        jobs.push(fetchWith((signal) => api.amlFreezes(auth, { estado, limite: 200 }, signal)).then((r) => alive && setFreezes(r.congelamientos)));
      }
      const results = await Promise.allSettled(jobs);
      const failure = results.find((r): r is PromiseRejectedResult => r.status === "rejected");
      running = false;
      if (!alive) return;
      if (!failure) {
        failures = 0;
        setError(null);
        setLastUpdated(Date.now());
      } else if (fail(failure.reason)) {
        failures += 1;
      }
      if (!alive) return;
      if (again) {
        again = false;
        void tick();
      } else {
        schedule(nextPollDelay(failures));
      }
    }

    /** Page mode: the newest alerts of the current filters. */
    async function loadFirstPage() {
      const key = currentKey();
      let page;
      try {
        page = await fetchWith((signal) =>
          api.amlAlerts(auth, { ...filtersToQuery(filtersRef.current), limite: FEED_PAGE_SIZE }, signal),
        );
      } catch (e) {
        if (e instanceof ApiError && !retriesFeedLoad(e.status)) rejectedKey = key;
        throw e;
      }
      if (!alive || currentKey() !== key) return;
      // Whatever waited behind the pill is shown with the new page.
      rowsRef.current = mergeFeed(mergeFeed(rowsRef.current, pendingRef.current).alertas, page.alertas).alertas;
      pendingRef.current = [];
      cursor ??= page.servidor_en;
      pageKey = key;
      nextBeforeRef.current = page.siguiente_antes_de_folio;
      setNextBeforeFolio(page.siguiente_antes_de_folio);
      setLoading(false);
      publish();
    }

    /** Filters changed (or first load): abort what is in flight, load the first page, then the rest. */
    async function reload() {
      requestedKeyRef.current = currentKey();
      abortAll();
      setLoading(true);
      try {
        await loadFirstPage();
        setError(null);
      } catch (e) {
        if (!fail(e)) return; // aborted by a newer reload, which takes over
        setLoading(false);
      }
      if (alive) void tick({ skipAlerts: pageKey === currentKey() });
    }

    async function loadMore() {
      const before = nextBeforeRef.current;
      if (before === null) return;
      setLoadingMore(true);
      try {
        const page = await fetchWith((signal) =>
          api.amlAlerts(auth, { ...filtersToQuery(filtersRef.current), antes_de_folio: before, limite: FEED_PAGE_SIZE }, signal),
        );
        if (!alive) return;
        rowsRef.current = mergeFeed(rowsRef.current, page.alertas).alertas;
        nextBeforeRef.current = page.siguiente_antes_de_folio;
        setNextBeforeFolio(page.siguiente_antes_de_folio);
        publish();
      } catch (e) {
        fail(e);
      } finally {
        if (alive) setLoadingMore(false);
      }
    }

    const onVisibility = () => {
      if (!document.hidden && paused) {
        paused = false;
        void tick();
      }
    };

    engineRef.current = {
      refresh: () => {
        rejectedKey = null;
        void tick();
      },
      reload: () => void reload(),
      loadMore: () => void loadMore(),
    };
    document.addEventListener("visibilitychange", onVisibility);
    void reload();

    return () => {
      alive = false;
      abortAll();
      clearTimer();
      document.removeEventListener("visibilitychange", onVisibility);
      engineRef.current = null;
      requestedKeyRef.current = null;
    };
  }, [token, router, publish, flash]);

  // Filters changed: abort what is in flight and load the first page again.
  useEffect(() => {
    if (engineRef.current && requestedKeyRef.current !== null && requestedKeyRef.current !== filtersKey) {
      engineRef.current.reload();
    }
  }, [filtersKey]);

  // Tab or freeze filter changed: fetch that tab's lists at once.
  useEffect(() => {
    const changed = lastTabKeyRef.current !== null && lastTabKeyRef.current !== tabKey;
    lastTabKeyRef.current = tabKey;
    if (changed) engineRef.current?.refresh();
  }, [tabKey]);

  const refresh = useCallback(() => engineRef.current?.refresh(), []);
  const loadMore = useCallback(() => engineRef.current?.loadMore(), []);

  const mergePending = useCallback(() => {
    const ids = pendingRef.current.map((a) => a.id);
    rowsRef.current = mergeFeed(rowsRef.current, pendingRef.current).alertas;
    pendingRef.current = [];
    flash(ids);
    publish();
  }, [flash, publish]);

  const upsert = useCallback(
    (alerts: AlertaAml[]) => {
      const merged = mergeFeed(rowsRef.current, alerts);
      rowsRef.current = merged.alertas;
      const added = new Set(merged.nuevas);
      pendingRef.current = pendingRef.current.filter((a) => !added.has(a.id));
      flash(merged.actualizadas);
      publish();
    },
    [flash, publish],
  );

  return {
    rows,
    pending,
    highlighted,
    summary,
    requests,
    freezes,
    nextBeforeFolio,
    lastUpdated,
    loading,
    loadingMore,
    error,
    refresh,
    mergePending,
    loadMore,
    upsert,
  };
}
