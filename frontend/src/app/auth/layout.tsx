import { ShieldCheck } from "lucide-react";

import { Brand } from "@/components/brand";

export default function AuthLayout({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex min-h-dvh flex-col items-center justify-center px-5 py-10">
      <div className="w-full max-w-md space-y-6">
        <div className="flex justify-center">
          <Brand />
        </div>
        <div className="rounded-2xl border border-border bg-surface p-6 shadow-sm sm:p-8">{children}</div>
        <p className="flex items-center justify-center gap-1.5 text-xs text-muted">
          <ShieldCheck className="size-3.5" aria-hidden />
          Passwords are hashed with Argon2id. Sessions expire automatically.
        </p>
      </div>
    </div>
  );
}
