"use client";

import { useId } from "react";

/**
 * "Berri", the Cyber-Strawberry: an original mascot drawn as inline SVG (no image request, so it ships inside the
 * static export). A glossy strawberry with a lit visor, mint "happy" eyes, circuit-trace seeds and a fuchsia antenna.
 * The radial body gradient, rim light and highlight give it its 3D look; `float` adds a gentle bob (off under
 * prefers-reduced-motion).
 */
export function Mascot({ className = "h-10 w-10", float = false, title }: { className?: string; float?: boolean; title?: string }) {
  const id = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const u = (n: string) => `${id}-${n}`;
  return (
    <svg
      viewBox="0 0 120 124"
      className={`${className} ${float ? "animate-float" : ""}`}
      role={title ? "img" : undefined}
      aria-label={title}
      aria-hidden={title ? undefined : true}
      focusable={false}
    >
      <defs>
        <radialGradient id={u("body")} cx="34%" cy="30%" r="78%">
          <stop offset="0%" stopColor="#ff9dbd" />
          <stop offset="32%" stopColor="#ff2a62" />
          <stop offset="72%" stopColor="#e0003d" />
          <stop offset="100%" stopColor="#7d0026" />
        </radialGradient>
        <linearGradient id={u("leaf")} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#7dffc2" />
          <stop offset="100%" stopColor="#00b862" />
        </linearGradient>
        <linearGradient id={u("visor")} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor="#35165f" />
          <stop offset="100%" stopColor="#0f0520" />
        </linearGradient>
        <filter id={u("glow")} x="-50%" y="-50%" width="200%" height="200%">
          <feGaussianBlur stdDeviation="1.6" result="b" />
          <feMerge>
            <feMergeNode in="b" />
            <feMergeNode in="SourceGraphic" />
          </feMerge>
        </filter>
        <filter id={u("soft")} x="-50%" y="-200%" width="200%" height="500%">
          <feGaussianBlur stdDeviation="3" />
        </filter>
      </defs>

      {/* floor glow */}
      <ellipse cx="60" cy="118" rx="28" ry="4.5" fill="#ff2a85" opacity="0.45" filter={`url(#${u("soft")})`} />

      {/* antenna */}
      <path d="M60 22 V11" stroke="#00b862" strokeWidth="2.6" strokeLinecap="round" />
      <circle cx="60" cy="8" r="3.8" fill="#ff2a85" filter={`url(#${u("glow")})`} />

      {/* body + rim light */}
      <path d="M60 34C79 29 101 37 101 59C101 82 81 106 60 114C39 106 19 82 19 59C19 37 41 29 60 34Z" fill={`url(#${u("body")})`} />
      <path d="M60 34C79 29 101 37 101 59C101 82 81 106 60 114C39 106 19 82 19 59C19 37 41 29 60 34Z" fill="none" stroke="#ffb3d3" strokeOpacity="0.45" strokeWidth="1" />

      {/* calyx */}
      <g fill={`url(#${u("leaf")})`}>
        <path d="M60 38C50 26 36 25 28 33C39 37 49 40 60 41Z" />
        <path d="M60 38C70 26 84 25 92 33C81 37 71 40 60 41Z" />
        <path d="M60 39C47 33 41 39 39 45C47 43 53 42 60 40Z" />
        <path d="M60 39C73 33 79 39 81 45C73 43 67 42 60 40Z" />
        <path d="M60 40C53 30 53 22 60 16C67 22 67 30 60 40Z" />
      </g>

      {/* gloss */}
      <ellipse cx="40" cy="50" rx="11" ry="6.5" fill="#ffffff" opacity="0.32" transform="rotate(-28 40 50)" />
      <ellipse cx="31.5" cy="61" rx="2.6" ry="1.8" fill="#ffffff" opacity="0.3" />

      {/* visor + eyes */}
      <rect x="32" y="54" width="56" height="21" rx="10.5" fill={`url(#${u("visor")})`} stroke="#ff2a85" strokeWidth="1.6" />
      <path d="M39 58.5H62" stroke="#f7eefe" strokeOpacity="0.16" strokeWidth="2" strokeLinecap="round" />
      <g stroke="#00ff88" strokeWidth="3" strokeLinecap="round" fill="none" filter={`url(#${u("glow")})`}>
        <path d="M42.5 68Q47.5 61.5 52.5 68" />
        <path d="M67.5 68Q72.5 61.5 77.5 68" />
      </g>

      {/* cheeks + smile */}
      <ellipse cx="37" cy="82" rx="5.5" ry="3.2" fill="#ff9ecf" opacity="0.55" />
      <ellipse cx="83" cy="82" rx="5.5" ry="3.2" fill="#ff9ecf" opacity="0.55" />
      <path d="M54 84Q60 90 66 84" stroke="#5a0420" strokeWidth="2.6" strokeLinecap="round" fill="none" />

      {/* circuit-trace seeds */}
      <g stroke="#00ff88" strokeOpacity="0.4" strokeWidth="1" fill="none">
        <path d="M40 95L46 99L52 103" />
        <path d="M80 95L74 99L68 103" />
      </g>
      <g fill="#00ff88">
        <circle cx="40" cy="95" r="1.9" opacity="0.9" />
        <circle cx="52" cy="103" r="1.9" opacity="0.9" />
        <circle cx="68" cy="103" r="1.9" opacity="0.9" />
        <circle cx="80" cy="95" r="1.9" opacity="0.9" />
        <circle cx="60" cy="99" r="1.6" opacity="0.75" />
        <circle cx="79" cy="46.5" r="1.5" opacity="0.7" />
        <circle cx="88" cy="54" r="1.5" opacity="0.7" />
      </g>
    </svg>
  );
}

/** Wordmark + mascot, the header's brand slot. */
export function Logo({ compact = false }: { compact?: boolean }) {
  return (
    <span className="flex items-center gap-2">
      <Mascot className="h-10 w-10 shrink-0 drop-shadow-[0_0_12px_rgba(255,42,133,0.45)]" />
      <span className={`text-xl font-extrabold tracking-tight ${compact ? "hidden sm:inline" : ""}`}>
        <span className="text-gradient">Strawberry</span>
        <span className="text-ink">Swap</span>
      </span>
    </span>
  );
}
