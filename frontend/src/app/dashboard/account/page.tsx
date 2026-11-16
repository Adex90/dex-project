"use client";

import { useCallback, useState } from "react";
import { useRouter } from "next/navigation";

import { LedgerFeed } from "@/components/dashboard/ledger-feed";
import { VaultCard } from "@/components/dashboard/vault-card";
import { useDashboard } from "@/components/layout/dashboard-shell";
import { CardManagement } from "@/components/wealth/card-management";
import { CashYieldSection } from "@/components/wealth/cash-yield-section";
import { WithdrawalDrawer } from "@/components/wealth/withdrawal-drawer";
import { ApiError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { parseLinkedAccounts, parseWithdrawalReceipt } from "@/lib/withdrawal";
import { useAuthStore } from "@/store/auth";
import { useDashboardStore } from "@/store/dashboard";
import { useVaultStore } from "@/store/vault";
import type { LinkedAccount, Withdrawal, WithdrawalRequest } from "@/types/wealth";

const UNREADABLE_ACCOUNTS = "Your linked accounts could not be read. Please try again.";
const UNREADABLE_RECEIPT =
  "The withdrawal was sent, but its receipt could not be read. Check your account before trying again: retrying this same withdrawal cannot send it twice.";
const INVALID_AMOUNT = "Enter an amount in pesos, with at most two decimals.";

/*
 * Account: the wealth and banking dashboard, around the IFPE sight account.
 *
 * - Cash & Yield (components/wealth/cash-yield-section.tsx): the uninvested
 *   cash and the Sovereign Yield counter, live. A preview: nothing is paid.
 * - Withdraw, in the Cash & Yield section: the fiat off-ramp
 *   (components/wealth/withdrawal-drawer.tsx), by SPEI to a linked bank
 *   account in the client's own name, from the available cash only.
 * - The debit card (components/wealth/card-management.tsx): lock, PIN,
 *   settings and saveback. A preview: no card is issued yet.
 * - The account itself (balance, CLABE, deposits) and its ledger, as before.
 *
 * Layout: one column on phones (yield, card, account, ledger); from xl, the
 * yield above the account in two thirds, the card in the third beside them,
 * and the ledger across the full width (a wide table, 760 px at least).
 */
export default function AccountPage() {
  const router = useRouter();
  const { token, kycLocked, refresh } = useDashboard();
  const [withdrawing, setWithdrawing] = useState(false);
  const cash = useVaultStore((s) => s.balance?.available_cash ?? null);
  const held = useVaultStore((s) => s.balance?.held_cash ?? "0");
  const summary = useDashboardStore((s) => s.summary);
  const movements = useDashboardStore((s) => s.movements);
  const hasMore = useDashboardStore((s) => s.hasMoreMovements);
  const loading = useDashboardStore((s) => s.loading);
  const loadingMore = useDashboardStore((s) => s.loadingMore);
  const loadMore = useDashboardStore((s) => s.loadMoreMovements);

  const endSession = useCallback(() => {
    useAuthStore.getState().expire();
    router.replace(EXPIRED_LOGIN_PATH);
  }, [router]);

  const loadAccounts = useCallback(
    async (signal: AbortSignal): Promise<LinkedAccount[]> => {
      try {
        const parsed = parseLinkedAccounts(await api.linkedAccounts(token, signal));
        if (!parsed) throw new Error(UNREADABLE_ACCOUNTS);
        return parsed.accounts;
      } catch (error) {
        if (error instanceof ApiError && error.isUnauthorized) endSession();
        throw error;
      }
    },
    [token, endSession],
  );

  const withdraw = useCallback(
    async (request: WithdrawalRequest): Promise<Withdrawal> => {
      let body: unknown;
      try {
        body = await api.withdraw(token, request);
      } catch (error) {
        if (error instanceof ApiError && error.isUnauthorized) endSession();
        // The drawer validates the amount before sending: a 400 means it did not travel in the API's format.
        if (error instanceof ApiError && error.code === "VALIDACION") throw new Error(INVALID_AMOUNT);
        throw error;
      }
      // A 2xx means the money left: never claim a failure that may be false.
      const parsed = parseWithdrawalReceipt(body);
      if (!parsed || parsed.withdrawal.linked_account.id !== request.linked_account_id) {
        throw new Error(UNREADABLE_RECEIPT);
      }
      // The vault's socket brings the new balance; the ledger and the summary follow.
      void refresh();
      return parsed.withdrawal;
    },
    [token, endSession, refresh],
  );

  return (
    <div className="space-y-6">
      <div className="grid gap-4 sm:gap-6 xl:grid-cols-3">
        <CashYieldSection
          className="min-w-0 xl:col-span-2"
          onWithdraw={() => setWithdrawing(true)}
          withdrawLocked={kycLocked}
        />
        {/* self-start: as tall as its content, not stretched beside the two sections it spans. */}
        <CardManagement className="min-w-0 xl:col-start-3 xl:row-span-2 xl:row-start-1 xl:self-start" />
        <div className="min-w-0 xl:col-span-2">
          <VaultCard
            account={summary?.boveda ?? null}
            reserve={summary?.reserva ?? null}
            token={token}
            kycLocked={kycLocked}
            onChanged={refresh}
          />
        </div>
      </div>
      <div className="min-w-0">
        <LedgerFeed
          movements={movements}
          accountId={summary?.boveda?.id ?? null}
          loading={loading}
          hasMore={hasMore}
          loadingMore={loadingMore}
          onLoadMore={() => void loadMore(token)}
        />
      </div>
      <WithdrawalDrawer
        open={withdrawing && cash !== null}
        onClose={() => setWithdrawing(false)}
        cash={cash ?? "0"}
        held={held}
        loadAccounts={loadAccounts}
        onConfirm={withdraw}
      />
    </div>
  );
}
