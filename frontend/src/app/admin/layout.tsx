"use client";

import { Loader2, LogOut, ShieldCheck } from "lucide-react";
import Link from "next/link";
import { useRouter } from "next/navigation";

import { Brand } from "@/components/brand";
import { Badge, Button } from "@/components/ui/primitives";
import { useSession } from "@/hooks/use-session";
import { ADMIN_AML_PATH } from "@/hooks/use-client-home";
import { isComplianceRole } from "@/lib/aml";

/**
 * Client-side guard of the staff console: compliance officers (CUMPLIMIENTO)
 * and auditors (AUDITOR, read-only). A convenience only: the API enforces the
 * role on every route, so a forged request from another role gets 403.
 */
export default function AdminLayout({ children }: { children: React.ReactNode }) {
  const router = useRouter();
  const { hydrated, valid, role, email, logout } = useSession({ redirect: true });

  const signOut = () => {
    logout();
    router.replace("/auth/login");
  };

  let body: React.ReactNode;
  if (!hydrated || !valid) {
    body = (
      <div className="flex min-h-dvh items-center justify-center text-sm text-muted">
        <Loader2 className="mr-2 size-4 animate-spin" aria-hidden /> Checking your session…
      </div>
    );
  } else if (!isComplianceRole(role)) {
    body = (
      <div className="flex min-h-dvh flex-col items-center justify-center gap-3 px-5 text-center text-sm">
        <p className="text-ink-2">This console is for compliance officers and auditors.</p>
        <Link href="/auth/login" onClick={logout} className="font-medium text-accent hover:underline">
          Sign out
        </Link>
      </div>
    );
  } else {
    const auditor = role === "AUDITOR";
    body = (
      <div className="min-h-dvh">
        <header className="sticky top-0 z-20 border-b border-border bg-bg/85 backdrop-blur">
          <div className="mx-auto flex h-14 max-w-[96rem] items-center justify-between gap-4 px-4 sm:px-6">
            <div className="flex min-w-0 items-center gap-3">
              <Brand href={ADMIN_AML_PATH} compact />
              <span className="truncate text-sm font-semibold tracking-tight text-ink">Compliance console</span>
            </div>
            <div className="flex items-center gap-2 sm:gap-3">
              <Badge tone={auditor ? "info" : "accent"} icon={<ShieldCheck className="size-3" aria-hidden />}>
                {auditor ? "Auditor · read-only" : "Compliance officer"}
              </Badge>
              {email ? <span className="hidden text-xs text-muted md:inline">{email}</span> : null}
              <Button variant="secondary" size="sm" onClick={signOut}>
                <LogOut className="size-3.5" aria-hidden />
                <span className="hidden sm:inline">Sign out</span>
              </Button>
            </div>
          </div>
        </header>
        {children}
      </div>
    );
  }

  return (
    <>
      <meta name="robots" content="noindex" />
      {body}
    </>
  );
}
