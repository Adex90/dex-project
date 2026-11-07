// Post-build gate: fail if the export is not a self-contained static site suitable for IPFS.
import { readdirSync, readFileSync, statSync, existsSync } from "node:fs";
import { join } from "node:path";

const OUT = "out";
if (!existsSync(join(OUT, "index.html"))) throw new Error("out/index.html missing — run `npm run build` first");

const files = [];
(function walk(d) {
  for (const f of readdirSync(d)) {
    const p = join(d, f);
    statSync(p).isDirectory() ? walk(p) : files.push(p);
  }
})(OUT);

const problems = [];
// third-party origins that must NOT be fetched at runtime (the site must survive with only IPFS up)
const forbidden = /https?:\/\/(?:fonts\.(?:googleapis|gstatic)\.com|cdn\.|unpkg\.com|cdnjs\.|www\.google-analytics|www\.googletagmanager)/;
for (const f of files.filter((f) => /\.(html|js|css)$/.test(f))) {
  const s = readFileSync(f, "utf8");
  const m = s.match(forbidden);
  if (m) problems.push(`${f}: references ${m[0]}`);
}
if (!existsSync(join(OUT, "trade", "index.html"))) problems.push("trade/index.html missing");
if (files.some((f) => f.includes("/api/"))) problems.push("api routes present in export");

if (problems.length) {
  console.error("STATIC EXPORT CHECK FAILED\n" + problems.join("\n"));
  process.exit(1);
}
console.log(`static export OK: ${files.length} files, no third-party origins`);
