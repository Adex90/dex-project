import Link from "next/link";

/** Wordmark: a vault door reduced to a square and a full circle (100%). */
export function Brand({ href = "/", compact = false }: { href?: string; compact?: boolean }) {
  return (
    <Link
      href={href}
      // The compact mark is only an icon: the link still needs a name.
      aria-label={compact ? "Synthetic Capital" : undefined}
      className="inline-flex items-center gap-2.5 rounded-md focus-visible:ring-2 focus-visible:ring-accent focus-visible:outline-none"
    >
      <BrandMark className="size-7 text-accent" />
      {compact ? null : (
        <span className="text-[15px] font-semibold tracking-tight text-ink">
          Synthetic <span className="text-accent">Capital</span>
        </span>
      )}
    </Link>
  );
}

/** The mark alone, in the current color: a vault door reduced to a square and a full circle. */
export function BrandMark({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 32 32" className={className} aria-hidden>
      <rect x="2" y="2" width="28" height="28" rx="7" fill="currentColor" opacity="0.14" />
      <rect x="2" y="2" width="28" height="28" rx="7" fill="none" stroke="currentColor" strokeWidth="2" />
      <circle cx="16" cy="16" r="7" fill="none" stroke="currentColor" strokeWidth="2.5" />
      <circle cx="16" cy="16" r="2" fill="currentColor" />
    </svg>
  );
}
