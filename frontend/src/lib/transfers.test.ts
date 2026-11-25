/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  CLARIFICATION_REASONS,
  CLARIFICATION_STATUS_LABELS,
  KIND_LABELS,
  REASON_LABELS,
  STEP_LABELS,
  activeClarifications,
  cepAmount,
  cepDetailsText,
  cepNotice,
  characterCount,
  clarificationBlock,
  clarificationBlockText,
  clarificationProblem,
  clarificationRequest,
  clarificationSignature,
  clarificationStatusText,
  cleanDescription,
  counterpartyLabel,
  formatOperationDate,
  isCepUrl,
  isSpeiMovement,
  parseClarification,
  parseSpeiClarifications,
  parseSpeiTransfers,
  parseTrackedTransfer,
  readTransfersQuery,
  reasonsFor,
  returnReasonText,
  transferMatches,
  transferStatusLabel,
  transferTimeline,
  transferTitle,
  transfersPath,
} from "./transfers.ts";
import type { TrackedTransfer } from "../types/transfers.ts";

const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
const BBVA = { code: "012", name: "BBVA México" };
const STP = { code: "646", name: "STP" };
const CEP_URL = "https://www.banxico.org.mx/cep/";
const OWN_CLABE = "646180157000000004";

type Raw = Record<string, unknown>;

/** A withdrawal to the client's BBVA account that settled, as the API answers it. */
function withdrawal(overrides: Raw = {}): Raw {
  return {
    transfer_id: id(1),
    direction: "outgoing",
    kind: "withdrawal",
    status: "settled",
    stage: "settled",
    amount: "1250.50000000",
    tracking_key: "SC20261124000001",
    operation_date: "2026-11-24",
    concept: "Retiro a cuenta propia",
    numeric_reference: 1124,
    counterparty: { name: "MARIA LOPEZ", bank: BBVA, account_last4: "1234" },
    initiated_at: "2026-11-24T18:00:00Z",
    sent_at: "2026-11-24T18:00:05Z",
    settled_at: "2026-11-24T18:00:09Z",
    returned_at: null,
    return_reason: null,
    transaction_id: id(11),
    refund_transaction_id: null,
    simulated: false,
    cep: {
      available: true,
      operation_date: "2026-11-24",
      tracking_key: "SC20261124000001",
      sender_bank: STP,
      receiver_bank: BBVA,
      beneficiary_account: null,
      beneficiary_account_last4: "1234",
      amount: "1250.50000000",
      url: CEP_URL,
    },
    clarification: null,
    ...overrides,
  };
}

/** An order still waiting for the dispatcher. */
function pending(overrides: Raw = {}): Raw {
  return withdrawal({
    transfer_id: id(3),
    status: "pending",
    stage: "initiated",
    operation_date: null,
    sent_at: null,
    settled_at: null,
    cep: null,
    ...overrides,
  });
}

/** A withdrawal the receiving bank sent back after it settled. */
function returnedOrder(overrides: Raw = {}): Raw {
  return withdrawal({
    transfer_id: id(4),
    status: "returned",
    stage: "returned",
    returned_at: "2026-11-24T19:30:00Z",
    return_reason: "account_closed",
    refund_transaction_id: id(14),
    ...overrides,
  });
}

/** An order the settlement provider refused before it settled. */
function rejectedOrder(overrides: Raw = {}): Raw {
  return withdrawal({
    transfer_id: id(5),
    status: "returned",
    stage: "rejected",
    operation_date: null,
    sent_at: null,
    settled_at: null,
    returned_at: "2026-11-24T18:01:00Z",
    return_reason: "other",
    refund_transaction_id: id(15),
    cep: null,
    ...overrides,
  });
}

/** A deposit from another bank, credited. */
function deposit(overrides: Raw = {}): Raw {
  return {
    transfer_id: id(2),
    direction: "incoming",
    kind: "deposit",
    status: "settled",
    stage: "settled",
    amount: "5000.00000000",
    tracking_key: "BBVA2026112300777",
    operation_date: "2026-11-23",
    concept: "Ahorro noviembre",
    numeric_reference: 0,
    counterparty: { name: "Juan Pérez", bank: BBVA, account_last4: "9876" },
    initiated_at: "2026-11-23T15:00:00Z",
    sent_at: null,
    settled_at: "2026-11-23T15:00:00Z",
    returned_at: null,
    return_reason: null,
    transaction_id: id(12),
    refund_transaction_id: null,
    simulated: false,
    cep: {
      available: true,
      operation_date: "2026-11-23",
      tracking_key: "BBVA2026112300777",
      sender_bank: BBVA,
      receiver_bank: STP,
      beneficiary_account: OWN_CLABE,
      beneficiary_account_last4: "0004",
      amount: "5000.00000000",
      url: CEP_URL,
    },
    clarification: null,
    ...overrides,
  };
}

/** A deposit the account could not receive: it went back to the sender, never credited. */
function returnedDeposit(overrides: Raw = {}): Raw {
  return deposit({
    transfer_id: id(6),
    status: "returned",
    stage: "returned",
    settled_at: null,
    returned_at: "2026-11-23T15:00:00Z",
    transaction_id: null,
    cep: null,
    ...overrides,
  });
}

/** A demo deposit: no bank, no operation date, no CEP at Banxico. */
function demoDeposit(overrides: Raw = {}): Raw {
  return deposit({
    transfer_id: id(7),
    tracking_key: "DEMO1",
    operation_date: null,
    concept: null,
    counterparty: { name: null, bank: { code: null, name: null }, account_last4: null },
    simulated: true,
    cep: {
      available: false,
      operation_date: null,
      tracking_key: "DEMO1",
      sender_bank: { code: null, name: null },
      receiver_bank: { code: "999", name: "Synthetic Capital" },
      beneficiary_account: "999180000000000001",
      beneficiary_account_last4: "0001",
      amount: "5000.00000000",
      url: CEP_URL,
    },
    ...overrides,
  });
}

function parsed(raw: Raw): TrackedTransfer {
  const transfer = parseTrackedTransfer(raw);
  assert.ok(transfer, JSON.stringify(raw));
  return transfer;
}

function withCep(raw: Raw, cep: Raw): Raw {
  return { ...raw, cep: { ...(raw.cep as Raw), ...cep } };
}

test("every kind of transfer the backend answers parses", () => {
  const page = parseSpeiTransfers({
    transfers: [
      withdrawal(),
      deposit(),
      pending(),
      pending({ transfer_id: id(8), stage: "sent", sent_at: "2026-11-24T18:00:05Z", kind: "payment" }),
      returnedOrder(),
      rejectedOrder(),
      returnedDeposit(),
      demoDeposit(),
    ],
    next_cursor: "1764007200000000_00000000000040008000000000000007",
  });
  assert.ok(page);
  assert.equal(page.transfers.length, 8);
  assert.equal(page.next_cursor, "1764007200000000_00000000000040008000000000000007");
  assert.deepEqual(parseSpeiTransfers({ transfers: [], next_cursor: null }), { transfers: [], next_cursor: null });
});

test("a transfer's free text is trimmed, blank is nothing, and lengths count characters", () => {
  const blank = parsed(deposit({ concept: "   ", counterparty: { name: "", bank: BBVA, account_last4: null } }));
  assert.equal(blank.concept, null);
  assert.equal(blank.counterparty.name, null);
  // 120 characters, each two UTF-16 units: within the 120 the database allows.
  const wide = "𝔸".repeat(120);
  assert.equal(
    parsed(deposit({ counterparty: { name: wide, bank: BBVA, account_last4: null } })).counterparty.name,
    wide,
  );
  assert.equal(
    parseTrackedTransfer(deposit({ counterparty: { name: `${wide}𝔸`, bank: BBVA, account_last4: null } })),
    null,
  );
});

test("incoherent transfers are refused whole", () => {
  const cases: [string, Raw][] = [
    ["an incoming withdrawal", withdrawal({ direction: "incoming" })],
    ["an outgoing deposit", deposit({ direction: "outgoing", kind: "deposit" })],
    ["a stage of another status", withdrawal({ status: "pending" })],
    [
      "a deposit waiting for the dispatcher",
      deposit({ status: "pending", stage: "initiated", settled_at: null, cep: null }),
    ],
    ["a deposit that never went out", returnedDeposit({ stage: "rejected" })],
    ["settled without its time", withdrawal({ settled_at: null, cep: null })],
    ["sent without its time", pending({ stage: "sent" })],
    ["a deposit with a sent time", deposit({ sent_at: "2026-11-23T15:00:00Z" })],
    ["returned without its time", returnedOrder({ returned_at: null })],
    ["a return time on a settled one", withdrawal({ returned_at: "2026-11-24T19:30:00Z" })],
    ["settled without its CEP data", withdrawal({ cep: null })],
    ["CEP data before it settled", pending({ cep: withdrawal().cep as Raw })],
    ["a return reason on a settled one", withdrawal({ return_reason: "other" })],
    ["a returned order without its reason", returnedOrder({ return_reason: null })],
    ["a returned deposit with a reason", returnedDeposit({ return_reason: "other" })],
    ["a refund on a settled one", withdrawal({ refund_transaction_id: id(14) })],
    ["a returned order without its refund", returnedOrder({ refund_transaction_id: null })],
    ["an order without its charge", withdrawal({ transaction_id: null })],
    ["a returned deposit that was credited", returnedDeposit({ transaction_id: id(16) })],
    ["a credited deposit without its credit", deposit({ transaction_id: null })],
    ["an unknown status", withdrawal({ status: "frozen" })],
    ["an unknown return reason", returnedOrder({ return_reason: "account_frozen" })],
    ["a lower-case tracking key", withdrawal({ tracking_key: "sc1" })],
    ["a tracking key past 30 characters", withdrawal({ tracking_key: "A".repeat(31) })],
    ["a zero amount", withdrawal({ amount: "0.00" })],
    ["an operation date that does not exist", withdrawal({ operation_date: "2026-02-30" })],
    ["a numeric reference past 7 digits", withdrawal({ numeric_reference: 10_000_000 })],
    [
      "a full account number of someone else",
      withdrawal({ counterparty: { name: null, bank: BBVA, account_last4: "01234" } }),
    ],
    [
      "a bank name without its code",
      withdrawal({ counterparty: { name: null, bank: { code: null, name: "BBVA" }, account_last4: null } }),
    ],
    ["a transfer id that is not a UUID", withdrawal({ transfer_id: "1" })],
    ["simulated as a string", withdrawal({ simulated: "false" })],
    [
      "a malformed clarification",
      withdrawal({ clarification: { clarification_id: id(9), number: 0, status: "open" } }),
    ],
    [
      "an unknown clarification status",
      withdrawal({ clarification: { clarification_id: id(9), number: 1, status: "closed" } }),
    ],
  ];
  for (const [what, raw] of cases) assert.equal(parseTrackedTransfer(raw), null, what);
});

test("the CEP data must match their transfer, and the link may only open Banxico", () => {
  const cases: [string, Raw][] = [
    ["another tracking key", withCep(withdrawal(), { tracking_key: "SC2" })],
    ["another amount", withCep(withdrawal(), { amount: "1250.51000000" })],
    ["available without its operation date", withCep(withdrawal(), { operation_date: null })],
    ["available although simulated", withdrawal({ simulated: true })],
    ["someone else's full account", withCep(withdrawal(), { beneficiary_account: "012180001234567897" })],
    ["a deposit without the client's own account", withCep(deposit(), { beneficiary_account: null })],
    ["the wrong last digits of the client's account", withCep(deposit(), { beneficiary_account_last4: "1111" })],
    ["another site", withCep(withdrawal(), { url: "https://evil.example/cep/" })],
    ["plain http", withCep(withdrawal(), { url: "http://www.banxico.org.mx/cep/" })],
    ["a look-alike host", withCep(withdrawal(), { url: "https://www.banxico.org.mx.evil.example/cep/" })],
    ["credentials in the link", withCep(withdrawal(), { url: "https://user@www.banxico.org.mx/cep/" })],
    ["a script", withCep(withdrawal(), { url: "javascript:alert(1)" })],
  ];
  for (const [what, raw] of cases) assert.equal(parseTrackedTransfer(raw), null, what);
  assert.equal(isCepUrl(CEP_URL), true);
  assert.equal(isCepUrl("https://www.banxico.org.mx/cep/?fecha=2026-11-24"), true);
  // Not available yet (no operation date) is fine: the screen says so.
  assert.equal(
    parsed(withCep(withdrawal({ operation_date: null }), { available: false, operation_date: null })).cep?.available,
    false,
  );
});

test("a page is refused whole when one transfer is malformed, repeats or there are too many", () => {
  assert.equal(parseSpeiTransfers({ transfers: [withdrawal(), withdrawal({ status: "x" })], next_cursor: null }), null);
  assert.equal(parseSpeiTransfers({ transfers: [withdrawal(), withdrawal()], next_cursor: null }), null);
  const many = Array.from({ length: 101 }, (_, i) => withdrawal({ transfer_id: id(100 + i) }));
  assert.equal(parseSpeiTransfers({ transfers: many, next_cursor: null }), null);
  assert.equal(parseSpeiTransfers({ transfers: [withdrawal()], next_cursor: 7 }), null);
  assert.equal(parseSpeiTransfers({ transfers: {}, next_cursor: null }), null);
  assert.equal(parseSpeiTransfers(null), null);
});

/** A clarification as the API answers it. */
function clarification(overrides: Raw = {}): Raw {
  return {
    clarification_id: id(21),
    number: 3,
    transfer_id: id(1),
    direction: "outgoing",
    tracking_key: "SC20261124000001",
    reason: "not_received",
    description: "The beneficiary does not see it yet.",
    status: "open",
    resolution_note: null,
    opened_at: "2026-11-24T20:00:00Z",
    updated_at: "2026-11-24T20:00:00Z",
    closed_at: null,
    history: [{ status: "open", at: "2026-11-24T20:00:00Z" }],
    ...overrides,
  };
}

const RESOLVED: Raw = {
  status: "resolved",
  resolution_note: "The receiving bank confirmed the credit on November 25.",
  updated_at: "2026-11-25T16:00:00Z",
  closed_at: "2026-11-25T16:00:00Z",
  history: [
    { status: "open", at: "2026-11-24T20:00:00Z" },
    { status: "in_review", at: "2026-11-25T10:00:00Z" },
    { status: "resolved", at: "2026-11-25T16:00:00Z" },
  ],
};

test("clarifications parse, and the closed ones carry their note and closing time", () => {
  const list = parseSpeiClarifications({
    clarifications: [clarification({ clarification_id: id(22), number: 4, ...RESOLVED }), clarification()],
  });
  assert.ok(list);
  assert.deepEqual(
    list.clarifications.map((c) => [c.number, c.status, c.history.length]),
    [
      [4, "resolved", 3],
      [3, "open", 1],
    ],
  );
  assert.equal(parseClarification(clarification({ description: null, reason: "delayed" }))?.description, null);
  assert.equal(parseClarification(clarification({ description: "  " }))?.description, null);
  // An in-review one may already carry a note.
  assert.ok(
    parseClarification(clarification({ status: "in_review", resolution_note: "We asked the receiving bank." })),
  );

  const cases: [string, Raw][] = [
    ["resolved without its note", clarification({ ...RESOLVED, resolution_note: null })],
    ["declined without its closing time", clarification({ ...RESOLVED, status: "declined", closed_at: null })],
    ["open with a closing time", clarification({ closed_at: "2026-11-25T16:00:00Z" })],
    ["an unknown reason", clarification({ reason: "fraud" })],
    ["an unknown status", clarification({ status: "escalated" })],
    ["an unknown step", clarification({ history: [{ status: "frozen", at: "2026-11-24T20:00:00Z" }] })],
    ["a step without its time", clarification({ history: [{ status: "open", at: "yesterday" }] })],
    ["a description past 500 characters", clarification({ description: "x".repeat(501) })],
    ["case number zero", clarification({ number: 0 })],
    ["no history list", clarification({ history: null })],
  ];
  for (const [what, raw] of cases) assert.equal(parseClarification(raw), null, what);
  assert.equal(parseSpeiClarifications({ clarifications: [clarification(), clarification()] }), null);
  assert.equal(
    parseSpeiClarifications({ clarifications: [clarification(), { ...clarification(), number: "3" }] }),
    null,
  );
});

test("the page's address names a tab and a transfer; anything else is ignored", () => {
  assert.equal(transfersPath(), "/dashboard/transfers");
  assert.equal(transfersPath({ transfer: id(1) }), `/dashboard/transfers?transfer=${id(1)}`);
  assert.equal(transfersPath({ tab: "clarifications" }), "/dashboard/transfers?tab=clarifications");
  assert.equal(transfersPath({ tab: "transfers", transfer: null }), "/dashboard/transfers");
  assert.deepEqual(readTransfersQuery(`?tab=clarifications&transfer=${id(1)}`), {
    tab: "clarifications",
    transfer: id(1),
  });
  assert.deepEqual(readTransfersQuery(`?transfer=${id(1).toUpperCase()}`), { tab: "transfers", transfer: id(1) });
  assert.deepEqual(readTransfersQuery("?tab=other&transfer=../../admin"), { tab: "transfers", transfer: null });
  assert.deepEqual(readTransfersQuery(""), { tab: "transfers", transfer: null });
});

test("a transfer is found by its id, by the ledger movement it made, or by its refund", () => {
  const order = parsed(returnedOrder());
  assert.equal(transferMatches(order, id(4)), true);
  assert.equal(transferMatches(order, id(11)), true);
  assert.equal(transferMatches(order, id(14).toUpperCase()), true);
  assert.equal(transferMatches(order, id(99)), false);
});

test("SPEI movements of the ledger: credited deposits, refunds and order charges", () => {
  assert.equal(isSpeiMovement({ tipo: "DEPOSITO", clave_idempotencia: "spei:40012|BBVA1|2026-11-23" }), true);
  assert.equal(isSpeiMovement({ tipo: "DEPOSITO", clave_idempotencia: `spei-reverso:${id(4)}` }), true);
  assert.equal(isSpeiMovement({ tipo: "RETIRO", clave_idempotencia: `cli:${id(9)}:withdraw-1` }), true);
  assert.equal(isSpeiMovement({ tipo: "RETIRO", clave_idempotencia: `spei-correccion:${id(6)}` }), false);
  assert.equal(isSpeiMovement({ tipo: "DEPOSITO", clave_idempotencia: "mutuo:1" }), false);
  assert.equal(isSpeiMovement({ tipo: "COMPRA_MERCADO", clave_idempotencia: "spei:1" }), false);
});

test("the timeline of an order: initiated, sent to Banxico via STP, settled, and returned when it came back", () => {
  const steps = (raw: Raw) => transferTimeline(parsed(raw)).map((s) => `${s.key}:${s.state}`);
  assert.deepEqual(steps(pending()), ["initiated:done", "sent:current", "settled:upcoming"]);
  assert.deepEqual(steps(pending({ stage: "sent", sent_at: "2026-11-24T18:00:05Z" })), [
    "initiated:done",
    "sent:done",
    "settled:current",
  ]);
  assert.deepEqual(steps(withdrawal()), ["initiated:done", "sent:done", "settled:done"]);
  assert.deepEqual(steps(returnedOrder()), ["initiated:done", "sent:done", "settled:done", "returned:returned"]);
  assert.deepEqual(steps(rejectedOrder()), ["initiated:done", "sent:skipped", "not_completed:returned"]);
  assert.deepEqual(steps(rejectedOrder({ sent_at: "2026-11-24T18:00:05Z" })), [
    "initiated:done",
    "sent:done",
    "not_completed:returned",
  ]);

  const returned = transferTimeline(parsed(returnedOrder()));
  assert.equal(returned[3]?.at, "2026-11-24T19:30:00Z");
  assert.match(returned[3]?.detail ?? "", /account is closed\. The full amount went back to your available cash\./);
  const settled = transferTimeline(parsed(withdrawal()));
  assert.deepEqual(
    settled.map((s) => s.at),
    ["2026-11-24T18:00:00Z", "2026-11-24T18:00:05Z", "2026-11-24T18:00:09Z"],
  );
  assert.equal(settled[2]?.detail, "BBVA México received it.");
});

test("the timeline of a deposit: sent by the other bank, received via STP, credited or sent back", () => {
  const credited = transferTimeline(parsed(deposit()));
  assert.deepEqual(
    credited.map((s) => [s.key, s.state, s.at, s.date]),
    [
      ["initiated", "done", null, "2026-11-23"],
      ["sent", "done", "2026-11-23T15:00:00Z", null],
      ["settled", "done", "2026-11-23T15:00:00Z", null],
    ],
  );
  assert.equal(credited[0]?.detail, "BBVA México sent it by SPEI.");
  const back = transferTimeline(parsed(returnedDeposit()));
  assert.deepEqual(
    back.map((s) => `${s.key}:${s.state}`),
    ["initiated:done", "sent:done", "returned:returned"],
  );
  // No cause, ever: the account "could not receive it".
  assert.equal(
    back[2]?.detail,
    "Your account could not receive it, so it went back to the sending bank. Nothing was credited.",
  );
  assert.equal(transferTimeline(parsed(demoDeposit()))[0]?.detail, "A simulated deposit: no bank sent it.");
});

test("the words for a transfer", () => {
  assert.equal(transferTitle(parsed(withdrawal())), "Withdrawal to BBVA México •••• 1234");
  assert.equal(transferTitle(parsed(deposit())), "Deposit from Juan Pérez");
  assert.equal(transferTitle(parsed(demoDeposit())), "Demo deposit");
  assert.equal(
    transferTitle(
      parsed(pending({ kind: "payment", counterparty: { name: "ACME SA", bank: BBVA, account_last4: "5555" } })),
    ),
    "Payment to ACME SA",
  );
  assert.equal(
    counterpartyLabel(
      parsed(deposit({ counterparty: { name: null, bank: { code: "137", name: null }, account_last4: null } })),
    ),
    "Bank 137",
  );
  assert.equal(
    counterpartyLabel(
      parsed(withdrawal({ counterparty: { name: null, bank: { code: null, name: null }, account_last4: null } })),
    ),
    "Your linked account",
  );
  assert.deepEqual(
    [withdrawal(), pending(), returnedOrder(), rejectedOrder(), returnedDeposit()].map((r) =>
      transferStatusLabel(parsed(r)),
    ),
    ["Settled", "Processing", "Returned", "Not completed", "Returned"],
  );
  assert.equal(formatOperationDate("2026-11-24"), "Nov 24, 2026");
  assert.equal(returnReasonText("account_not_found"), "The receiving bank says the account does not exist.");
  assert.equal(returnReasonText(null), "The receiving bank sent it back.");
});

test("the CEP: what Banxico asks for, or why there is none to look up", () => {
  assert.equal(cepNotice(parsed(withdrawal())), null);
  assert.equal(cepNotice(parsed(pending())), "Banxico issues the CEP once the transfer settles.");
  assert.equal(cepNotice(parsed(rejectedOrder())), "It never settled, so Banxico has no CEP for it.");
  assert.equal(
    cepNotice(parsed(returnedDeposit())),
    "It was never credited to your account, so there is no CEP for it.",
  );
  assert.match(cepNotice(parsed(demoDeposit())) ?? "", /^A simulated transfer: Banxico has no CEP for it\./);
  assert.match(
    cepNotice(parsed(withCep(withdrawal({ operation_date: null }), { available: false, operation_date: null }))) ?? "",
    /operation date yet/,
  );
  assert.equal(
    cepDetailsText(parsed(withdrawal()).cep!),
    [
      "Operation date: 2026-11-24",
      "Tracking key: SC20261124000001",
      "Sender: STP (646)",
      "Receiver: BBVA México (012)",
      "Beneficiary account: •••• 1234",
      "Amount: 1250.50 MXN",
    ].join("\n"),
  );
  // The API's amounts carry 8 decimals; Banxico's form takes them to the cent, without separators.
  assert.deepEqual(["1250.50000000", "1234567.80000000", "0.01000000"].map(cepAmount), [
    "1250.50",
    "1234567.80",
    "0.01",
  ]);
  const own = cepDetailsText(
    parsed(deposit({ cep: { ...(deposit().cep as Raw), receiver_bank: { code: "646", name: null } } })).cep!,
  );
  assert.match(own, /Receiver: Bank 646\n/);
  assert.match(own, new RegExp(`Beneficiary account: ${OWN_CLABE}\n`));
});

test("one active clarification per transfer, within 90 days, at most 10 open", () => {
  const now = Date.parse("2026-11-25T12:00:00Z");
  const settled = parsed(withdrawal());
  assert.equal(clarificationBlock(settled, 0, now), null);
  const open = { ...settled, clarification: { clarification_id: id(21), number: 3, status: "in_review" as const } };
  assert.deepEqual(clarificationBlock(open, 1, now), { kind: "active", number: 3 });
  const closed = { ...settled, clarification: { clarification_id: id(21), number: 3, status: "declined" as const } };
  assert.equal(clarificationBlock(closed, 0, now), null);
  assert.deepEqual(clarificationBlock(settled, 10, now), { kind: "limit" });
  const old = { ...settled, initiated_at: "2026-08-26T11:59:00Z" };
  assert.deepEqual(clarificationBlock(old, 0, now), { kind: "expired" });
  assert.equal(clarificationBlock({ ...settled, initiated_at: "2026-08-27T12:00:00Z" }, 0, now), null);
  assert.equal(
    clarificationBlockText({ kind: "active", number: 3 }),
    "Clarification #3 is already open for this transfer. Follow it in Clarifications.",
  );
  assert.equal(
    activeClarifications([
      parseClarification(clarification())!,
      parseClarification(clarification({ clarification_id: id(22), ...RESOLVED }))!,
    ]),
    1,
  );
});

test("the reasons offered fit where the transfer is", () => {
  assert.deepEqual(reasonsFor(parsed(pending())), ["delayed", "other"]);
  assert.deepEqual(reasonsFor(parsed(withdrawal())), ["not_received", "wrong_amount", "other"]);
  assert.deepEqual(reasonsFor(parsed(returnedOrder())), ["returned", "other"]);
  assert.deepEqual(reasonsFor(parsed(rejectedOrder())), ["returned", "other"]);
  assert.deepEqual(reasonsFor(parsed(deposit())), ["wrong_amount", "not_recognized", "other"]);
  assert.deepEqual(reasonsFor(parsed(returnedDeposit())), ["returned", "other"]);
});

test("the description is cleaned as the backend wants it, and counted as it counts", () => {
  assert.equal(cleanDescription("  Line one\r\nline\ttwo\u0007\r  "), "Line one\nline two");
  assert.equal(characterCount("𝔸𝔸"), 2);
  assert.equal(clarificationProblem(null, ""), "Choose what happened.");
  assert.equal(clarificationProblem("delayed", ""), null);
  assert.equal(clarificationProblem("other", "  short  "), "Tell us what happened, in at least 10 characters.");
  assert.equal(clarificationProblem("other", "It arrived twice."), null);
  assert.equal(clarificationProblem("wrong_amount", "𝔸".repeat(500)), null);
  assert.equal(clarificationProblem("wrong_amount", "𝔸".repeat(501)), "Keep the description to 500 characters.");
});

test("the request: no description when there is none; one key per set of terms", () => {
  const transfer = parsed(withdrawal());
  assert.deepEqual(clarificationRequest(transfer, "not_received", "   ", "k1"), {
    transfer_id: id(1),
    direction: "outgoing",
    reason: "not_received",
    idempotency_key: "k1",
  });
  assert.deepEqual(clarificationRequest(parsed(deposit()), "other", " Two\r\nlines here ", "k2"), {
    transfer_id: id(2),
    direction: "incoming",
    reason: "other",
    description: "Two\nlines here",
    idempotency_key: "k2",
  });
  assert.equal(
    clarificationSignature(id(1), "other", "Not there yet "),
    clarificationSignature(id(1), "other", " Not there yet"),
  );
  assert.notEqual(
    clarificationSignature(id(1), "other", "Not there yet"),
    clarificationSignature(id(1), "delayed", "Not there yet"),
  );
  assert.notEqual(clarificationSignature(id(1), "delayed", ""), clarificationSignature(id(2), "delayed", ""));
});

test("no wording would tip off a client under review", () => {
  // Nothing on a client screen may use the words of an AML hold (e2e/aml.mjs TIP_OFF).
  const TIP_OFF =
    /\b(compliance hold|compliance review|frozen|freeze|restrict\w*|AML|PLD|suspicious|alert|congel\w*|preventiv\w*|UIF|investigat\w*|risk)\b/i;
  const all = [
    withdrawal(),
    deposit(),
    pending(),
    returnedOrder(),
    rejectedOrder(),
    returnedDeposit(),
    demoDeposit(),
  ].map(parsed);
  const words = [
    ...Object.values(KIND_LABELS),
    ...Object.values(CLARIFICATION_STATUS_LABELS),
    ...Object.values(STEP_LABELS),
    ...Object.values(REASON_LABELS),
    ...CLARIFICATION_REASONS.map((r) => REASON_LABELS[r]),
    ...(["open", "in_review", "resolved", "declined"] as const).map(clarificationStatusText),
    ...(["account_not_found", "account_blocked", "account_closed", "missing_information", "other"] as const).map(
      returnReasonText,
    ),
    ...all.flatMap((t) => [transferTitle(t), transferStatusLabel(t), cepNotice(t) ?? ""]),
    ...all.flatMap((t) => transferTimeline(t).flatMap((s) => [s.title, s.detail])),
    clarificationBlockText({ kind: "active", number: 1 }),
    clarificationBlockText({ kind: "expired" }),
    clarificationBlockText({ kind: "limit" }),
    clarificationProblem(null, "") ?? "",
    clarificationProblem("other", "") ?? "",
    clarificationProblem("other", "x".repeat(501)) ?? "",
  ];
  for (const sentence of words) assert.doesNotMatch(sentence, TIP_OFF, sentence);
});
