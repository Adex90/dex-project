// Proves one zkdeco_oracle statement for the oracle relayer (strawberryswap/oracle):
//
//   node scripts/prove-zkdeco.mjs <circuit.wasm> <circuit.zkey> <vkey.json>   < witness.json
//
// The witness arrives on stdin and is never written to disk: it holds the query salt, which hides the instrument on
// chain. The proof is verified against the verification key before it is printed. Output, one JSON line on stdout:
//
//   {"snark":"0x<abi.encode(uint256[2] a, uint256[2][2] b, uint256[2] c)>","publicSignals":["…", …]}
import * as snarkjs from "snarkjs";
import { readFileSync } from "node:fs";

const [wasm, zkey, vkeyPath] = process.argv.slice(2);
if (!wasm || !zkey || !vkeyPath) {
  console.error("usage: prove-zkdeco.mjs <circuit.wasm> <circuit.zkey> <vkey.json> < witness.json");
  process.exit(64);
}
const input = JSON.parse(readFileSync(0, "utf8"));
const vkey = JSON.parse(readFileSync(vkeyPath, "utf8"));
const { proof, publicSignals } = await snarkjs.groth16.fullProve(input, wasm, zkey);
if (!(await snarkjs.groth16.verify(vkey, publicSignals, proof))) {
  console.error("the proof does not verify against the verification key");
  process.exit(2);
}
// exportSolidityCallData swaps each pi_b pair into the Fp2 order the Solidity verifier expects.
const [a, b, c] = JSON.parse(`[${await snarkjs.groth16.exportSolidityCallData(proof, publicSignals)}]`);
const words = [...a, ...b[0], ...b[1], ...c].map((x) => BigInt(x).toString(16).padStart(64, "0"));
process.stdout.write(JSON.stringify({ snark: "0x" + words.join(""), publicSignals }) + "\n");
// snarkjs keeps worker threads alive after proving; exit explicitly.
process.exit(0);
