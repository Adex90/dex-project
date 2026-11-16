/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  accountLabel,
  assessWithdrawal,
  bankName,
  maskedClabe,
  parseLinkedAccounts,
  parseWithdrawalReceipt,
  receiptTitle,
  withdrawalRequest,
  withdrawalSignature,
} from "./withdrawal.ts";

const ACCOUNT = {
  id: "c0000000-0000-4000-8000-000000000001",
  bank_code: "012",
  bank_name: "BBVA México",
  clabe_last4: "1234",
  holder_name: "ANA PRUEBA",
  linked_at: "2026-11-16T18:00:00Z",
};

const RECEIPT = {
  withdrawal: {
    id: "0a000000-0000-4000-8000-000000000001",
    status: "processing",
    amount: "1250.50000000",
    linked_account: ACCOUNT,
    tracking_key: "SC20261116000000000042",
    requested_at: "2026-11-16T18:05:00.123456Z",
  },
};

test("parseLinkedAccounts: the accounts as the API sends them", () => {
  assert.deepEqual(parseLinkedAccounts({ accounts: [ACCOUNT] }), { accounts: [ACCOUNT] });
  assert.deepEqual(parseLinkedAccounts({ accounts: [] }), { accounts: [] });
  // A bank outside the catalog has no name.
  assert.deepEqual(parseLinkedAccounts({ accounts: [{ ...ACCOUNT, bank_name: null }] })?.accounts[0]?.bank_name, null);
});

test("parseLinkedAccounts: one malformed account rejects the whole list", () => {
  const broken = [
    { ...ACCOUNT, clabe_last4: "12345" },
    { ...ACCOUNT, clabe_last4: "12a4" },
    { ...ACCOUNT, bank_code: "12" },
    { ...ACCOUNT, holder_name: " " },
    { ...ACCOUNT, linked_at: "yesterday" },
    { ...ACCOUNT, id: "" },
    { ...ACCOUNT, bank_name: 12 },
  ];
  for (const account of broken) {
    assert.equal(parseLinkedAccounts({ accounts: [ACCOUNT, account] }), null, JSON.stringify(account));
  }
  assert.equal(parseLinkedAccounts({ accounts: "none" }), null);
  assert.equal(parseLinkedAccounts([ACCOUNT]), null);
  assert.equal(parseLinkedAccounts({ accounts: Array.from({ length: 21 }, () => ACCOUNT) }), null);
});

test("parseWithdrawalReceipt: the withdrawal and its account", () => {
  assert.deepEqual(parseWithdrawalReceipt(RECEIPT), RECEIPT);
  for (const status of ["settled", "returned"]) {
    assert.equal(parseWithdrawalReceipt({ withdrawal: { ...RECEIPT.withdrawal, status } })?.withdrawal.status, status);
  }
  const broken = [
    { status: "pending" },
    { amount: "0" },
    { amount: "-5.00" },
    { amount: 1250.5 },
    { tracking_key: "sc-lowercase" },
    { tracking_key: "A".repeat(31) },
    { requested_at: "" },
    { linked_account: { ...ACCOUNT, clabe_last4: "" } },
    { id: null },
  ];
  for (const change of broken) {
    assert.equal(
      parseWithdrawalReceipt({ withdrawal: { ...RECEIPT.withdrawal, ...change } }),
      null,
      JSON.stringify(change),
    );
  }
  assert.equal(parseWithdrawalReceipt(RECEIPT.withdrawal), null, "the withdrawal comes wrapped");
});

test("bankName, maskedClabe and accountLabel", () => {
  assert.equal(bankName(ACCOUNT), "BBVA México");
  assert.equal(bankName({ bank_code: "137", bank_name: null }), "Bank 137");
  assert.equal(maskedClabe(ACCOUNT), "•••• 1234");
  assert.equal(accountLabel(ACCOUNT), "BBVA México •••• 1234");
});

test("assessWithdrawal: only the available cash, to the cent", () => {
  assert.deepEqual(assessWithdrawal("", "15000.00000000"), {
    problem: "empty",
    max: "15000",
    amount: "0.00",
    remaining: "15000.00000000",
    ready: false,
  });
  assert.deepEqual(assessWithdrawal("1250.5", "15000.00000000"), {
    problem: null,
    max: "15000",
    amount: "1250.50",
    remaining: "13749.50000000",
    ready: true,
  });
  // Exactly everything is fine; one cent more is not.
  assert.equal(assessWithdrawal("15000", "15000.00000000").ready, true);
  assert.equal(assessWithdrawal("15000", "15000.00000000").remaining, "0.00000000");
  assert.equal(assessWithdrawal("15000.01", "15000.00000000").problem, "exceeds");
  assert.equal(assessWithdrawal("15000.01", "15000.00000000").remaining, "0.00000000", "never below zero");
  // Max rounds down to the cent: SPEI settles in cents.
  assert.equal(assessWithdrawal("", "100.12999999").max, "100.12");
  assert.equal(assessWithdrawal("100.12", "100.12999999").ready, true);
  assert.equal(assessWithdrawal("100.13", "100.12999999").problem, "exceeds");
  // Nothing available: no Max, nothing to send.
  assert.equal(assessWithdrawal("", "0.00000000").max, "");
  assert.equal(assessWithdrawal("1", "0.00000000").problem, "exceeds");
  // "0." is still nothing.
  assert.equal(assessWithdrawal("0.", "10.00000000").problem, "empty");
});

test("withdrawalSignature: the same account and amount are the same withdrawal", () => {
  assert.equal(withdrawalSignature("a", "1250.5"), withdrawalSignature("a", "1250.50"));
  assert.notEqual(withdrawalSignature("a", "1250.5"), withdrawalSignature("a", "1250.51"));
  assert.notEqual(withdrawalSignature("a", "10"), withdrawalSignature("b", "10"));
});

test("withdrawalRequest: what POST /api/v1/wealth/withdraw takes", () => {
  assert.deepEqual(withdrawalRequest(ACCOUNT.id, "1250.5", "key-1"), {
    linked_account_id: ACCOUNT.id,
    amount: "1250.50",
    idempotency_key: "key-1",
  });
  assert.equal(withdrawalRequest(ACCOUNT.id, "7", "key-2").amount, "7.00");
});

test("receiptTitle: one headline per status", () => {
  assert.equal(receiptTitle({ status: "processing" }), "Withdrawal processing");
  assert.equal(receiptTitle({ status: "settled" }), "Withdrawal settled");
  assert.equal(receiptTitle({ status: "returned" }), "Withdrawal returned");
});
