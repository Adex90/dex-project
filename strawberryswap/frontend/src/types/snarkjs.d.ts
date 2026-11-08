// snarkjs ships no type declarations. Only the surface this app uses is declared (and kept strict).
declare module "snarkjs" {
  type FastFile = string | { type: "mem"; data: Uint8Array };
  export interface Groth16Proof { pi_a: string[]; pi_b: string[][]; pi_c: string[]; protocol: string; curve: string }
  export const groth16: {
    fullProve(input: Record<string, unknown>, wasm: FastFile, zkey: FastFile): Promise<{ proof: Groth16Proof; publicSignals: string[] }>;
    verify(vkey: unknown, publicSignals: string[], proof: Groth16Proof): Promise<boolean>;
    exportSolidityCallData(proof: Groth16Proof, publicSignals: string[]): Promise<string>;
  };
}
