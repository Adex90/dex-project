import type { Hex } from "viem";
import type { Groth16Proof } from "snarkjs";

/**
 * snarkjs proof → the 256-byte `abi.encode(uint256[2] a, uint256[2][2] b, uint256[2] c)` the on-chain verifier adapters take.
 * Each pair of pi_b is swapped (Fp2 coefficient order), exactly as `snarkjs zkey export soliditycalldata` does.
 */
export function proofToHex(p: Groth16Proof): Hex {
  const words = [p.pi_a[0], p.pi_a[1], p.pi_b[0]?.[1], p.pi_b[0]?.[0], p.pi_b[1]?.[1], p.pi_b[1]?.[0], p.pi_c[0], p.pi_c[1]];
  let out = "0x";
  for (const w of words) {
    if (w === undefined || !/^\d+$/.test(w)) throw new Error("malformed proof");
    out += BigInt(w).toString(16).padStart(64, "0");
  }
  return out as Hex;
}
