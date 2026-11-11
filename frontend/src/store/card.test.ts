/** Run with: npm test */
import assert from "node:assert/strict";
import { beforeEach, test } from "node:test";

import { CARD_USES, useCardStore } from "./card.ts";

const card = () => useCardStore.getState();

beforeEach(() => card().reset());

test("the card starts active, online, contactless and ATM on, abroad off", () => {
  assert.equal(card().frozen, false);
  assert.deepEqual(card().uses, { online: true, contactless: true, atm: true, abroad: false });
  assert.deepEqual(
    CARD_USES.map((use) => use.key),
    ["online", "contactless", "atm", "abroad"],
  );
});

test("freeze toggles; uses change one at a time; reset brings the defaults back", () => {
  card().toggleFrozen();
  assert.equal(card().frozen, true);
  card().setUse("abroad", true);
  card().setUse("atm", false);
  assert.deepEqual(card().uses, { online: true, contactless: true, atm: false, abroad: true });
  card().toggleFrozen();
  assert.equal(card().frozen, false);
  card().toggleFrozen();
  card().reset();
  assert.equal(card().frozen, false);
  assert.deepEqual(card().uses, { online: true, contactless: true, atm: true, abroad: false });
});
