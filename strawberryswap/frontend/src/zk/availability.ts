import manifestJson from "./manifest.generated.json";
import type { ZkManifest } from "./protocol";

const m = manifestJson as ZkManifest;
/** False when the build was made without proving artifacts (scripts/prepare-zk.mjs found none). */
export const proofsAvailable: boolean = m.artifacts !== null;
/** True while the proving key comes from a single-party local setup. */
export const devKeys: boolean = m.devKeys;
export const zkeyBytes: number = m.artifacts?.zkey.bytes ?? 0;
