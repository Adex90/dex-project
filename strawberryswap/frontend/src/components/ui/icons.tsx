import type { SVGProps } from "react";

/** Inline stroke icons (24×24, currentColor). Inline on purpose: the static export may not fetch an icon CDN. */
type P = SVGProps<SVGSVGElement>;
const base = (p: P): P => ({
  width: 20,
  height: 20,
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 2,
  strokeLinecap: "round",
  strokeLinejoin: "round",
  "aria-hidden": true,
  focusable: false,
  ...p,
});

export const IconTrade = (p: P) => (
  <svg {...base(p)}><path d="M7 4v16M7 4 3.5 7.5M7 4l3.5 3.5M17 20V4M17 20l-3.5-3.5M17 20l3.5-3.5" /></svg>
);
export const IconStocks = (p: P) => (
  <svg {...base(p)}><path d="M3 17l5.5-5.5 4 4L21 7" /><path d="M15 7h6v6" /></svg>
);
export const IconEarn = (p: P) => (
  <svg {...base(p)}><ellipse cx="12" cy="6.5" rx="7" ry="3" /><path d="M5 6.5v5c0 1.66 3.13 3 7 3s7-1.34 7-3v-5" /><path d="M5 11.5v5c0 1.66 3.13 3 7 3s7-1.34 7-3v-5" /></svg>
);
export const IconPortfolio = (p: P) => (
  <svg {...base(p)}><path d="M21 12a9 9 0 1 1-9-9v9h9Z" /><path d="M15 3.5A9 9 0 0 1 20.5 9H15V3.5Z" /></svg>
);
export const IconFlip = (p: P) => (
  <svg {...base(p)}><path d="M8 4v15m0 0-4-4m4 4 4-4M16 20V5m0 0 4 4m-4-4-4 4" /></svg>
);
export const IconChevron = (p: P) => (
  <svg {...base(p)}><path d="m6 9 6 6 6-6" /></svg>
);
export const IconSettings = (p: P) => (
  <svg {...base(p)}>
    <path d="M4 7h10M18 7h2M4 17h2M10 17h10" />
    <circle cx="16" cy="7" r="2" />
    <circle cx="8" cy="17" r="2" />
  </svg>
);
export const IconShield = (p: P) => (
  <svg {...base(p)}><path d="M12 3 4.5 6v5.5c0 4.6 3.2 8.4 7.5 9.5 4.3-1.1 7.5-4.9 7.5-9.5V6L12 3Z" /><path d="m9 12 2 2 4-4" /></svg>
);
export const IconLock = (p: P) => (
  <svg {...base(p)}><rect x="5" y="10.5" width="14" height="10" rx="3" /><path d="M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5" /></svg>
);
export const IconWallet = (p: P) => (
  <svg {...base(p)}><path d="M4 7.5A2.5 2.5 0 0 1 6.5 5H18v3" /><rect x="4" y="8" width="16" height="11" rx="2.5" /><circle cx="16" cy="13.5" r="1.2" fill="currentColor" /></svg>
);
export const IconZap = (p: P) => (
  <svg {...base(p)}><path d="M13 2 4.5 13.5H12L11 22l8.5-11.5H12L13 2Z" /></svg>
);
export const IconRefresh = (p: P) => (
  <svg {...base(p)}><path d="M20 11a8 8 0 0 0-14.6-4.5M4 4v3h3M4 13a8 8 0 0 0 14.6 4.5M20 20v-3h-3" /></svg>
);
export const IconVault = (p: P) => (
  <svg {...base(p)}><rect x="3.5" y="4.5" width="17" height="15" rx="3" /><circle cx="12" cy="12" r="3.5" /><path d="M12 8.5V7M12 17v-1.5M15.5 12H17M7 12h1.5" /></svg>
);
export const IconBank = (p: P) => (
  <svg {...base(p)}><path d="M3 9.5 12 4l9 5.5M5 10v7M9.5 10v7M14.5 10v7M19 10v7M3.5 20h17" /></svg>
);
export const IconClock = (p: P) => (
  <svg {...base(p)}><circle cx="12" cy="12" r="8.5" /><path d="M12 7.5V12l3 2" /></svg>
);
export const IconInfo = (p: P) => (
  <svg {...base(p)}><circle cx="12" cy="12" r="9" /><path d="M12 11v5M12 8h.01" /></svg>
);
