import { test } from "node:test";
import assert from "node:assert/strict";
import { validateAutomatorUrl } from "../src/lib/automator";

test("automator URL policy: https, or http only for onion services and loopback", () => {
  for (const ok of ["https://relay.example", "http://abcdefghijklmnop.onion", "http://xyz.onion:8080/", "http://127.0.0.1:8787", "http://localhost:8787"]) {
    assert.doesNotThrow(() => validateAutomatorUrl(ok), ok);
  }
  for (const bad of ["http://relay.example", "ftp://x.onion", "javascript:alert(1)", "not a url", "http://127.0.0.1.evil.com", "https://user:pw@relay.example"]) {
    assert.throws(() => validateAutomatorUrl(bad), Error, bad);
  }
});
