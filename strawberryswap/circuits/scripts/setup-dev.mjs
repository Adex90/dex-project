// DEV-ONLY Groth16 setup. ⚠ NOT A SECURE CEREMONY: one local contributor, entropy from this machine.
// Anyone holding the toxic waste of this setup can forge proofs. It exists only to generate test fixtures.
// Production keys MUST come from a public multi-party phase-2 ceremony on top of a trusted phase-1 ptau.
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
import { randomBytes } from "node:crypto";

const SNARKJS = "node_modules/.bin/snarkjs";
const POWER = process.env.PTAU_POWER ?? "15";
const names = process.argv.slice(2);
if (!names.length) throw new Error("usage: setup-dev.mjs <circuit> [<circuit>...]");
const sj = (...a) => execFileSync(SNARKJS, a, { stdio: "inherit" });

mkdirSync("build/ptau", { recursive: true });
const ptau = `build/ptau/dev_${POWER}_final.ptau`;
if (!existsSync(ptau)) {
  sj("powersoftau", "new", "bn128", POWER, `build/ptau/p0.ptau`);
  sj("powersoftau", "contribute", `build/ptau/p0.ptau`, `build/ptau/p1.ptau`, "--name=dev", `-e=${randomBytes(32).toString("hex")}`);
  sj("powersoftau", "prepare", "phase2", `build/ptau/p1.ptau`, ptau);
}
for (const n of names) {
  const d = `build/${n}`;
  sj("groth16", "setup", `${d}/${n}.r1cs`, ptau, `${d}/${n}_0.zkey`);
  sj("zkey", "contribute", `${d}/${n}_0.zkey`, `${d}/${n}.zkey`, "--name=dev", `-e=${randomBytes(32).toString("hex")}`);
  sj("zkey", "export", "verificationkey", `${d}/${n}.zkey`, `${d}/${n}.vkey.json`);
  sj("zkey", "export", "solidityverifier", `${d}/${n}.zkey`, `${d}/${n}.Verifier.sol`);
}
