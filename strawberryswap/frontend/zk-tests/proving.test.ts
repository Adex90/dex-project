// REAL proofs: the browser engine drives snarkjs with the actual joinsplit wasm/zkey and every proof is verified
// against the actual verification key. Skipped (loudly) if the circuit artifacts have not been built.
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { after, test } from "node:test";
import * as snarkjs from "snarkjs";
import type { Hex } from "viem";
import { ZkEngine, type Groth16Prover } from "../src/zk/engine";
import { proofToHex } from "../src/zk/format";
import { ALICE, FakeChain, SIG_A, U, sealLikeAutomator } from "./helpers";
import { buildJoinSplitWitness } from "../src/zk/joinsplit";
import { makeNote, swapOutputCommitment } from "../src/zk/notes";
import { NoteTree } from "../src/zk/tree";

const dir = new URL("../../circuits/build/joinsplit/", import.meta.url);
const wasm = new URL("joinsplit_js/joinsplit.wasm", dir).pathname;
const zkey = new URL("joinsplit.zkey", dir).pathname;
const vkeyPath = new URL("joinsplit.vkey.json", dir).pathname;
const have = existsSync(wasm) && existsSync(zkey) && existsSync(vkeyPath);
if (!have) console.warn("SKIPPED: circuits/build/joinsplit artifacts missing (run `npm run compile && npm run setup:dev` in circuits/)");

class NodeProver implements Groth16Prover {
  readonly verified: { pub: string[]; ok: boolean }[] = [];
  async prove(input: Record<string, unknown>, expected: bigint[]): Promise<Hex> {
    const { proof, publicSignals } = await snarkjs.groth16.fullProve(input, wasm, zkey);
    assert.deepEqual(publicSignals, expected.map(String), "prover public signals must equal what the engine expects");
    const vkey: unknown = JSON.parse(readFileSync(vkeyPath, "utf8"));
    const ok = await snarkjs.groth16.verify(vkey, publicSignals, proof);
    this.verified.push({ pub: publicSignals, ok });
    return proofToHex(proof);
  }
}

test("deposit → intent → withdraw: every browser-engine proof verifies against the circuit's verification key", { skip: !have, timeout: 240_000 }, async () => {
  const prover = new NodeProver();
  const eng = new ZkEngine(prover);
  eng.unlock(SIG_A);
  const chain = new FakeChain();

  const dep = await eng.proveDeposit({ assetId: 0n, amount: 1000n * U, caller: ALICE });
  assert.match(dep.js.proof, /^0x[0-9a-f]{512}$/, "256-byte proof");
  chain.pushPair(dep.js.commitments, dep.js.encryptedNotes);
  await eng.addLeaves(chain.leaves);

  // spends the deposit note: real Merkle membership + nullifier + value conservation (400 -> intent, 600 change)
  const intent = await eng.proveIntent({ dir: 0, amount: 400n * U, minOut: 790n * U, deadline: 1_900_000_000n });
  chain.pushPair(intent.js.commitments, intent.js.encryptedNotes);
  await eng.addLeaves(chain.leaves);

  // pretend the Automator settled it: the sealed output becomes a spendable note, which we withdraw
  const pub = eng.state().pubKey as bigint;
  const terms = { dir: 0 as const, value: 400n * U, destPubKey: pub, refundPubKey: pub, destBlinding: BigInt(intent.opening.destBlinding), minOut: 790n * U, deadline: 1_900_000_000n };
  chain.push(swapOutputCommitment(terms, 799n * U), await sealLikeAutomator(terms.destBlinding, 1, 799n * U));
  const st = await eng.addLeaves(chain.leaves);
  const out = st.notes.find((n) => n.assetId === 1n);
  assert.ok(out && out.value === 799n * U);
  await eng.proveWithdraw({ commitment: out.commitment, recipient: ALICE });

  assert.equal(prover.verified.length, 3);
  assert.ok(prover.verified.every((v) => v.ok), "every proof verifies");

  // A verifier must reject a proof presented with different public data (here: an inflated deposit amount).
  const vkey: unknown = JSON.parse(readFileSync(vkeyPath, "utf8"));
  const first = prover.verified[0];
  assert.ok(first);
  const tampered = [...first.pub];
  tampered[5] = (BigInt(tampered[5] as string) + 1n).toString(); // publicIn + 1
  const w = buildJoinSplitWitness(new NoteTree(), { assetId: 0n, ins: [undefined, undefined], outs: [makeNote(0n, 5n, 1n, 2n), makeNote(0n, 0n, 1n, 3n)], publicIn: 5n, publicOut: 0n, ext: 7n });
  const { proof, publicSignals } = await snarkjs.groth16.fullProve(w.input, wasm, zkey);
  assert.equal(await snarkjs.groth16.verify(vkey, publicSignals, proof), true);
  assert.equal(await snarkjs.groth16.verify(vkey, tampered, proof), false, "a proof for other public inputs must not verify");
});

// snarkjs/ffjavascript keep worker threads alive; release them so the Node test process can exit.
after(async () => {
  const curve = (globalThis as unknown as { curve_bn128?: { terminate(): Promise<void> } }).curve_bn128;
  await curve?.terminate();
});
