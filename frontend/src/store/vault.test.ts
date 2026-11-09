/** Run with: npm test */
import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

import type { Holding, SpeiTransfer, VaultBalance, VaultSnapshot } from "../types/vault.ts";
import { MAX_RECENT_TRANSFERS, useVaultStore } from "./vault.ts";

const vault = () => useVaultStore.getState();

beforeEach(() => vault().reset());

/* Fixtures */

function balance(total: string, cash = "0.00"): VaultBalance {
  return { total_mxn: total, available_cash: cash, invested_assets: "110000.00", unrealized_pnl_percentage: "4.25" };
}

const NVDA: Holding = {
  ticker: "NVDA",
  name: "NVIDIA Corp.",
  shares: "12.5",
  current_price: "3200.00",
  total_value: "40000.00",
  daily_change_pct: "-1.40",
};

const CETES: Holding = {
  ticker: "CETES28",
  name: "CETES 28 días",
  shares: "7000",
  current_price: "10.00",
  total_value: "70000.00",
  daily_change_pct: "0.03",
};

function snapshot(total = "128450.75"): VaultSnapshot {
  return {
    user_id: "7f3c9a52-1d4e-4b8a-9c6f-2e5d8a1b3c4d",
    clabe: "032180000118359719",
    bank_name: "Synthetic Capital IFPE",
    balance: balance(total, "18450.75"),
    reserve_proof: { status: "verified", zk_root_hash: "0x9f4c2a7be81d03c5" },
    holdings: [NVDA, CETES],
  };
}

function transfer(trackingKey: string, status: SpeiTransfer["status"] = "settled"): SpeiTransfer {
  return {
    tracking_key: trackingKey,
    direction: "inbound",
    status,
    amount: "2500.00",
    counterparty_name: "Ana López",
    counterparty_bank: "BBVA México",
    occurred_at: "2026-11-08T17:04:05Z",
  };
}

/** A WebSocket text frame, as `event.data` delivers it. */
const frame = (message: object) => JSON.stringify(message);

/** Opens a live stream whose baseline is the snapshot at `seq`. */
function goLive(seq = 10, vaultSnapshot = snapshot()) {
  vault().setRealtimeStatus("connecting");
  vault().setRealtimeStatus("live");
  assert.equal(vault().receive(frame({ type: "vault.snapshot", seq, vault: vaultSnapshot })), "applied");
}

/* REST */

test("load: a valid body fills every slice", async () => {
  await vault().load(async () => snapshot());
  const s = vault();
  assert.equal(s.status, "ready");
  assert.equal(s.error, null);
  assert.deepEqual(s.account, {
    user_id: "7f3c9a52-1d4e-4b8a-9c6f-2e5d8a1b3c4d",
    clabe: "032180000118359719",
    bank_name: "Synthetic Capital IFPE",
  });
  assert.equal(s.balance?.total_mxn, "128450.75");
  assert.equal(s.reserveProof?.status, "verified");
  assert.deepEqual(s.holdings, [NVDA, CETES]);
  assert.ok(s.updatedAt);
});

test("load: a malformed body is an error, never a guessed state", async () => {
  const body = { ...snapshot(), balance: { ...balance("1"), total_mxn: 128450.75 } };
  await vault().load(async () => body);
  assert.equal(vault().status, "error");
  assert.equal(vault().balance, null);
  assert.match(vault().error?.message ?? "", /expected format/);
});

test("load: a failed refresh reports the error and keeps the data on screen", async () => {
  await vault().load(async () => snapshot());
  await vault().load(async () => {
    throw new Error("network down");
  });
  assert.equal(vault().status, "ready");
  assert.equal(vault().balance?.total_mxn, "128450.75");
  assert.equal(vault().error?.message, "network down");
});

test("load: a newer load cancels the older one, whose late answer is dropped", async () => {
  let releaseSlow: (body: unknown) => void = () => {};
  let slowSignal: AbortSignal | null = null;
  const slow = vault().load(
    (signal) =>
      new Promise((resolve) => {
        slowSignal = signal;
        releaseSlow = resolve;
      }),
  );
  await vault().load(async () => snapshot("200000.00"));
  releaseSlow(snapshot("100.00"));
  await slow;
  assert.equal((slowSignal as AbortSignal | null)?.aborted, true);
  assert.equal(vault().balance?.total_mxn, "200000.00");
});

/* Real time */

test("deltas before the socket's first snapshot are ignored", async () => {
  await vault().load(async () => snapshot());
  vault().setRealtimeStatus("live");
  const result = vault().receive(
    frame({ type: "spei.transfer", seq: 11, transfer: transfer("K1"), balance: balance("1.00") }),
  );
  assert.equal(result, "no-baseline");
  assert.equal(vault().balance?.total_mxn, "128450.75");
});

test("a SPEI deposit sets the authoritative balance and joins the feed", () => {
  goLive(10);
  const after = balance("130950.75", "20950.75");
  const result = vault().receive(frame({ type: "spei.transfer", seq: 11, transfer: transfer("K1"), balance: after }));
  assert.equal(result, "applied");
  assert.deepEqual(vault().balance, after);
  assert.deepEqual(vault().recentTransfers, [transfer("K1")]);
  assert.equal(vault().lastSeq, 11);
  assert.equal(vault().stale, false);
});

test("a replayed message changes nothing", () => {
  goLive(10);
  const message = { type: "spei.transfer", seq: 11, transfer: transfer("K1"), balance: balance("130950.75") };
  vault().receive(frame(message));
  const before = vault();
  assert.equal(vault().receive(frame({ ...message, balance: balance("999.00") })), "duplicate");
  assert.equal(vault().balance, before.balance);
  assert.equal(vault().recentTransfers.length, 1);
});

test("a returned transfer replaces its settled entry instead of adding one", () => {
  goLive(10);
  vault().receive(frame({ type: "spei.transfer", seq: 11, transfer: transfer("K1"), balance: balance("130950.75") }));
  vault().receive(frame({ type: "spei.transfer", seq: 12, transfer: transfer("K2"), balance: balance("133450.75") }));
  vault().receive(
    frame({ type: "spei.transfer", seq: 13, transfer: transfer("K1", "returned"), balance: balance("130950.75") }),
  );
  assert.deepEqual(
    vault().recentTransfers.map((t) => `${t.tracking_key}:${t.status}`),
    ["K1:returned", "K2:settled"],
  );
});

test("a gap is applied but marks the state stale until the next snapshot", () => {
  goLive(10);
  const result = vault().receive(
    frame({ type: "spei.transfer", seq: 13, transfer: transfer("K3"), balance: balance("131000.00") }),
  );
  assert.equal(result, "gap");
  assert.equal(vault().stale, true);
  assert.equal(vault().balance?.total_mxn, "131000.00");

  assert.equal(vault().receive(frame({ type: "vault.snapshot", seq: 14, vault: snapshot("131000.00") })), "applied");
  assert.equal(vault().stale, false);
});

test("the resync after a gap answers at the same version, and clears stale", () => {
  goLive(10);
  vault().receive(frame({ type: "spei.transfer", seq: 13, transfer: transfer("K3"), balance: balance("131000.00") }));
  assert.equal(vault().stale, true);
  // The server reads the account at seq 13 again: same version, now complete.
  assert.equal(vault().receive(frame({ type: "vault.snapshot", seq: 13, vault: snapshot("131000.00") })), "applied");
  assert.equal(vault().stale, false);
  assert.equal(vault().lastSeq, 13);
});

test("an older snapshot is a replay; a delta at the same version too", () => {
  goLive(10);
  assert.equal(vault().receive(frame({ type: "vault.snapshot", seq: 9, vault: snapshot("1.00") })), "duplicate");
  assert.equal(
    vault().receive(frame({ type: "spei.transfer", seq: 10, transfer: transfer("K1"), balance: balance("2.00") })),
    "duplicate",
  );
  assert.equal(vault().balance?.total_mxn, "128450.75");
});

test("a new reserve proof can arrive as a snapshot at the same version", () => {
  goLive(10);
  const pending = { ...snapshot(), reserve_proof: { status: "pending", zk_root_hash: "0xabc123" } };
  assert.equal(vault().receive(frame({ type: "vault.snapshot", seq: 10, vault: pending })), "applied");
  assert.equal(vault().reserveProof?.status, "pending");
});

test("holdings update in place, new ones append, closed ones leave", () => {
  goLive(10);
  const repriced = { ...NVDA, current_price: "3300.00", total_value: "41250.00", daily_change_pct: "1.65" };
  vault().receive(frame({ type: "holding.updated", seq: 11, holding: repriced, balance: balance("129700.75") }));
  assert.deepEqual(vault().holdings, [repriced, CETES]);

  const apple = { ...NVDA, ticker: "AAPL", name: "Apple Inc." };
  vault().receive(frame({ type: "holding.updated", seq: 12, holding: apple, balance: balance("169700.75") }));
  assert.deepEqual(
    vault().holdings.map((h) => h.ticker),
    ["NVDA", "CETES28", "AAPL"],
  );

  const closed = { ...CETES, shares: "0", total_value: "0" };
  vault().receive(frame({ type: "holding.updated", seq: 13, holding: closed, balance: balance("99700.75") }));
  assert.deepEqual(
    vault().holdings.map((h) => h.ticker),
    ["NVDA", "AAPL"],
  );
  assert.equal(vault().balance?.total_mxn, "99700.75");
});

test("a new reserve proof replaces the old one", () => {
  goLive(10);
  const pending = { status: "pending", zk_root_hash: "0xabc123" } as const;
  assert.equal(vault().receive(frame({ type: "reserve.updated", seq: 11, reserve_proof: pending })), "applied");
  assert.deepEqual(vault().reserveProof, pending);
});

test("REST never overwrites the live stream; it applies again once the socket drops", () => {
  goLive(10, snapshot("128450.75"));
  assert.equal(vault().hydrate(snapshot("1.00")), false);
  assert.equal(vault().balance?.total_mxn, "128450.75");

  vault().setRealtimeStatus("reconnecting");
  assert.equal(vault().lastSeq, null);
  assert.equal(vault().hydrate(snapshot("2.00")), true);
  assert.equal(vault().balance?.total_mxn, "2.00");
});

test("invalid frames are reported and ignored", () => {
  goLive(10);
  assert.equal(vault().receive("{not json"), "invalid");
  assert.equal(vault().receive(frame({ type: "spei.transfer", seq: 11, transfer: transfer("K1") })), "invalid");
  assert.equal(vault().lastSeq, 10);
});

test("the transfer feed is bounded, newest first", () => {
  goLive(10);
  const count = MAX_RECENT_TRANSFERS + 5;
  for (let i = 1; i <= count; i++) {
    vault().receive(frame({ type: "spei.transfer", seq: 10 + i, transfer: transfer(`K${i}`), balance: balance("1.00") }));
  }
  const feed = vault().recentTransfers;
  assert.equal(feed.length, MAX_RECENT_TRANSFERS);
  assert.equal(feed[0]?.tracking_key, `K${count}`);
  assert.equal(feed.at(-1)?.tracking_key, `K${count - MAX_RECENT_TRANSFERS + 1}`);
});

test("reset clears data and the stream baseline", () => {
  goLive(10);
  vault().reset();
  const s = vault();
  assert.equal(s.account, null);
  assert.equal(s.balance, null);
  assert.deepEqual(s.holdings, []);
  assert.equal(s.lastSeq, null);
  assert.equal(s.status, "idle");
  assert.equal(s.realtime, "offline");
});
