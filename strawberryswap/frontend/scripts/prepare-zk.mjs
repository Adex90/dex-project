// Copies the JoinSplit proving artifacts into public/zk/ and writes src/zk/manifest.generated.json with their SHA-256.
// The manifest is compiled INTO the JS bundle, so the site's content hash (its IPFS CID) commits to the exact proving
// key, witness generator and verification key, and the worker refuses any bytes that do not match.
//
// Source, in order: $ZK_SRC; ../circuits/build/joinsplit (a fresh `npm run compile && npm run setup:dev` in circuits/,
// copied into public/zk/); else the artifacts committed in public/zk/, which match the join-split verifier the testnet
// deploys (contracts/test/fixtures/JoinSplitGroth16Verifier.dev.sol), so a fresh checkout builds with proving.
// If none is there the build still succeeds with an EMPTY manifest and the UI reports that private swaps are
// unavailable (set REQUIRE_ZK=1 in CI to make a missing artifact fatal).
import { copyFileSync, existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");
const src = process.env.ZK_SRC ?? join(root, "../circuits/build/joinsplit");
const files = {
  wasm: [join(src, "joinsplit_js/joinsplit.wasm"), "zk/joinsplit.wasm"],
  zkey: [join(src, "joinsplit.zkey"), "zk/joinsplit.zkey"],
  vkey: [join(src, "joinsplit.vkey.json"), "zk/joinsplit.vkey.json"],
};
const manifestPath = join(root, "src/zk/manifest.generated.json");

const present = (set) => Object.values(set).every(([from]) => existsSync(from));
let source = src;
if (!present(files) && !process.env.ZK_SRC) {
  const committed = Object.fromEntries(Object.entries(files).map(([name, [, rel]]) => [name, [join(root, "public", rel), rel]]));
  if (present(committed)) {
    Object.assign(files, committed);
    source = "public/zk (committed)";
  }
}

if (!present(files)) {
  const msg = `prepare-zk: proving artifacts not found in ${src}`;
  if (process.env.REQUIRE_ZK) throw new Error(msg);
  console.warn(`${msg}\n  -> building WITHOUT in-browser proving (private swaps will be disabled in the UI)`);
  writeFileSync(manifestPath, JSON.stringify({ circuit: "joinsplit", devKeys: true, artifacts: null }, null, 2) + "\n");
  process.exit(0);
}

mkdirSync(join(root, "public/zk"), { recursive: true });
const artifacts = {};
for (const [name, [from, rel]] of Object.entries(files)) {
  const to = join(root, "public", rel);
  if (from !== to) copyFileSync(from, to);
  artifacts[name] = { path: rel, sha256: createHash("sha256").update(readFileSync(from)).digest("hex"), bytes: statSync(from).size };
}
// DEV keys come from a single-party local setup (circuits/scripts/setup-dev.mjs). A production build must supply
// ceremony keys and set ZK_PRODUCTION_KEYS=1, which flips this flag and removes the UI warning.
const devKeys = !process.env.ZK_PRODUCTION_KEYS;
writeFileSync(manifestPath, JSON.stringify({ circuit: "joinsplit", devKeys, artifacts }, null, 2) + "\n");
console.log(`prepare-zk: pinned ${Object.keys(artifacts).join(", ")} from ${source} (${(artifacts.zkey.bytes / 1e6).toFixed(1)} MB zkey)${devKeys ? "  [DEV KEYS]" : ""}`);
