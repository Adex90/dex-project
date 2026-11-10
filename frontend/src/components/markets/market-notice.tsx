import type { ReactNode } from "react";
import Link from "next/link";
import type { LucideIcon } from "lucide-react";

import { cx } from "@/components/ui/primitives";

/*
 * A full-width state in place of market content: an asset that does not
 * exist, the market not open yet, a search with no match. Square charcoal
 * card, a dashed yellow icon, and at most one way forward (`children`).
 */

export function MarketNotice({
  icon: Icon,
  title,
  body,
  testId,
  children,
}: {
  icon: LucideIcon;
  title: string;
  body: string;
  testId: string;
  /** The way forward: a `NoticeLink` or a button. */
  children?: ReactNode;
}) {
  return (
    <section className="flex flex-col items-center border border-border bg-surface px-6 py-14 text-center" data-testid={testId}>
      <span
        aria-hidden
        className="grid size-12 place-items-center border border-dashed border-synthetic-yellow/40 text-synthetic-yellow"
      >
        <Icon className="size-5" />
      </span>
      <h2 className="mt-4 text-base font-semibold tracking-tight text-ink">{title}</h2>
      <p className="mt-1 max-w-sm text-sm text-muted">{body}</p>
      {children ? <div className="mt-6">{children}</div> : null}
    </section>
  );
}

/** The notice's way forward, as the one gold action on the card. */
export const NOTICE_ACTION = cx(
  "inline-flex h-11 items-center justify-center bg-synthetic-yellow px-5 text-xs font-bold tracking-[0.2em] text-black uppercase transition-colors",
  "hover:bg-synthetic-yellow-hover active:bg-synthetic-yellow-pressed",
  "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:ring-offset-2 focus-visible:ring-offset-surface focus-visible:outline-none",
);

export function NoticeLink({ href, children }: { href: string; children: ReactNode }) {
  return (
    <Link href={href} className={NOTICE_ACTION}>
      {children}
    </Link>
  );
}
