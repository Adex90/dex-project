"use client";

import { useEffect, useState, type FormEvent } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { UserPlus } from "lucide-react";

import { Alert, Button, Field, Input, cx } from "@/components/ui/primitives";
import { useSession } from "@/hooks/use-session";
import { describeError } from "@/lib/api/client";
import type { RegistroInput, TipoPersona } from "@/lib/api/types";
import { useAuthStore } from "@/store/auth";

const MIN_PASSWORD = 12;
const CURP_RE = /^[A-Z]{4}\d{6}[HMX][A-Z]{5}[A-Z0-9]\d$/;
const RFC_RE = /^[A-ZÑ&]{3,4}\d{6}[A-Z0-9]{3}$/;

interface FormState {
  tipo: TipoPersona;
  nombre: string;
  paterno: string;
  materno: string;
  email: string;
  password: string;
  confirm: string;
  curp: string;
  rfc: string;
}

type Errors = Partial<Record<keyof FormState, string>>;

function validate(f: FormState): Errors {
  const e: Errors = {};
  if (!f.nombre.trim()) e.nombre = f.tipo === "FISICA" ? "Enter your given name." : "Enter the legal name.";
  if (f.tipo === "FISICA" && !f.paterno.trim()) e.paterno = "Enter your paternal surname.";
  if (!/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(f.email.trim())) e.email = "Enter a valid email address.";
  if ([...f.password].length < MIN_PASSWORD) e.password = `Use at least ${MIN_PASSWORD} characters.`;
  if (f.confirm !== f.password) e.confirm = "Passwords do not match.";
  if (f.curp && !CURP_RE.test(f.curp.toUpperCase())) e.curp = "CURP must have 18 characters (e.g. ABCD800101HDFRRR09).";
  if (f.rfc && !RFC_RE.test(f.rfc.toUpperCase())) e.rfc = "RFC must have 12 or 13 characters.";
  return e;
}

export default function RegisterPage() {
  const router = useRouter();
  const register = useAuthStore((s) => s.register);
  const { valid, role } = useSession();
  const [form, setForm] = useState<FormState>({
    tipo: "FISICA",
    nombre: "",
    paterno: "",
    materno: "",
    email: "",
    password: "",
    confirm: "",
    curp: "",
    rfc: "",
  });
  const [touched, setTouched] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const errors = touched ? validate(form) : {};

  useEffect(() => {
    if (valid && role === "CLIENTE") router.replace("/dashboard");
  }, [valid, role, router]);

  const set = <K extends keyof FormState>(key: K, value: FormState[K]) => setForm((f) => ({ ...f, [key]: value }));

  const onSubmit = async (event: FormEvent) => {
    event.preventDefault();
    setTouched(true);
    setError(null);
    if (Object.keys(validate(form)).length) return;
    const input: RegistroInput = {
      tipo_persona: form.tipo,
      nombre: form.nombre.trim(),
      email: form.email.trim(),
      password: form.password,
      ...(form.tipo === "FISICA" && form.paterno.trim() ? { apellido_paterno: form.paterno.trim() } : {}),
      ...(form.tipo === "FISICA" && form.materno.trim() ? { apellido_materno: form.materno.trim() } : {}),
      ...(form.tipo === "FISICA" && form.curp.trim() ? { curp: form.curp.trim().toUpperCase() } : {}),
      ...(form.rfc.trim() ? { rfc: form.rfc.trim().toUpperCase() } : {}),
    };
    setSubmitting(true);
    try {
      await register(input);
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
        <h1 className="text-xl font-semibold tracking-tight">Open an account</h1>
        <p className="text-sm text-muted">You get a vault account with its own CLABE right away.</p>
      </div>

      {error ? <Alert tone="error">{error}</Alert> : null}

      <fieldset className="grid grid-cols-2 gap-1 rounded-lg border border-border bg-surface-2 p-1">
        <legend className="sr-only">Account holder type</legend>
        {(
          [
            ["FISICA", "Individual"],
            ["MORAL", "Company"],
          ] as const
        ).map(([value, label]) => (
          <label
            key={value}
            className={cx(
              "cursor-pointer rounded-md px-3 py-1.5 text-center text-sm font-medium transition",
              form.tipo === value ? "bg-surface text-ink shadow-sm" : "text-muted hover:text-ink",
            )}
          >
            <input
              type="radio"
              name="tipo"
              value={value}
              checked={form.tipo === value}
              onChange={() => set("tipo", value)}
              className="sr-only"
            />
            {label}
          </label>
        ))}
      </fieldset>

      <Field label={form.tipo === "FISICA" ? "Given name(s)" : "Legal name"} htmlFor="nombre" error={errors.nombre}>
        <Input
          id="nombre"
          autoComplete={form.tipo === "FISICA" ? "given-name" : "organization"}
          value={form.nombre}
          onChange={(e) => set("nombre", e.target.value)}
          invalid={Boolean(errors.nombre)}
        />
      </Field>

      {form.tipo === "FISICA" ? (
        <div className="grid grid-cols-2 gap-3">
          <Field label="Paternal surname" htmlFor="paterno" error={errors.paterno}>
            <Input
              id="paterno"
              autoComplete="family-name"
              value={form.paterno}
              onChange={(e) => set("paterno", e.target.value)}
              invalid={Boolean(errors.paterno)}
            />
          </Field>
          <Field label="Maternal surname" htmlFor="materno" hint="Optional">
            <Input id="materno" value={form.materno} onChange={(e) => set("materno", e.target.value)} />
          </Field>
        </div>
      ) : null}

      <Field label="Email" htmlFor="email" error={errors.email}>
        <Input
          id="email"
          type="email"
          autoComplete="email"
          value={form.email}
          onChange={(e) => set("email", e.target.value)}
          invalid={Boolean(errors.email)}
        />
      </Field>

      <div className="grid grid-cols-2 gap-3">
        <Field label="Password" htmlFor="password" error={errors.password} hint={`${MIN_PASSWORD}+ characters`}>
          <Input
            id="password"
            type="password"
            autoComplete="new-password"
            value={form.password}
            onChange={(e) => set("password", e.target.value)}
            invalid={Boolean(errors.password)}
          />
        </Field>
        <Field label="Confirm password" htmlFor="confirm" error={errors.confirm}>
          <Input
            id="confirm"
            type="password"
            autoComplete="new-password"
            value={form.confirm}
            onChange={(e) => set("confirm", e.target.value)}
            invalid={Boolean(errors.confirm)}
          />
        </Field>
      </div>

      <details className="group rounded-lg border border-border px-3 py-2 text-sm">
        <summary className="cursor-pointer text-ink-2 select-none">Tax and identity IDs (optional)</summary>
        <div className="mt-3 grid gap-3 sm:grid-cols-2">
          {form.tipo === "FISICA" ? (
            <Field label="CURP" htmlFor="curp" error={errors.curp}>
              <Input
                id="curp"
                maxLength={18}
                value={form.curp}
                onChange={(e) => set("curp", e.target.value.toUpperCase())}
                invalid={Boolean(errors.curp)}
                className="font-mono uppercase"
              />
            </Field>
          ) : null}
          <Field label="RFC" htmlFor="rfc" error={errors.rfc}>
            <Input
              id="rfc"
              maxLength={13}
              value={form.rfc}
              onChange={(e) => set("rfc", e.target.value.toUpperCase())}
              invalid={Boolean(errors.rfc)}
              className="font-mono uppercase"
            />
          </Field>
        </div>
      </details>

      <Button type="submit" className="w-full" size="lg" loading={submitting}>
        <UserPlus className="size-4" aria-hidden />
        Create account
      </Button>

      <p className="text-center text-sm text-muted">
        Already a client?{" "}
        <Link href="/auth/login" className="font-medium text-accent hover:underline">
          Sign in
        </Link>
      </p>
    </form>
  );
}
