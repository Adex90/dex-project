import { poseidon1, poseidon2, poseidon3, poseidon4, poseidon5 } from "poseidon-lite";
import { inField } from "./field";

/**
 * Poseidon over BN254, bit-identical to circom's `Poseidon(n)` (verified against circomlibjs and the Go
 * implementation by golden vectors). Pure bigint: no wasm, no Node APIs, ~0.2 ms per hash.
 */
export function H(...xs: bigint[]): bigint {
  for (const x of xs) if (!inField(x)) throw new Error("poseidon input is not a reduced field element");
  switch (xs.length) {
    case 1: return poseidon1(xs);
    case 2: return poseidon2(xs);
    case 3: return poseidon3(xs);
    case 4: return poseidon4(xs);
    case 5: return poseidon5(xs);
    default: throw new Error(`unsupported poseidon arity ${xs.length}`);
  }
}
