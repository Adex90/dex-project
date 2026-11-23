"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { useRouter } from "next/navigation";
import { ArrowLeftRight, ChevronRight, FileText, RefreshCw } from "lucide-react";

import { DocumentViewer } from "@/components/documents/document-viewer";
import { useDashboard } from "@/components/layout/dashboard-shell";
import { Money, truncateHash } from "@/components/ui/data";
import { Alert, Button, Skeleton, cx } from "@/components/ui/primitives";
import { SegmentedControl, type SegmentOption } from "@/components/ui/segmented-control";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import {
  DOCUMENTS_PATH,
  documentDate,
  filterDocuments,
  isDocumentId,
  parseDocument,
  parseDocumentList,
  type InboxFilter,
} from "@/lib/documents";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { useAuthStore } from "@/store/auth";
import type { DocumentList, DocumentSummary, FinancialDocument } from "@/types/documents";

const UNREADABLE_LIST = "Your documents could not be read. Please try again.";
const UNREADABLE_DOCUMENT = "This document could not be read. Please try again.";
const NOT_AVAILABLE = "This document is not available.";

/*
 * Documents (roadmap item 24): the client's inbox of account statements and
 * trade confirmations.
 *
 * - Every executed order has a trade confirmation, and every month with
 *   activity an account statement. The server derives both from the
 *   hash-chained ledgers, so an issued document never changes: its reference
 *   hash proves it. The month in progress is marked "In progress" until it
 *   closes.
 * - A row opens the viewer (components/documents/document-viewer.tsx): the
 *   exact figures, the zero-commission breakdown and the reserve seal, to
 *   print, save as PDF or download. `?id=` opens one directly.
 *
 * Layout: rows stacked on phones; from md, a table of date, document,
 * reference and amount.
 */
export default function DocumentsPage() {
  const router = useRouter();
  const { token } = useDashboard();
  const [list, setList] = useState<DocumentList | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [filter, setFilter] = useState<InboxFilter>("all");
  const [open, setOpen] = useState<DocumentSummary | null>(null);

  const endSession = useCallback(() => {
    useAuthStore.getState().expire();
    router.replace(EXPIRED_LOGIN_PATH);
  }, [router]);

  const load = useCallback(
    async (signal?: AbortSignal) => {
      setLoading(true);
      try {
        const parsed = parseDocumentList(await api.documents(token, signal));
        if (!parsed) throw new Error(UNREADABLE_LIST);
        setList(parsed);
        setLoadError(null);
      } catch (error) {
        if (error instanceof DOMException && error.name === "AbortError") return;
        if (error instanceof ApiError && error.isUnauthorized) {
          endSession();
          return;
        }
        setLoadError(describeError(error));
      } finally {
        setLoading(false);
      }
    },
    [token, endSession],
  );

  useEffect(() => {
    const controller = new AbortController();
    void load(controller.signal);
    return () => controller.abort();
  }, [load]);

  // `?id=` opens that document once the inbox has loaded.
  useEffect(() => {
    if (!list) return;
    const id = new URLSearchParams(window.location.search).get("id");
    if (!id || !isDocumentId(id)) return;
    const found = list.documents.find((d) => d.id === id);
    if (found) setOpen((current) => current ?? found);
  }, [list]);

  const loadDocument = useCallback(
    async (id: string, signal: AbortSignal): Promise<FinancialDocument> => {
      try {
        const parsed = parseDocument(await api.document(token, id, signal));
        if (!parsed || parsed.id !== id) throw new Error(UNREADABLE_DOCUMENT);
        return parsed;
      } catch (error) {
        if (error instanceof ApiError && error.isUnauthorized) endSession();
        if (error instanceof ApiError && error.status === 404) throw new Error(NOT_AVAILABLE);
        throw error;
      }
    },
    [token, endSession],
  );

  const openDocument = useCallback(
    (summary: DocumentSummary) => {
      setOpen(summary);
      router.replace(`${DOCUMENTS_PATH}?id=${encodeURIComponent(summary.id)}`, { scroll: false });
    },
    [router],
  );

  const closeDocument = useCallback(() => {
    setOpen(null);
    router.replace(DOCUMENTS_PATH, { scroll: false });
  }, [router]);

  const counts = useMemo(() => {
    const documents = list?.documents ?? [];
    return {
      all: documents.length,
      account_statement: documents.filter((d) => d.kind === "account_statement").length,
      trade_confirmation: documents.filter((d) => d.kind === "trade_confirmation").length,
    };
  }, [list]);
  const options: SegmentOption<InboxFilter>[] = [
    { value: "all", label: `All · ${counts.all}` },
    { value: "account_statement", label: `Statements · ${counts.account_statement}` },
    { value: "trade_confirmation", label: `Confirmations · ${counts.trade_confirmation}` },
  ];
  const shown = list ? filterDocuments(list.documents, filter) : null;

  return (
    <div className="space-y-6" data-testid="documents-page">
      <div className="space-y-1">
        <p className="text-[11px] font-semibold tracking-[0.24em] text-synthetic-yellow uppercase">Document inbox</p>
        <h2 className="text-2xl font-semibold tracking-tight text-ink">Statements and trade confirmations</h2>
        <p className="max-w-2xl text-sm text-muted">
          A confirmation for every executed order and a statement for every month with activity, issued from the
          hash-chained ledgers. Issued documents never change: each one&apos;s reference hash proves it.
        </p>
      </div>

      {loadError ? (
        <Alert tone="error" title="Could not load your documents">
          {loadError}{" "}
          <Button variant="ghost" size="sm" onClick={() => void load()}>
            Retry
          </Button>
        </Alert>
      ) : null}

      <section className="border border-border bg-surface" aria-labelledby="inbox-title">
        <header className="flex flex-wrap items-center justify-between gap-3 border-b border-border px-4 py-3 sm:px-5">
          <h3 id="inbox-title" className="text-sm font-semibold text-ink">
            Inbox
          </h3>
          <div className="flex w-full min-w-0 items-center gap-2 sm:w-auto">
            <SegmentedControl
              legend="Show"
              options={options}
              value={filter}
              onChange={setFilter}
              className="min-w-0 flex-1 sm:w-[30rem] sm:flex-none"
              testId="documents-filter"
            />
            <Button
              variant="ghost"
              size="sm"
              onClick={() => void load()}
              disabled={loading}
              aria-label="Refresh documents"
              className="h-10 shrink-0"
            >
              <RefreshCw className={cx("size-4", loading && "animate-spin")} aria-hidden />
            </Button>
          </div>
        </header>

        {/* Column headings, from md: the rows below are the table's body. */}
        <div
          aria-hidden
          className="hidden grid-cols-[8.5rem_minmax(0,1fr)_11rem_10rem_1.25rem] gap-4 border-b border-border px-5 py-2 text-[10px] font-semibold tracking-[0.14em] text-muted uppercase md:grid"
        >
          <span>Date</span>
          <span>Document</span>
          <span>Reference</span>
          <span className="text-right">Amount</span>
          <span />
        </div>

        {shown === null ? (
          <div className="space-y-px p-4" aria-busy="true">
            {[0, 1, 2].map((i) => (
              <Skeleton key={i} className="h-14 w-full" />
            ))}
          </div>
        ) : shown.length === 0 ? (
          <p className="px-5 py-10 text-center text-sm text-muted" data-testid="documents-empty">
            {filter === "trade_confirmation"
              ? "No trade confirmations yet. Each executed order issues one."
              : filter === "account_statement"
                ? "No statements yet. Your first one starts with your first deposit."
                : "No documents yet. Your first statement starts with your first deposit, and every executed order issues a trade confirmation."}
          </p>
        ) : (
          <ul className="divide-y divide-border" data-testid="documents-list">
            {shown.map((d) => (
              <li key={d.id}>
                <DocumentRow summary={d} onOpen={() => openDocument(d)} />
              </li>
            ))}
          </ul>
        )}

        {list?.truncated ? (
          <p className="border-t border-border px-5 py-3 text-xs text-muted">
            Showing your latest trade confirmations. Earlier trades appear in the monthly statements.
          </p>
        ) : null}
      </section>

      {open ? <DocumentViewer key={open.id} summary={open} load={loadDocument} onClose={closeDocument} /> : null}
    </div>
  );
}

function DocumentRow({ summary, onOpen }: { summary: DocumentSummary; onOpen: () => void }) {
  const statement = summary.kind === "account_statement";
  const Icon = statement ? FileText : ArrowLeftRight;
  return (
    <button
      type="button"
      onClick={onOpen}
      className={cx(
        "group grid w-full grid-cols-[2.5rem_minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1 px-4 py-3 text-left transition-colors sm:px-5",
        "md:grid-cols-[8.5rem_minmax(0,1fr)_11rem_10rem_1.25rem] md:gap-x-4",
        "hover:bg-surface-2 focus-visible:bg-surface-2 focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none focus-visible:ring-inset",
      )}
      data-testid="document-row"
      data-kind={summary.kind}
      data-id={summary.id}
      aria-label={`Open ${summary.title}, ${summary.folio}`}
    >
      {/* Phones: an icon tile. From md: the date column. */}
      <span
        aria-hidden
        className={cx(
          "grid size-10 place-items-center border md:hidden",
          statement ? "border-synthetic-yellow text-synthetic-yellow" : "border-border-strong text-ink-2",
        )}
      >
        <Icon className="size-4" />
      </span>
      <span className="hidden text-sm text-ink-2 tabular-nums md:block">{documentDate(summary.date)}</span>

      <span className="min-w-0">
        <span className="flex min-w-0 items-center gap-2">
          <span className="truncate text-sm font-semibold text-ink">{summary.title}</span>
          {summary.provisional ? (
            <span className="shrink-0 border border-border-strong px-1.5 py-px text-[10px] font-bold tracking-[0.12em] text-ink-2 uppercase">
              In progress
            </span>
          ) : null}
        </span>
        <span className="block truncate font-mono text-[11px] text-muted">
          {summary.folio}
          <span className="md:hidden">
            {" "}
            · {documentDate(summary.date)} · {truncateHash(summary.reference, 6, 4)}
          </span>
        </span>
      </span>

      <span className="hidden truncate font-mono text-xs text-ink-2 md:block" title={summary.reference}>
        {truncateHash(summary.reference, 8, 6)}
      </span>

      <span className="text-right">
        <Money value={summary.amount} className="text-sm font-semibold text-ink" />
        <span className="block text-[10px] text-muted">{statement ? "Closing balance" : "Order amount"}</span>
      </span>

      <ChevronRight
        className="hidden size-4 text-muted transition-colors group-hover:text-synthetic-yellow md:block"
        aria-hidden
      />
    </button>
  );
}
