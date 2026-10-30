/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import { EXPIRED_LOGIN_PATH, LOGIN_PATH, guardRedirect, isSessionUsable } from "./session.ts";

const NOW = Date.parse("2026-10-27T12:00:00Z");
const LATER = "2026-10-27T13:00:00Z";
const EARLIER = "2026-10-27T11:00:00Z";

test("a usable session is not redirected", () => {
  assert.equal(isSessionUsable({ token: "t", expiresAt: LATER }, NOW), true);
  assert.equal(guardRedirect({ token: "t", expiresAt: LATER, expired: false }, NOW), null);
});

test("a session that ended by a 401 keeps the expired notice through the layout guard", () => {
  // The API rejected the token before its local expiry: the 401 handler signs
  // out with the expired flag, then the layout sees no token. Both must send
  // the officer to the same URL, with the notice (e2e#3).
  assert.equal(guardRedirect({ token: null, expiresAt: null, expired: true }, NOW), EXPIRED_LOGIN_PATH);
});

test("a locally expired token also shows the notice", () => {
  assert.equal(isSessionUsable({ token: "t", expiresAt: EARLIER }, NOW), false);
  assert.equal(guardRedirect({ token: "t", expiresAt: EARLIER, expired: false }, NOW), EXPIRED_LOGIN_PATH);
});

test("signing out, or never signing in, goes to the plain sign-in page", () => {
  assert.equal(guardRedirect({ token: null, expiresAt: null, expired: false }, NOW), LOGIN_PATH);
  assert.equal(LOGIN_PATH, "/auth/login");
  assert.equal(EXPIRED_LOGIN_PATH, "/auth/login?expired=1");
});
