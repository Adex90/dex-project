/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  ENTRY_LABELS,
  ORDER_TYPE_LABELS,
  PRICE_SOURCE_LABELS,
  SIDE_LABELS,
  STATEMENT_HOLD_LABEL,
  documentDate,
  documentTime,
  downloadContent,
  downloadName,
  filterDocuments,
  isDocumentId,
  parseDocument,
  parseDocumentList,
  periodLabel,
} from "./documents.ts";

const H = (c: string) => c.repeat(64);
const ORDER_ID = "0d9f1c6e-6c2a-4a58-9a0b-2f6d3c1e7a11";

const SUMMARY_CO = {
  id: `co_${ORDER_ID}`,
  kind: "trade_confirmation",
  folio: "CO-00000042",
  title: "Buy 0.7752 AUG1",
  date: "2026-11-23T12:00:00Z",
  reference: H("a"),
  amount: "1249.93000000",
  provisional: false,
};
const SUMMARY_EC = {
  id: "ec_2026-11",
  kind: "account_statement",
  folio: "EC-2026-11-0017",
  title: "Account statement · November 2026",
  date: "2026-11-23T12:00:00Z",
  reference: H("b"),
  amount: "23750.07000000",
  provisional: true,
};

test("the inbox list, validated whole", () => {
  const list = parseDocumentList({ documents: [SUMMARY_EC, SUMMARY_CO], truncated: false });
  assert.ok(list);
  assert.deepEqual(
    list.documents.map((d) => d.kind),
    ["account_statement", "trade_confirmation"],
  );
  assert.deepEqual(
    filterDocuments(list.documents, "trade_confirmation").map((d) => d.folio),
    ["CO-00000042"],
  );
  assert.equal(filterDocuments(list.documents, "all").length, 2);
  for (const bad of [
    { documents: [{ ...SUMMARY_CO, kind: "account_statement" }], truncated: false },
    { documents: [{ ...SUMMARY_CO, id: "co_1" }], truncated: false },
    { documents: [{ ...SUMMARY_CO, reference: "abc" }], truncated: false },
    { documents: [{ ...SUMMARY_CO, amount: 1249.93 }], truncated: false },
    { documents: [SUMMARY_CO] },
  ]) {
    assert.equal(parseDocumentList(bad), null, JSON.stringify(bad).slice(0, 90));
  }
  assert.equal(isDocumentId(`co_${ORDER_ID}`), true);
  assert.equal(isDocumentId("ec_2026-11"), true);
  assert.equal(isDocumentId("ec_../../x"), false);
});

const SEAL = {
  status: "sealed",
  snapshot_id: "3f2a7c1e-1111-4a58-9a0b-2f6d3c1e7a11",
  root: H("c"),
  taken_at: "2026-11-23T12:05:00Z",
  reserve_ratio: "100.00",
  ledger_anchor: 12,
  market_anchor: 4,
  covers_ledger_sequence: 10,
  covers_market_sequence: 4,
};
const PENDING = {
  status: "pending",
  snapshot_id: null,
  root: null,
  taken_at: null,
  reserve_ratio: null,
  ledger_anchor: null,
  market_anchor: null,
  covers_ledger_sequence: 10,
  covers_market_sequence: 4,
};
const CONFIRMATION = {
  id: `co_${ORDER_ID}`,
  kind: "trade_confirmation",
  folio: "CO-00000004",
  holder: { name: "Murray Rothbard", rfc: null },
  executed_at: "2026-11-23T12:00:00Z",
  side: "sell",
  ticker: "STSLA",
  asset_name: "Synthetic Tesla",
  order_type: "market",
  limit_price: null,
  stop_price: null,
  requested_amount: "1000.00000000",
  quantity: "0.22640000",
  price: "4415.30000000",
  gross_amount: "999.62000000",
  commission: "0.00000000",
  net_amount: "999.62000000",
  cost_basis: "1000.00000000",
  realized_pnl: "-0.38000000",
  price_source: "simulated",
  market_sequence: 4,
  reference: H("a"),
  previous_hash: H("d"),
  settlement_sequence: 10,
  settlement_hash: H("e"),
  account_clabe_last4: "0017",
  reserve_seal: SEAL,
};

test("a trade confirmation: figures, hashes and its seal", () => {
  const doc = parseDocument(CONFIRMATION);
  assert.ok(doc && doc.kind === "trade_confirmation");
  assert.equal(doc.realized_pnl, "-0.38000000");
  assert.equal(doc.reserve_seal.status, "sealed");
  assert.ok(parseDocument({ ...CONFIRMATION, reserve_seal: PENDING }));
  assert.ok(
    parseDocument({ ...CONFIRMATION, reserve_seal: { ...SEAL, market_anchor: null } }),
    "a cut before any trade",
  );
  for (const bad of [
    { ...CONFIRMATION, side: "short" },
    { ...CONFIRMATION, commission: "-1" },
    { ...CONFIRMATION, reference: "nope" },
    { ...CONFIRMATION, reserve_seal: { ...SEAL, root: null } },
    { ...CONFIRMATION, reserve_seal: { ...PENDING, snapshot_id: SEAL.snapshot_id } },
    { ...CONFIRMATION, reserve_seal: { ...PENDING, market_anchor: 4 } },
    { ...CONFIRMATION, reserve_seal: { ...SEAL, reserve_ratio: null } },
    { ...CONFIRMATION, account_clabe_last4: "17" },
    { ...CONFIRMATION, kind: "receipt" },
  ]) {
    assert.equal(parseDocument(bad), null, JSON.stringify(bad).slice(0, 90));
  }
  assert.equal(downloadName(doc), "synthetic-capital-CO-00000004.json");
  assert.deepEqual(JSON.parse(downloadContent(doc)), doc, "the download is the document as served");
});

const STATEMENT = {
  id: "ec_2026-11",
  kind: "account_statement",
  folio: "EC-2026-11-0017",
  period: "2026-11",
  period_start: "2026-11-01T06:00:00Z",
  period_end: "2026-11-23T12:00:00Z",
  provisional: true,
  holder: { name: "Murray Rothbard", rfc: "ROMU260120AB1" },
  account_clabe: "646180157000000017",
  opening_balance: "0",
  credits: "25000.00000000",
  debits: "1249.93000000",
  closing_balance: "23750.07000000",
  closing_breakdown: { available: "23450.07000000", on_hold: "300.00000000", held_for_orders: "0" },
  entries: [
    {
      date: "2026-11-23T11:00:00Z",
      kind: "deposit",
      ticker: null,
      tracking_key: "DEMO123",
      sequence: 7,
      hash: H("1"),
      amount: "25000.00000000",
      balance: "25000.00000000",
    },
    {
      date: "2026-11-23T12:00:00Z",
      kind: "market_buy",
      ticker: "AUG1",
      tracking_key: null,
      sequence: 9,
      hash: H("2"),
      amount: "-1249.93000000",
      balance: "23750.07000000",
    },
  ],
  trades: { buys: 1, bought: "1249.93000000", sells: 0, sold: "0", commission: "0" },
  positions: [{ ticker: "AUG1", asset_name: "Gold", quantity: "0.77520000" }],
  reference: H("b"),
  reserve_seal: PENDING,
};

test("an account statement: balances, entries, positions and its seal", () => {
  const doc = parseDocument(STATEMENT);
  assert.ok(doc && doc.kind === "account_statement");
  assert.equal(doc.entries[1]?.amount, "-1249.93000000");
  assert.equal(doc.closing_breakdown.on_hold, "300.00000000");
  for (const bad of [
    { ...STATEMENT, id: "ec_2026-10" },
    { ...STATEMENT, account_clabe: "123" },
    { ...STATEMENT, entries: [{ ...STATEMENT.entries[0], kind: "hold" }] },
    { ...STATEMENT, closing_breakdown: { ...STATEMENT.closing_breakdown, on_hold: "-1" } },
    { ...STATEMENT, positions: [{ ticker: "aug1", asset_name: "Gold", quantity: "1" }] },
  ]) {
    assert.equal(parseDocument(bad), null, JSON.stringify(bad).slice(0, 90));
  }
  assert.equal(periodLabel("2026-11"), "November 2026");
});

test("no document wording would tip off a client under review", () => {
  const TIP_OFF =
    /\b(compliance hold|compliance review|frozen|freeze|restrict\w*|AML|PLD|suspicious|alert|congel\w*|preventiv\w*|UIF|investigat\w*|risk)\b/i;
  for (const words of [...Object.values(ENTRY_LABELS), ...Object.values(ORDER_TYPE_LABELS), STATEMENT_HOLD_LABEL]) {
    assert.doesNotMatch(words, TIP_OFF, words);
  }
  assert.equal(STATEMENT_HOLD_LABEL, "Funds on hold (still fully backed)");
});

test("documents tell time as the ledgers do: in Mexico City", () => {
  assert.equal(documentTime("2026-11-23T12:00:00Z"), "Nov 23, 2026, 06:00:00 Mexico City");
  // 03:00 UTC on the 1st is still the last day of the month before in Mexico City.
  assert.equal(documentDate("2026-12-01T03:00:00Z"), "Nov 30, 2026");
  assert.equal(SIDE_LABELS.sell, "Sell");
  assert.doesNotMatch(Object.values(PRICE_SOURCE_LABELS).join(" "), /risk|alert/i);
});
