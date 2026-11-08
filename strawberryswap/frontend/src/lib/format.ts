import { formatUnits, parseUnits } from "viem";

/** Fixed-width-friendly number formatting for 18-decimal chain integers. Never uses floating point on money. */
export function fmt(value: bigint | undefined, decimals = 18, maxFrac = 4): string {
  if (value === undefined) return "—";
  const s = formatUnits(value, decimals);
  const [int = "0", frac = ""] = s.split(".");
  const grouped = int.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  const f = frac.slice(0, maxFrac).replace(/0+$/, "");
  return f ? `${grouped}.${f}` : grouped;
}

/** Parse user text into base units; undefined when empty/invalid/non-positive. */
export function parseAmount(text: string, decimals = 18): bigint | undefined {
  const t = text.trim();
  if (!/^\d*\.?\d*$/.test(t) || t === "" || t === ".") return undefined;
  try {
    const v = parseUnits(t, decimals);
    return v > 0n ? v : undefined;
  } catch {
    return undefined;
  }
}

export function bpsToPct(bps: bigint | undefined): string {
  if (bps === undefined) return "—";
  return `${(Number(bps) / 100).toFixed(2)}%`;
}

export function shortAddr(a: string | undefined): string {
  return a ? `${a.slice(0, 6)}…${a.slice(-4)}` : "—";
}

export function errMsg(e: unknown): string {
  if (e && typeof e === "object") {
    const anyE = e as { shortMessage?: string; message?: string };
    return anyE.shortMessage ?? anyE.message ?? "unknown error";
  }
  return String(e);
}
