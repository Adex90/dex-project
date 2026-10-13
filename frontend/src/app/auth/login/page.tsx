"use client";

import { Suspense, useEffect, useState, type FormEvent } from "react";
import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { LogIn } from "lucide-react";

import { Alert, Button, Field, Input } from "@/components/ui/primitives";
import { useSession } from "@/hooks/use-session";
import { describeError } from "@/lib/api/client";
import { useAuthStore } from "@/store/auth";

function LoginForm() {
  const router = useRouter();
  const params = useSearchParams();
  const login = useAuthStore((s) => s.login);
  const { valid, role } = useSession();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    if (valid && role === "CLIENTE") router.replace("/dashboard");
  }, [valid, role, router]);

  const onSubmit = async (event: FormEvent) => {
    event.preventDefault();
    setError(null);
    setSubmitting(true);
    try {
      const session = await login(email, password);
      if (session.rol !== "CLIENTE") {
        setError("This app is for account holders. Operators and auditors use the operations console.");
        useAuthStore.getState().logout();
        return;
      }
      router.replace("/dashboard");
    } catch (e) {
      setError(describeError(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <form onSubmit={onSubmit} className="space-y-5" noValidate>
      <div className="space-y-1">
        <h1 className="text-xl font-semibold tracking-tight">Sign in</h1>
        <p className="text-sm text-muted">Access your vault and fixed-term contracts.</p>
      </div>

      {params.get("expired") ? (
        <Alert tone="info" title="Your session expired">
          For your security, sessions last one hour. Please sign in again.
        </Alert>
      ) : null}
      {error ? <Alert tone="error">{error}</Alert> : null}

      <Field label="Email" htmlFor="email">
        <Input
          id="email"
          type="email"
          autoComplete="email"
          required
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          placeholder="you@example.com"
        />
      </Field>
      <Field label="Password" htmlFor="password">
        <Input
          id="password"
          type="password"
          autoComplete="current-password"
          required
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
      </Field>

      <Button type="submit" className="w-full" size="lg" loading={submitting} disabled={!email || !password}>
        <LogIn className="size-4" aria-hidden />
        Sign in
      </Button>

      <p className="text-center text-sm text-muted">
        New to Synthetic Capital?{" "}
        <Link href="/auth/register" className="font-medium text-accent hover:underline">
          Open an account
        </Link>
      </p>
    </form>
  );
}

export default function LoginPage() {
  return (
    <Suspense>
      <LoginForm />
    </Suspense>
  );
}
