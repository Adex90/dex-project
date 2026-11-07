// Compiles every circuit in ./ with circom (path to the binary in $CIRCOM, default `circom`).
import { execFileSync } from "node:child_process";
import { mkdirSync } from "node:fs";

const CIRCOM = process.env.CIRCOM ?? "circom";
const targets = [
  ["joinsplit/joinsplit.circom", "joinsplit"],
  ["zkdeco_oracle/zkdeco_oracle.circom", "zkdeco_oracle"],
];
for (const [src, name] of targets) {
  mkdirSync(`build/${name}`, { recursive: true });
  console.log(`compiling ${src}`);
  execFileSync(CIRCOM, [src, "--r1cs", "--wasm", "--sym", "-l", "node_modules", "-o", `build/${name}`], { stdio: "inherit" });
}
