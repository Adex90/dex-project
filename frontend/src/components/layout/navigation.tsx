"use client";

import { useEffect } from "react";
import Link from "next/link";
import { usePathname } from "next/navigation";
import { Landmark, Search, ShieldCheck, TrendingUp, Vault, type LucideIcon } from "lucide-react";
import { create } from "zustand";

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
 *
 * An asset's page is not a tab of its own: it opens from the Vault's holdings
 * and from Discover, and the tab that opened it stays current there (its
 * Back link returns to that tab). Opened by its address, it is the Vault's.
 */

export interface NavItem {
  href: string;
  label: string;
  icon: LucideIcon;
  /** Active only on this exact path (a section root whose sub-pages are other items). */
  exact?: boolean;
}

const HOME = "/dashboard";
const VAULT: NavItem = { href: HOME, label: "Vault", icon: Vault, exact: true };

export const DASHBOARD_NAV: readonly NavItem[] = [
  VAULT,
  { href: "/dashboard/discover", label: "Discover", icon: Search },
  { href: "/dashboard/account", label: "Account", icon: Landmark },
  { href: "/dashboard/invest", label: "Invest", icon: TrendingUp },
  { href: "/dashboard/reserve", label: "Reserve", icon: ShieldCheck },
];

/** Pages that open from more than one tab: they belong to the tab that opened them. */
const DETAIL_PATHS: readonly string[] = ["/dashboard/asset"];

/** Under `path`: the page itself or one of its sub-pages. */
function isUnder(pathname: string, path: string): boolean {
  return pathname === path || pathname.startsWith(`${path}/`);
}

/** Whether `item` is the page at `pathname` (or, unless exact, one of its sub-pages). */
export function isActive(item: Pick<NavItem, "href" | "exact">, pathname: string): boolean {
  if (pathname === item.href) return true;
  return !item.exact && isUnder(pathname, item.href);
}

/** Whether `pathname` is a detail page (an asset's), which belongs to the tab that opened it. */
export function isDetailPage(pathname: string): boolean {
  return DETAIL_PATHS.some((path) => isUnder(pathname, path));
}

/** The tab at `href`; the Vault for anything else. */
export function tabFor(href: string, items: readonly NavItem[] = DASHBOARD_NAV): NavItem {
  return items.find((item) => item.href === href) ?? VAULT;
}

/**
 * The item for `pathname`, for the navigation and the page title; null outside
 * the list. A detail page belongs to `origin`, the tab that opened it.
 */
export function activeItem(pathname: string, origin = HOME, items: readonly NavItem[] = DASHBOARD_NAV): NavItem | null {
  if (isDetailPage(pathname)) return tabFor(origin, items);
  return items.find((item) => isActive(item, pathname)) ?? null;
}

/** The last tab shown: module state, so it outlives the pages between tabs. */
const useOriginStore = create<{ href: string }>()(() => ({ href: HOME }));

/**
 * The tab a detail page belongs to: the last tab shown before it, so an asset
 * opened from Discover keeps Discover current and its Back link returns
 * there. The Vault for a page opened by its address. Every caller records
 * the tab it is shown under.
 */
export function useNavOrigin(items: readonly NavItem[] = DASHBOARD_NAV): string {
  const pathname = usePathname();
  const origin = useOriginStore((state) => state.href);
  useEffect(() => {
    if (isDetailPage(pathname)) return;
    const tab = items.find((item) => isActive(item, pathname));
    if (tab && tab.href !== useOriginStore.getState().href) useOriginStore.setState({ href: tab.href });
  }, [pathname, items]);
  return origin;
}

/** Forgets the last tab (sign-out): the next session's first asset page belongs to the Vault. */
export function resetNavOrigin(): void {
  useOriginStore.setState({ href: HOME });
}

/** Width of the sidebar: the content column offsets by the same amount (`lg:pl-60`). */
export const SIDEBAR_WIDTH = "w-60";

export function Navigation({ items = DASHBOARD_NAV }: { items?: readonly NavItem[] }) {
  const pathname = usePathname();
  const current = activeItem(pathname, useNavOrigin(items), items);

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
                <SidebarLink item={item} active={item === current} />
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
              <TabLink item={item} active={item === current} />
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
        <span aria-hidden className="absolute inset-y-2 -left-3 w-[3px] bg-synthetic-yellow" />
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
          "grid h-8 w-14 place-items-center rounded-sm transition-colors",
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
