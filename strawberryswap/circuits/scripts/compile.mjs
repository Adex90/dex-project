// Compiles the circuits with circom (path to the binary in $CIRCOM, default `circom`).
//   node scripts/compile.mjs                      every circuit
//   node scripts/compile.mjs zkdeco_oracle ...    only the named build targets
// Every target is addressable as build/<name>/<name>.{r1cs,sym} (what setup-dev.mjs and the provers expect); the
// witness generator keeps circom's own name (e.g. build/batch_swap_n4/main_n4_js/main_n4.wasm).
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";

const CIRCOM = process.env.CIRCOM ?? "circom";
const targets = [
  ["joinsplit/joinsplit.circom", "joinsplit"],
  ["zkdeco_oracle/zkdeco_oracle.circom", "zkdeco_oracle"],
  ...[2, 4, 8, 16].map((n) => [`batch_swap/main_n${n}.circom`, `batch_swap_n${n}`]),
];
const only = process.argv.slice(2);
for (const [src, name] of targets) {
  if (only.length && !only.includes(name)) continue;
  const out = `build/${name}`;
  mkdirSync(out, { recursive: true });
  console.log(`compiling ${src}`);
  execFileSync(CIRCOM, [src, "--r1cs", "--wasm", "--sym", "-l", "node_modules", "-o", out], { stdio: "inherit" });
  const stem = src.split("/").pop().replace(/\.circom$/, "");
  if (stem !== name) for (const ext of ["r1cs", "sym"]) copyFileSync(`${out}/${stem}.${ext}`, `${out}/${name}.${ext}`);
}
