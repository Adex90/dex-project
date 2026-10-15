"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Building2, Loader2, LogOut } from "lucide-react";

import { StepIne } from "@/components/onboarding/step-ine";
import { StepLiveness, type VerificationOutcome } from "@/components/onboarding/step-liveness";
import { StepPersonal } from "@/components/onboarding/step-personal";
import { KycStatusBadge, StepStatus } from "@/components/onboarding/step-status";
import {
  EMPTY_INE,
  MORAL_EXPLANATION,
  STEPS,
  canRetry,
  canVisitStep,
  checkCredentialDraft,
  checkIneImages,
  effectiveStep,
  ineDraftFromFile,
  isProfileSaved,
  mergeSavedProfile,
  resumeStep,
  splitDetails,
  verificationProblem,
  type IneDraft,
  type Step,
} from "@/components/onboarding/logic";
import { StepFrame, Stepper } from "@/components/onboarding/parts";
import { Alert, Button, Skeleton } from "@/components/ui/primitives";
import { useSession } from "@/hooks/use-session";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import type { ResultadoVerificacion, SolicitudVerificacion, VistaExpediente } from "@/lib/api/types";
import { parseCurp, todayInMexicoCity } from "@/lib/kyc";

/** How often the status page asks for news while a file is in review. */
const REVIEW_POLL_MS = 10_000;

const DESCRIPTIONS: Record<Step, string> = {
  1: "As printed on your INE. We check your CURP and RFC as you type.",
  2: "Photograph both sides of your voting credential and type the details printed on it.",
  3: "A short live check that you are the person on the INE.",
  4: "Where your identity verification stands.",
};

function isAbort(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

/**
 * Digital onboarding (KYC): personal data -> INE -> liveness selfie ->
 * status. Until the file is VERIFICADO the database blocks deposits,
 * transfers, withdrawals and investments for this customer.
 */
export default function OnboardingPage() {
  const router = useRouter();
  const { hydrated, valid, token, role, email, logout } = useSession({ redirect: true });
  const [file, setFile] = useState<VistaExpediente | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [step, setStep] = useState<Step>(1);
  const [ine, setIne] = useState<IneDraft>(EMPTY_INE);
  const [immediate, setImmediate] = useState<ResultadoVerificacion | null>(null);
  const [checking, setChecking] = useState(false);
  const [lastChecked, setLastChecked] = useState<Date | null>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const focusPending = useRef(false);

  const expire = useCallback(() => {
    logout();
    router.replace("/auth/login?expired=1");
  }, [logout, router]);

  const signOut = () => {
    logout();
    router.replace("/auth/login");
  };

  /** GET /kyc/expediente; null (and `loadError`) when it fails. */
  const fetchFile = useCallback(
    async (signal?: AbortSignal): Promise<VistaExpediente | null> => {
      if (!token) return null;
      try {
        const fresh = await api.kycFile(token, signal);
        setFile(fresh);
        setLoadError(null);
        setLastChecked(new Date());
        return fresh;
      } catch (error) {
        if (isAbort(error)) return null;
        if (error instanceof ApiError && error.isUnauthorized) {
          expire();
          return null;
        }
        setLoadError(describeError(error));
        return null;
      }
    },
    [token, expire],
  );

  // First load: resume where the customer left off.
  useEffect(() => {
    if (!token || role !== "CLIENTE") return;
    const ctrl = new AbortController();
    void fetchFile(ctrl.signal).then((fresh) => {
      if (!fresh) return;
      setStep(resumeStep(fresh));
      setIne(ineDraftFromFile(fresh));
    });
    return () => ctrl.abort();
  }, [token, role, fetchFile]);

  const today = useMemo(() => todayInMexicoCity(), []);
  const curp = useMemo(() => {
    const parsed = parseCurp(file?.curp ?? "");
    return parsed.ok ? parsed.value : null;
  }, [file?.curp]);
  const profileSaved = file !== null && isProfileSaved(file);
  const ineReady =
    curp !== null &&
    checkCredentialDraft(ine, curp, today).input !== null &&
    Object.keys(checkIneImages(ine)).length === 0;
  const routing = file
    ? {
        estado_kyc: file.estado_kyc,
        profileSaved,
        ineReady,
        hasAttempt: file.intentos > 0 || file.ultimo_resultado !== null,
      }
    : null;
  const current: Step = routing ? effectiveStep(step, routing) : step;

  const goTo = useCallback((next: Step) => {
    focusPending.current = true;
    setStep(next);
  }, []);

  // Move focus to the new step's heading so assistive technology announces it.
  useEffect(() => {
    if (!focusPending.current) return;
    focusPending.current = false;
    headingRef.current?.focus();
  }, [current]);

  const checkNow = useCallback(async () => {
    setChecking(true);
    try {
      await fetchFile();
    } finally {
      setChecking(false);
    }
  }, [fetchFile]);

  // In review: poll every ~10 s while the page is visible, and on returning to it.
  const polling = current === 4 && file?.estado_kyc === "EN_REVISION";
  useEffect(() => {
    if (!polling) return;
    const tick = () => {
      if (document.visibilityState === "visible") void checkNow();
    };
    const id = setInterval(tick, REVIEW_POLL_MS);
    document.addEventListener("visibilitychange", tick);
    return () => {
      clearInterval(id);
      document.removeEventListener("visibilitychange", tick);
    };
  }, [polling, checkNow]);

  const submitVerification = useCallback(
    async ({ frames, reto }: { frames: string[]; reto: string }): Promise<VerificationOutcome> => {
      const credential = curp ? checkCredentialDraft(ine, curp, todayInMexicoCity()).input : null;
      if (!token || !credential || !ine.frente || !ine.reverso) {
        return { ok: false, message: "Complete the photos and details of your INE first.", action: "id" };
      }
      const body: SolicitudVerificacion = {
        ine: {
          clave_elector: credential.clave_elector,
          cic: credential.cic,
          ...(credential.identificador_ciudadano ? { identificador_ciudadano: credential.identificador_ciudadano } : {}),
          emision: credential.emision,
          vigencia: credential.vigencia,
          frente: ine.frente,
          reverso: ine.reverso,
        },
        selfie: { frames, reto },
        consentimiento_biometrico: true,
      };
      try {
        const sent = await api.kycVerify(token, body);
        setImmediate(sent.resultado);
        const fresh = await fetchFile();
        if (!fresh) {
          setFile((prev) =>
            prev
              ? {
                  ...prev,
                  estado_kyc: sent.estado_kyc,
                  intentos: sent.intentos,
                  intentos_restantes: sent.resultado?.intentos_restantes ?? Math.max(0, prev.max_intentos - sent.intentos),
                  verificacion_en_curso: sent.verificacion_id,
                }
              : prev,
          );
        }
        goTo(4);
        return { ok: true };
      } catch (error) {
        if (error instanceof ApiError) {
          if (error.isUnauthorized) {
            expire();
            return { ok: false, message: "Your session has expired. Please sign in again.", action: null };
          }
          const problem = verificationProblem(error.code, error.message);
          if (problem) {
            if (problem.action === "status") {
              await fetchFile();
              goTo(4);
            }
            return { ok: false, ...problem };
          }
        }
        return { ok: false, message: describeError(error), action: "selfie" };
      }
    },
    [token, curp, ine, fetchFile, goTo, expire],
  );

  // ---------------------------------------------------------------------------
  // Render
  // ---------------------------------------------------------------------------

  if (!hydrated || !valid) {
    return (
      <div className="flex items-center justify-center py-10 text-sm text-muted" role="status">
        <Loader2 className="mr-2 size-4 animate-spin" aria-hidden /> Checking your session…
      </div>
    );
  }

  if (role !== "CLIENTE") {
    return (
      <div className="space-y-3 text-center text-sm">
        <h1 className="text-lg font-semibold">Identity verification is for account holders</h1>
        <p className="text-ink-2">Operators and auditors review files from the operations console.</p>
        <Link href="/auth/login" onClick={logout} className="font-medium text-accent hover:underline">
          Sign in with a client account
        </Link>
      </div>
    );
  }

  const header = (
    <div className="space-y-1">
      <h1 className="text-xl font-semibold tracking-tight">Verify your identity</h1>
      <p className="text-sm text-muted">
        Mexican banking rules require it before you can deposit, transfer or invest. It takes a few minutes.
      </p>
    </div>
  );

  const footer = (
    <div className="flex items-center justify-between gap-3 border-t border-border pt-4 text-xs text-muted">
      <span className="min-w-0 truncate">{email ? `Signed in as ${email}` : null}</span>
      <Button type="button" variant="ghost" size="sm" onClick={signOut}>
        <LogOut className="size-3.5" aria-hidden />
        Sign out
      </Button>
    </div>
  );

  if (!file) {
    return (
      <div className="space-y-6">
        {header}
        {loadError ? (
          <Alert tone="error" title="Could not load your verification file">
            <p>{loadError}</p>
            <Button type="button" variant="secondary" size="sm" className="mt-2" onClick={() => void checkNow()} loading={checking}>
              Retry
            </Button>
          </Alert>
        ) : (
          <div className="space-y-4" aria-busy="true" aria-label="Loading your verification file">
            <Skeleton className="h-10 w-full" />
            <Skeleton className="h-5 w-2/3" />
            <Skeleton className="h-40 w-full" />
          </div>
        )}
        {footer}
      </div>
    );
  }

  if (file.tipo_persona === "MORAL") {
    return (
      <div className="space-y-6">
        {header}
        <section data-testid="onboarding-moral" aria-labelledby="moral-title" className="space-y-4 text-center">
          <Building2 className="mx-auto size-8 text-accent" aria-hidden />
          <h2 id="moral-title" className="text-lg font-semibold">
            Remote verification is for individuals
          </h2>
          <p className="text-sm text-ink-2">{MORAL_EXPLANATION}</p>
          <KycStatusBadge estado={file.estado_kyc} />
          <Button type="button" size="lg" className="w-full" onClick={() => router.push("/dashboard")}>
            Go to dashboard
          </Button>
        </section>
        {footer}
      </div>
    );
  }

  const title = STEPS.find((s) => s.n === current)?.title ?? "Verify your identity";
  const lastRejected = canRetry(file) && file.ultimo_resultado?.resultado === "RECHAZADO" ? file.ultimo_resultado : null;

  return (
    <div className="space-y-6">
      {header}
      {routing ? <Stepper current={current} canVisit={(s) => canVisitStep(s, routing)} onVisit={goTo} /> : null}
      {loadError ? <Alert tone="warning">Could not refresh your file: {loadError}</Alert> : null}

      <StepFrame key={current} step={current} title={title} description={DESCRIPTIONS[current]} headingRef={headingRef}>
        {current === 1 ? (
          <StepPersonal
            token={token ?? ""}
            file={file}
            onSaved={(saved) => setFile((prev) => (prev ? mergeSavedProfile(prev, saved.expediente) : prev))}
            onContinue={() => goTo(2)}
            onReload={() => void fetchFile()}
            onUnauthorized={expire}
          />
        ) : null}
        {current === 2 && curp ? (
          <StepIne
            curp={curp}
            draft={ine}
            onChange={setIne}
            previousAttempt={
              lastRejected
                ? { errors: splitDetails(lastRejected.detalles_fallo).errors, remaining: file.intentos_restantes }
                : null
            }
            onViewStatus={() => goTo(4)}
            onBack={() => goTo(1)}
            onContinue={() => goTo(3)}
          />
        ) : null}
        {current === 3 && ine.frente && ine.reverso ? (
          <StepLiveness
            ineImages={{ frente: ine.frente, reverso: ine.reverso }}
            onSubmit={submitVerification}
            onBack={() => goTo(2)}
            onEditProfile={() => goTo(1)}
          />
        ) : null}
        {current === 4 ? (
          <StepStatus
            file={file}
            immediate={immediate}
            polling={{ active: polling, checking, lastChecked, onCheckNow: () => void checkNow() }}
            onTryAgain={() => goTo(2)}
            onResume={() => goTo(resumeStep(file))}
            onDashboard={() => router.push("/dashboard")}
          />
        ) : null}
      </StepFrame>

      {footer}
    </div>
  );
}
