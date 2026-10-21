"use client";

import { Loader2 } from "lucide-react";
import Link from "next/link";

import { homeForStaff } from "@/hooks/use-client-home";
import { useSession } from "@/hooks/use-session";

/** Client-side guard: only authenticated account holders see the dashboard. */
export default function DashboardLayout({ children }: { children: React.ReactNode }) {
  const { hydrated, valid, role, logout } = useSession({ redirect: true });

  if (!hydrated || !valid) {
    return (
      <div className="flex min-h-dvh items-center justify-center text-sm text-muted">
        <Loader2 className="mr-2 size-4 animate-spin" aria-hidden /> Checking your session…
      </div>
    );
  }
  if (role !== "CLIENTE") {
    const staffHome = role ? homeForStaff(role) : null;
    return (
      <div className="flex min-h-dvh flex-col items-center justify-center gap-3 px-5 text-center text-sm">
        {staffHome ? (
          <>
            <p className="text-ink-2">This dashboard is for account holders. Compliance staff use the compliance console.</p>
            <Link href={staffHome} className="font-medium text-accent hover:underline">
              Open the compliance console
            </Link>
          </>
        ) : (
          <>
            <p className="text-ink-2">This dashboard is for account holders. Operators use the operations console.</p>
            <Link href="/auth/login" onClick={logout} className="font-medium text-accent hover:underline">
              Sign in with a client account
            </Link>
          </>
        )}
      </div>
    );
  }
  return <>{children}</>;
}
