"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { Landmark, ShieldCheck, TrendingUp, Vault, type LucideIcon } from "lucide-react";

import { Brand } from "@/components/brand";
import { cx } from "@/components/ui/primitives";

/*
 * Dashboard navigation: one list of destinations, two presentations.
 *
 * - From `lg` up, a fixed left sidebar: the web terminal.
 * - Below `lg`, a bottom tab bar within thumb reach, padded clear of the
 *   phone's home indicator.
 *
 * The active destination is Synthetic Capital Yellow: text, icon and its
 * indicator (a bar in the sidebar, a solid pill in the tab bar), and it
 * carries aria-current="page". Only one of the two is ever displayed, so
 * assistive technology meets a single "Main" navigation.
 */

export interface NavItem {
  href: string;
  label: string;
  icon: LucideIcon;
  /** Active only on this exact path (a section root whose sub-pages are other items). */
  exact?: boolean;
}

export const DASHBOARD_NAV: readonly NavItem[] = [
  { href: "/dashboard", label: "Vault", icon: Vault, exact: true },
  { href: "/dashboard/account", label: "Account", icon: Landmark },
  { href: "/dashboard/invest", label: "Invest", icon: TrendingUp },
  { href: "/dashboard/reserve", label: "Reserve", icon: ShieldCheck },
];

/** Whether `item` is the page at `pathname` (or, unless exact, one of its sub-pages). */
export function isActive(item: Pick<NavItem, "href" | "exact">, pathname: string): boolean {
  if (pathname === item.href) return true;
  return !item.exact && pathname.startsWith(`${item.href}/`);
}

/** The item for `pathname`, for page titles; null outside the list. */
export function activeItem(pathname: string, items: readonly NavItem[] = DASHBOARD_NAV): NavItem | null {
  return items.find((item) => isActive(item, pathname)) ?? null;
}

/** Width of the sidebar: the content column offsets by the same amount (`lg:pl-60`). */
export const SIDEBAR_WIDTH = "w-60";

export function Navigation({ items = DASHBOARD_NAV }: { items?: readonly NavItem[] }) {
  const pathname = usePathname();

  return (
    <>
      <aside
        className={cx(
          "fixed inset-y-0 left-0 z-30 hidden flex-col border-r border-border bg-bg lg:flex",
          SIDEBAR_WIDTH,
        )}
      >
        <div className="flex h-16 shrink-0 items-center border-b border-border px-5">
          <Brand href="/dashboard" />
        </div>
        <nav aria-label="Main" className="flex-1 overflow-y-auto px-3 py-5">
          <p className="px-3 pb-3 text-[10px] font-semibold tracking-[0.24em] text-muted uppercase">Menu</p>
          <ul className="space-y-1">
            {items.map((item) => (
              <li key={item.href}>
                <SidebarLink item={item} active={isActive(item, pathname)} />
              </li>
            ))}
          </ul>
        </nav>
        <p className="border-t border-border px-5 py-4 text-[11px] leading-relaxed text-muted">
          <span className="font-semibold text-synthetic-yellow">100% reserve.</span> Every peso is held in custody,
          never lent.
        </p>
      </aside>

      <nav
        aria-label="Main"
        className="fixed inset-x-0 bottom-0 z-30 border-t border-border bg-bg/95 pb-[env(safe-area-inset-bottom)] backdrop-blur-md lg:hidden"
      >
        <ul
          className="mx-auto grid max-w-lg"
          style={{ gridTemplateColumns: `repeat(${items.length}, minmax(0, 1fr))` }}
        >
          {items.map((item) => (
            <li key={item.href}>
              <TabLink item={item} active={isActive(item, pathname)} />
            </li>
          ))}
        </ul>
      </nav>
    </>
  );
}

/* ------------------------------------------------------------------------ */

function SidebarLink({ item, active }: { item: NavItem; active: boolean }) {
  const Icon = item.icon;
  return (
    <Link
      href={item.href}
      aria-current={active ? "page" : undefined}
      className={cx(
        "relative flex min-h-10 items-center gap-3 rounded-lg px-3 text-sm font-medium transition-colors",
        "focus-visible:ring-2 focus-visible:ring-synthetic-yellow focus-visible:outline-none",
        active
          ? "bg-synthetic-yellow/10 font-semibold text-synthetic-yellow"
          : "text-muted hover:bg-surface hover:text-ink",
      )}
    >
      {/* The active bar sits on the sidebar's inner edge. */}
      {active ? (
        <span aria-hidden className="absolute inset-y-2 -left-3 w-[3px] rounded-r-full bg-synthetic-yellow" />
      ) : null}
      <Icon className="size-[18px] shrink-0" strokeWidth={active ? 2.25 : 1.75} aria-hidden />
      {item.label}
    </Link>
  );
}

function TabLink({ item, active }: { item: NavItem; active: boolean }) {
  const Icon = item.icon;
  return (
    <Link
      href={item.href}
      aria-current={active ? "page" : undefined}
      className="group flex min-h-16 flex-col items-center justify-center gap-1 px-1 pt-2 pb-1.5 focus-visible:outline-none"
    >
      <span
        className={cx(
          "grid h-8 w-14 place-items-center rounded-full transition-colors",
          "group-focus-visible:ring-2 group-focus-visible:ring-synthetic-yellow",
          active ? "bg-synthetic-yellow text-black" : "text-muted group-hover:text-ink",
        )}
      >
        <Icon className="size-5" strokeWidth={active ? 2.25 : 1.75} aria-hidden />
      </span>
      <span
        className={cx(
          "text-[11px] leading-none",
          active ? "font-semibold text-synthetic-yellow" : "font-medium text-muted group-hover:text-ink",
        )}
      >
        {item.label}
      </span>
    </Link>
  );
}
