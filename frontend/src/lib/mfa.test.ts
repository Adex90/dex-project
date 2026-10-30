/** Run with: npm test */
import assert from "node:assert/strict";
import { test } from "node:test";

import {
  PASSWORD_MIN_LENGTH,
  formatSecretForManualEntry,
  isTotpCode,
  newPasswordProblem,
  normalizeTotpInput,
  resetTokenFromHash,
} from "./mfa.ts";

test("an authenticator code keeps its six digits whatever the person typed around them", () => {
  assert.equal(normalizeTotpInput("123 456"), "123456");
  assert.equal(normalizeTotpInput("123-456"), "123456");
  assert.equal(normalizeTotpInput(" 12a3b4c56789"), "123456");
  assert.equal(isTotpCode("123456"), true);
  for (const code of ["12345", "1234567", "12345a", ""]) assert.equal(isTotpCode(code), false, code);
});

test("the reset token is read from the link's fragment, never from the query", () => {
  const token = "q3Jd9Kx0bW1aTkVr0p8yQm5NcXhLWnRkUkZ1T0Z5abc";
  assert.equal(resetTokenFromHash(`#token=${token}`), token);
  assert.equal(resetTokenFromHash(`token=${token}`), token);
  assert.equal(resetTokenFromHash(""), null);
  assert.equal(resetTokenFromHash("#token="), null);
  assert.equal(resetTokenFromHash("#token=short"), null);
  assert.equal(resetTokenFromHash("#token=has spaces and <script>"), null);
});

test("a new password follows the backend policy and must be typed twice", () => {
  const ok = "x".repeat(PASSWORD_MIN_LENGTH);
  assert.equal(newPasswordProblem(ok, ok), null);
  assert.match(newPasswordProblem("short", "short") ?? "", /at least 12/);
  assert.match(newPasswordProblem("x".repeat(129), "x".repeat(129)) ?? "", /at most 128/);
  assert.match(newPasswordProblem(ok, `${ok}!`) ?? "", /do not match/);
  // Length is counted in characters, as the backend does, not in UTF-16 units.
  assert.equal(newPasswordProblem("contraseña-ñ", "contraseña-ñ"), null);
});

test("the manual-entry key is shown in groups of four", () => {
  assert.equal(formatSecretForManualEntry("JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP"), "JBSW Y3DP EHPK 3PXP JBSW Y3DP EHPK 3PXP");
  assert.equal(formatSecretForManualEntry(""), "");
});
