/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import type { VaultSnapshot } from "../types/vault.ts";
import {
  isDecimalString,
  moneyParts,
  parseRealtimeMessage,
  parseVaultSnapshot,
  shortHash,
  signedPercent,
} from "./vault.ts";

const ROOT = "0x9f4c2a7be81d03c5a6f0e2b4d7c91a3e5f60b8d2c4e6a8b0c1d3e5f7a9b2c4d6";

function snapshot(): VaultSnapshot {
  return {
    user_id: "7f3c9a52-1d4e-4b8a-9c6f-2e5d8a1b3c4d",
    clabe: "032180000118359719",
    bank_name: "Synthetic Capital IFPE",
    balance: {
      total_mxn: "128450.75",
      available_cash: "18450.75",
      invested_assets: "110000.00",
      unrealized_pnl_percentage: "4.25",
    },
    reserve_proof: { status: "verified", zk_root_hash: ROOT },
    holdings: [
      {
        ticker: "NVDA",
        name: "NVIDIA Corp.",
        shares: "12.5",
        current_price: "3200.00",
        total_value: "40000.00",
        daily_change_pct: "-1.40",
      },
      {
        ticker: "CETES28",
        name: "CETES 28 días",
        shares: "7000",
        current_price: "10.00",
        total_value: "70000.00",
        daily_change_pct: "0.03",
      },
    ],
  };
}

/* Formatting */

test("percentages carry an explicit sign and a matching trend", () => {
  assert.deepEqual(signedPercent("4.256"), { text: "+4.26%", trend: "up" });
  assert.deepEqual(signedPercent("-1.4"), { text: "−1.40%", trend: "down" });
  assert.deepEqual(signedPercent("0"), { text: "0.00%", trend: "flat" });
});

test("the sign follows the rounded figure, rounded half-even", () => {
  // Rounds to 0.00: shown as flat, never as "−0.00%" in red.
  assert.deepEqual(signedPercent("-0.001"), { text: "0.00%", trend: "flat" });
  assert.deepEqual(signedPercent("0.005"), { text: "0.00%", trend: "flat" });
  assert.deepEqual(signedPercent("0.015"), { text: "+0.02%", trend: "up" });
  assert.deepEqual(signedPercent("-0.025"), { text: "−0.02%", trend: "down" });
});

test("money splits into grouped integer and cents, exactly", () => {
  assert.deepEqual(moneyParts("128450.75"), { negative: false, integer: "128,450", fraction: "75" });
  assert.deepEqual(moneyParts("9999999999.99999999"), {
    negative: false,
    integer: "10,000,000,000",
    fraction: "00",
  });
  assert.deepEqual(moneyParts("-5"), { negative: true, integer: "5", fraction: "00" });
  assert.deepEqual(moneyParts("1234.5", 0), { negative: false, integer: "1,234", fraction: "" });
});

test("hashes shorten around an ellipsis and keep the 0x prefix", () => {
  assert.equal(shortHash(ROOT), "0x9f4c2a…c4d6");
  assert.equal(shortHash("abcdef0123456789"), "abcdef…6789");
  assert.equal(shortHash("0xabc"), "0xabc");
});

test("decimal strings: digits, an optional sign and point, at most 8 decimals", () => {
  for (const ok of ["0", "15000.00", "-1.25", "0.00000001"]) assert.equal(isDecimalString(ok), true, ok);
  for (const bad of [1500, "1e3", "1.", ".5", " 1", "1,000", "0.000000001", "", null]) {
    assert.equal(isDecimalString(bad), false, String(bad));
  }
});

/* Snapshot validation */

test("a well-formed snapshot is accepted as is", () => {
  assert.deepEqual(parseVaultSnapshot(snapshot()), snapshot());
});

test("unknown fields are dropped, at every level", () => {
  const raw = {
    ...snapshot(),
    debug: true,
    balance: { ...snapshot().balance, overdraft: "999" },
  };
  const parsed = parseVaultSnapshot(raw);
  assert.ok(parsed);
  assert.equal("debug" in parsed, false);
  assert.equal("overdraft" in parsed.balance, false);
});

test("money sent as a JSON number is refused: floats never reach the store", () => {
  const raw = { ...snapshot(), balance: { ...snapshot().balance, total_mxn: 128450.75 } };
  assert.equal(parseVaultSnapshot(raw), null);
});

test("more than 8 decimals is refused", () => {
  const raw = { ...snapshot(), balance: { ...snapshot().balance, available_cash: "1.123456789" } };
  assert.equal(parseVaultSnapshot(raw), null);
});

test("an unknown reserve status or a non-hex root is refused", () => {
  assert.equal(parseVaultSnapshot({ ...snapshot(), reserve_proof: { status: "ok", zk_root_hash: ROOT } }), null);
  assert.equal(
    parseVaultSnapshot({ ...snapshot(), reserve_proof: { status: "verified", zk_root_hash: "<script>" } }),
    null,
  );
});

test("one malformed holding rejects the whole snapshot", () => {
  const holdings = [...snapshot().holdings, { ticker: "AAPL", name: "Apple", shares: "-1" }];
  assert.equal(parseVaultSnapshot({ ...snapshot(), holdings }), null);
});

test("a ticker listed twice is refused: it would be double-counted", () => {
  const first = snapshot().holdings[0];
  assert.ok(first);
  assert.equal(parseVaultSnapshot({ ...snapshot(), holdings: [first, first] }), null);
});

test("a malformed CLABE is kept as sent, for the deposit card to flag", () => {
  const parsed = parseVaultSnapshot({ ...snapshot(), clabe: "032180000118359710" });
  assert.equal(parsed?.clabe, "032180000118359710");
});

/* Real-time messages */

const transfer = {
  tracking_key: "BNET01002611080000123",
  direction: "inbound",
  status: "settled",
  amount: "2500.00",
  counterparty_name: "Ana López",
  counterparty_bank: "BBVA México",
  occurred_at: "2026-11-08T17:04:05Z",
};

test("a SPEI transfer frame parses from JSON text", () => {
  const frame = JSON.stringify({ type: "spei.transfer", seq: 7, transfer, balance: snapshot().balance });
  const message = parseRealtimeMessage(frame);
  assert.ok(message);
  assert.equal(message.type, "spei.transfer");
  assert.equal(message.seq, 7);
});

test("every message type parses", () => {
  const { holdings, balance, reserve_proof } = snapshot();
  const frames = [
    { type: "vault.snapshot", seq: 1, vault: snapshot() },
    { type: "holding.updated", seq: 2, holding: holdings[0], balance },
    { type: "reserve.updated", seq: 3, reserve_proof: { ...reserve_proof, status: "pending" } },
  ];
  for (const frame of frames) assert.equal(parseRealtimeMessage(frame)?.type, frame.type);
});

test("broken JSON, binary frames and unknown types give null", () => {
  assert.equal(parseRealtimeMessage("{not json"), null);
  assert.equal(parseRealtimeMessage(new ArrayBuffer(8)), null);
  assert.equal(parseRealtimeMessage({ type: "account.deleted", seq: 1 }), null);
  assert.equal(parseRealtimeMessage(null), null);
});

test("seq must be a positive integer", () => {
  for (const seq of [0, -1, 1.5, "2", Number.MAX_SAFE_INTEGER + 1, undefined]) {
    const frame = { type: "reserve.updated", seq, reserve_proof: snapshot().reserve_proof };
    assert.equal(parseRealtimeMessage(frame), null, String(seq));
  }
});

test("a transfer needs a positive amount, a known direction and a real date", () => {
  const balance = snapshot().balance;
  for (const broken of [
    { ...transfer, amount: "0" },
    { ...transfer, amount: "-10.00" },
    { ...transfer, direction: "sideways" },
    { ...transfer, occurred_at: "yesterday" },
    { ...transfer, tracking_key: "" },
  ]) {
    assert.equal(parseRealtimeMessage({ type: "spei.transfer", seq: 4, transfer: broken, balance }), null);
  }
});

test("a delta without its authoritative balance is refused", () => {
  assert.equal(parseRealtimeMessage({ type: "spei.transfer", seq: 4, transfer }), null);
});
