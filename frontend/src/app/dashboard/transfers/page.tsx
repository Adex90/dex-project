"use client";

import { useCallback, useEffect, useState } from "react";

import { useDashboard } from "@/components/layout/dashboard-shell";
import { ClarificationList } from "@/components/transfers/clarification-list";
import { TransferInspector } from "@/components/transfers/transfer-inspector";
import { TransferList } from "@/components/transfers/transfer-list";
import { SegmentedControl, type SegmentOption } from "@/components/ui/segmented-control";
import { useSignOutOn401 } from "@/hooks/use-sign-out-on-401";
import { api } from "@/lib/api/endpoints";
import {
  activeClarifications,
  parseClarification,
  readTransfersQuery,
  transfersPath,
  type TransfersTab,
} from "@/lib/transfers";
import { useTransfersStore } from "@/store/transfers";
import { useVaultStore } from "@/store/vault";
import type { SpeiClarification, SpeiClarificationRequest } from "@/types/transfers";

/*
 * Transfers: /dashboard/transfers, in the Account group. Every SPEI transfer
 * of the client, in and out (roadmap item 17):
 *
 * - Transfers: the list (components/transfers/transfer-list.tsx); a row opens
 *   the inspector (transfer-inspector.tsx): the settlement timeline, the
 *   tracking key with one-tap copy, the Banxico CEP and "Report a delay" /
 *   "Open a clarification".
 * - Clarifications: the client's clarifications, their status, history and
 *   the note that closed them (clarification-list.tsx).
 *
 * The address keeps the tab and the open transfer (`?tab=clarifications`,
 * `?transfer=<id>`), so a link from the Account ledger, a withdrawal receipt
 * or a reload opens the same; `transfer` may also be the id of a ledger
 * movement the transfer made. Both lists reload whenever the vault moves (a
 * deposit arrives, a withdrawal settles or comes back, a clarification
 * changes: the database announces each one).
 */

/** Vault moves this close together (one action's commits) reload once. */
const RELOAD_MS = 150;

const UNREADABLE_CLARIFICATION =
  "Your clarification was sent, but its answer could not be read. Check Clarifications before sending it again: sending the same one again cannot open two.";

export default function TransfersPage() {
  const { token } = useDashboard();
  const signOutOn401 = useSignOutOn401();
  // Null until the address is read: no flash of the other tab.
  const [tab, setTab] = useState<TransfersTab | null>(null);
  // From the inspector to the clarifications: their title takes the focus the closed dialog had.
  const [focusClarifications, setFocusClarifications] = useState(false);
  const inspected = useTransfersStore((s) => s.inspected);
  const clarifications = useTransfersStore((s) => s.clarifications);
  const vaultMoved = useVaultStore((s) => s.updatedAt);
  const active = activeClarifications(clarifications);

  const fetchPage = useCallback(
    (cursor: string | null, signal: AbortSignal) => api.speiTransfers(token, cursor, signal).catch(signOutOn401),
    [token, signOutOn401],
  );
  const fetchOne = useCallback(
    (id: string, signal: AbortSignal) => api.speiTransfer(token, id, signal).catch(signOutOn401),
    [token, signOutOn401],
  );
  const fetchClarifications = useCallback(
    (signal: AbortSignal) => api.speiClarifications(token, signal).catch(signOutOn401),
    [token, signOutOn401],
  );

  // The address, once mounted: the tab, and the transfer to open.
  useEffect(() => {
    const query = readTransfersQuery(window.location.search);
    setTab(query.tab);
    if (query.transfer) void useTransfersStore.getState().inspect(fetchOne, query.transfer);
    // Leaving the page closes the inspector: coming back opens what the address says.
    return () => useTransfersStore.getState().closeInspector();
  }, [fetchOne]);

  // On mount, and whenever the vault moves.
  useEffect(() => {
    const timer = setTimeout(() => {
      const store = useTransfersStore.getState();
      void store.load(fetchPage);
      void store.loadClarifications(fetchClarifications);
      void store.refreshInspected(fetchOne);
    }, RELOAD_MS);
    return () => clearTimeout(timer);
  }, [fetchPage, fetchClarifications, fetchOne, vaultMoved]);

  const show = (next: TransfersTab, transfer: string | null) =>
    window.history.replaceState(window.history.state, "", transfersPath({ tab: next, transfer }));

  const choose = (next: TransfersTab) => {
    setTab(next);
    setFocusClarifications(false);
    show(next, useTransfersStore.getState().inspected?.key ?? null);
  };

  const open = (key: string) => {
    void useTransfersStore.getState().inspect(fetchOne, key);
    show(tab ?? "transfers", key);
  };

  const close = useCallback(() => {
    useTransfersStore.getState().closeInspector();
    window.history.replaceState(window.history.state, "", transfersPath({ tab: tab ?? "transfers" }));
  }, [tab]);

  const retryInspect = () => {
    const key = useTransfersStore.getState().inspected?.key;
    if (key) void useTransfersStore.getState().inspect(fetchOne, key);
  };

  const openClarification = useCallback(
    async (body: SpeiClarificationRequest): Promise<SpeiClarification> => {
      const clarification = parseClarification(await api.openSpeiClarification(token, body).catch(signOutOn401));
      if (!clarification) throw new Error(UNREADABLE_CLARIFICATION);
      useTransfersStore.getState().clarificationOpened(clarification);
      return clarification;
    },
    [token, signOutOn401],
  );

  const showClarifications = () => {
    useTransfersStore.getState().closeInspector();
    setTab("clarifications");
    setFocusClarifications(true);
    show("clarifications", null);
  };

  const tabs: readonly SegmentOption<TransfersTab>[] = [
    { value: "transfers", label: "Transfers" },
    { value: "clarifications", label: active > 0 ? `Clarifications · ${active}` : "Clarifications" },
  ];

  return (
    <div className="space-y-4" data-testid="transfers-page">
      <header className="flex flex-wrap items-end justify-between gap-4">
        <div className="min-w-0">
          <h2 className="text-2xl font-semibold tracking-tight text-ink">Transfers</h2>
          <p className="mt-1 max-w-2xl text-sm text-muted">
            Every SPEI transfer in and out of your vault: where it is, its tracking key and its Banxico receipt (CEP).
            Something wrong? Open a clarification from the transfer.
          </p>
        </div>
        <SegmentedControl
          legend="Show"
          options={tabs}
          value={tab ?? "transfers"}
          onChange={choose}
          className="w-full sm:w-80"
          testId="transfers-tabs"
        />
      </header>

      {tab === "transfers" ? (
        <TransferList
          openId={inspected?.transfer?.transfer_id ?? null}
          onOpen={open}
          onRetry={() => void useTransfersStore.getState().load(fetchPage)}
          onLoadMore={() => void useTransfersStore.getState().loadMore(fetchPage)}
        />
      ) : tab === "clarifications" ? (
        <ClarificationList
          focusTitle={focusClarifications}
          onOpenTransfer={open}
          onShowTransfers={() => choose("transfers")}
          onRetry={() => void useTransfersStore.getState().loadClarifications(fetchClarifications)}
        />
      ) : null}

      <TransferInspector
        inspected={inspected}
        activeCount={active}
        onClose={close}
        onRetry={retryInspect}
        onOpenClarification={openClarification}
        onShowClarifications={showClarifications}
      />
    </div>
  );
}
