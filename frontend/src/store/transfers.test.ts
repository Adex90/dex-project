/** Run with: npm test */
import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

import { useTransfersStore } from "./transfers.ts";
import { parseClarification } from "../lib/transfers.ts";

const store = () => useTransfersStore.getState();
beforeEach(() => store().reset());

const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;

/** An order, as the API answers it: pending, or settled with its CEP data. */
function order(n: number, at: string, settled = false) {
  return {
    transfer_id: id(n),
    direction: "outgoing",
    kind: "withdrawal",
    status: settled ? "settled" : "pending",
    stage: settled ? "settled" : "initiated",
    amount: "100.00000000",
    tracking_key: `SC${n}`,
    operation_date: settled ? "2026-11-24" : null,
    concept: "Retiro",
    numeric_reference: 1,
    counterparty: { name: "MARIA LOPEZ", bank: { code: "012", name: "BBVA México" }, account_last4: "1234" },
    initiated_at: at,
    sent_at: settled ? at : null,
    settled_at: settled ? at : null,
    returned_at: null,
    return_reason: null,
    transaction_id: id(1000 + n),
    refund_transaction_id: null,
    simulated: false,
    cep: settled
      ? {
          available: true,
          operation_date: "2026-11-24",
          tracking_key: `SC${n}`,
          sender_bank: { code: "646", name: "STP" },
          receiver_bank: { code: "012", name: "BBVA México" },
          beneficiary_account: null,
          beneficiary_account_last4: "1234",
          amount: "100.00000000",
          url: "https://www.banxico.org.mx/cep/",
        }
      : null,
    clarification: null,
  };
}

function clarification(n: number, transfer: number, status = "open") {
  const closed = status === "resolved" || status === "declined";
  return {
    clarification_id: id(500 + n),
    number: n,
    transfer_id: id(transfer),
    direction: "outgoing",
    tracking_key: `SC${transfer}`,
    reason: "delayed",
    description: null,
    status,
    resolution_note: closed ? "The receiving bank confirmed it." : null,
    opened_at: "2026-11-24T20:00:00Z",
    updated_at: "2026-11-24T20:00:00Z",
    closed_at: closed ? "2026-11-25T20:00:00Z" : null,
    history: [{ status: "open", at: "2026-11-24T20:00:00Z" }],
  };
}

const never = () => new Promise<unknown>(() => undefined);

test("the first page, then the next ones until the last", async () => {
  const pages: Record<string, unknown> = {
    first: { transfers: [order(5, "2026-11-24T18:05:00Z"), order(4, "2026-11-24T18:04:00Z")], next_cursor: "c2" },
    c2: { transfers: [order(3, "2026-11-24T18:03:00Z")], next_cursor: null },
  };
  const asked: (string | null)[] = [];
  const fetchPage = async (cursor: string | null) => {
    asked.push(cursor);
    return pages[cursor ?? "first"];
  };
  await store().load(fetchPage);
  await store().loadMore(fetchPage);
  await store().loadMore(fetchPage); // the last page was shown: nothing more to ask
  assert.deepEqual(asked, [null, "c2"]);
  assert.deepEqual(
    store().transfers.map((t) => t.tracking_key),
    ["SC5", "SC4", "SC3"],
  );
  assert.equal(store().cursor, null);
});

test("a refresh shows the first page as it is now and keeps the older pages", async () => {
  await store().load(async () => ({
    transfers: [order(2, "2026-11-24T18:02:00Z"), order(1, "2026-11-24T18:01:00Z")],
    next_cursor: "older",
  }));
  // A new deposit arrived and the withdrawal settled; the first page now ends at it.
  await store().load(async () => ({
    transfers: [order(3, "2026-11-24T18:03:00Z"), order(2, "2026-11-24T18:02:00Z", true)],
    next_cursor: "c2",
  }));
  assert.deepEqual(
    store().transfers.map((t) => [t.tracking_key, t.status]),
    [
      ["SC3", "pending"],
      ["SC2", "settled"],
      ["SC1", "pending"],
    ],
  );
  // The cursor of the pages already loaded stays: the next page follows them.
  assert.equal(store().cursor, "older");
});

test("a malformed page is refused; a failed refresh keeps what is shown", async () => {
  await store().load(async () => ({
    transfers: [{ ...order(1, "2026-11-24T18:01:00Z"), stage: "settled" }],
    next_cursor: null,
  }));
  assert.equal(store().status, "error");
  assert.equal(store().error?.message, "Your transfers could not be read.");
  store().reset();
  await store().load(async () => ({ transfers: [order(1, "2026-11-24T18:01:00Z")], next_cursor: "next" }));
  await store().load(async () => {
    throw new Error("offline");
  });
  assert.deepEqual(
    { shown: store().transfers.length, status: store().status, error: store().error?.message },
    { shown: 1, status: "ready", error: "offline" },
  );
  await store().loadMore(async () => {
    throw new Error("offline");
  });
  assert.deepEqual(
    { shown: store().transfers.length, loadingMore: store().loadingMore },
    { shown: 1, loadingMore: false },
  );
});

test("a newer load wins over an older one still in flight", async () => {
  const slow = store().load(never);
  await store().load(async () => ({ transfers: [order(1, "2026-11-24T18:01:00Z")], next_cursor: null }));
  assert.equal(store().transfers.length, 1);
  void slow;
});

test("the inspector opens a transfer from the pages shown, by its id or a ledger movement's", async () => {
  await store().load(async () => ({ transfers: [order(1, "2026-11-24T18:01:00Z")], next_cursor: null }));
  let fetched = 0;
  const fetchOne = async () => {
    fetched += 1;
    return order(9, "2026-11-20T10:00:00Z", true);
  };
  await store().inspect(fetchOne, id(1001)); // the order's charge in the ledger
  assert.equal(store().inspected?.transfer?.transfer_id, id(1));
  assert.equal(fetched, 0);
  // One not on the pages shown: asked by its id.
  await store().inspect(fetchOne, id(9));
  assert.deepEqual(
    {
      status: store().inspected?.status,
      key: store().inspected?.key,
      transfer: store().inspected?.transfer?.tracking_key,
    },
    { status: "ready", key: id(9), transfer: "SC9" },
  );
  assert.equal(fetched, 1);
  store().closeInspector();
  assert.equal(store().inspected, null);
});

test("an inspected transfer that is not the client's, or is malformed, says so", async () => {
  await store().inspect(async () => {
    throw new Error("not found");
  }, id(7));
  assert.deepEqual(
    { status: store().inspected?.status, error: store().inspected?.error?.message },
    { status: "error", error: "not found" },
  );
  await store().inspect(async () => ({ ...order(7, "2026-11-24T18:01:00Z"), status: "lost" }), id(7));
  assert.equal(store().inspected?.error?.message, "The transfer could not be read.");
});

test("opening another transfer, or closing, drops the one still loading", async () => {
  void store().inspect(never, id(7));
  assert.equal(store().inspected?.status, "loading");
  await store().inspect(async () => order(8, "2026-11-24T18:01:00Z"), id(8));
  assert.equal(store().inspected?.transfer?.transfer_id, id(8));
  void store().inspect(never, id(7));
  store().closeInspector();
  assert.equal(store().inspected, null);
});

test("the open transfer follows the vault: from a fresh page, or asked again", async () => {
  await store().load(async () => ({ transfers: [order(1, "2026-11-24T18:01:00Z")], next_cursor: null }));
  await store().inspect(never, id(1));
  await store().load(async () => ({ transfers: [order(1, "2026-11-24T18:01:00Z", true)], next_cursor: null }));
  assert.equal(store().inspected?.transfer?.status, "settled");

  await store().inspect(async () => order(9, "2026-11-20T10:00:00Z"), id(9));
  await store().refreshInspected(async () => order(9, "2026-11-20T10:00:00Z", true));
  assert.equal(store().inspected?.transfer?.status, "settled");
  // A failed or malformed refresh keeps what is shown.
  await store().refreshInspected(async () => {
    throw new Error("offline");
  });
  await store().refreshInspected(async () => ({ broken: true }));
  assert.deepEqual(
    { status: store().inspected?.status, transfer: store().inspected?.transfer?.status },
    { status: "ready", transfer: "settled" },
  );
});

test("clarifications load, and one just opened shows on its transfer and on top", async () => {
  await store().load(async () => ({
    transfers: [order(2, "2026-11-24T18:02:00Z"), order(1, "2026-11-24T18:01:00Z")],
    next_cursor: null,
  }));
  await store().inspect(never, id(1));
  await store().loadClarifications(async () => ({ clarifications: [clarification(1, 2, "resolved")] }));
  assert.equal(store().clarificationsStatus, "ready");

  const opened = parseClarification(clarification(2, 1));
  assert.ok(opened);
  store().clarificationOpened(opened);
  assert.deepEqual(
    store().clarifications.map((c) => c.number),
    [2, 1],
  );
  assert.deepEqual(store().transfers.find((t) => t.transfer_id === id(1))?.clarification, {
    clarification_id: id(502),
    number: 2,
    status: "open",
  });
  assert.equal(store().transfers.find((t) => t.transfer_id === id(2))?.clarification, null);
  assert.equal(store().inspected?.transfer?.clarification?.number, 2);
  // The same one answered again (a retried request) stays in its place.
  store().clarificationOpened(opened);
  assert.equal(store().clarifications.length, 2);
});

test("an older clarification answered again does not replace a newer one on its transfer", async () => {
  await store().load(async () => ({
    transfers: [
      { ...order(1, "2026-11-24T18:01:00Z"), clarification: { clarification_id: id(505), number: 5, status: "open" } },
    ],
    next_cursor: null,
  }));
  store().clarificationOpened(parseClarification(clarification(4, 1, "declined"))!);
  assert.equal(store().transfers[0]?.clarification?.number, 5);
});

test("malformed clarifications are refused; a failed refresh keeps them", async () => {
  await store().loadClarifications(async () => ({ clarifications: [{ ...clarification(1, 1), status: "lost" }] }));
  assert.equal(store().clarificationsStatus, "error");
  assert.equal(store().clarificationsError?.message, "Your clarifications could not be read.");
  await store().loadClarifications(async () => ({ clarifications: [clarification(1, 1)] }));
  await store().loadClarifications(async () => {
    throw new Error("offline");
  });
  assert.deepEqual(
    { shown: store().clarifications.length, status: store().clarificationsStatus },
    { shown: 1, status: "ready" },
  );
});

test("reset forgets everything (sign-out)", async () => {
  await store().load(async () => ({ transfers: [order(1, "2026-11-24T18:01:00Z")], next_cursor: "c2" }));
  await store().loadClarifications(async () => ({ clarifications: [clarification(1, 1)] }));
  await store().inspect(never, id(1));
  store().reset();
  assert.deepEqual(
    {
      transfers: store().transfers.length,
      cursor: store().cursor,
      status: store().status,
      clarifications: store().clarifications.length,
      inspected: store().inspected,
    },
    { transfers: 0, cursor: null, status: "idle", clarifications: 0, inspected: null },
  );
});
