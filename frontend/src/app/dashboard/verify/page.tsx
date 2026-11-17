"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import { ArrowLeft, ArrowRight, Building2, Clock } from "lucide-react";

import { useDashboard } from "@/components/layout/dashboard-shell";
import { acceptanceFor } from "@/components/onboarding/agreements";
import {
  DECLARATION_INPUT_IDS,
  EMPTY_INE,
  MORAL_EXPLANATION,
  STEPS,
  canRetry,
  canVisitStep,
  checkCredentialDraft,
  checkDeclarations,
  checkIneImages,
  declarationsFromFile,
  effectiveStep,
  identityKey,
  ineDraftFromFile,
  isProfileSaved,
  mergeSavedProfile,
  resumeStep,
  splitDetails,
  verificationProblem,
  type DeclarationField,
  type DeclarationsDraft,
  type IneDraft,
  type Step,
  type View,
} from "@/components/onboarding/logic";
import { Progress, StepFrame } from "@/components/onboarding/parts";
import { StepDeclarations } from "@/components/onboarding/step-declarations";
import { StepIne } from "@/components/onboarding/step-ine";
import { StepLiveness, type LivenessCapture } from "@/components/onboarding/step-liveness";
import { StepPersonal } from "@/components/onboarding/step-personal";
import { StepReview, type SubmitProblem } from "@/components/onboarding/step-review";
import { KycStatusBadge, StepStatus, type SubmitNotice } from "@/components/onboarding/step-status";
import { Alert, Button, Skeleton } from "@/components/ui/primitives";
import { ApiError, describeError } from "@/lib/api/client";
import { api } from "@/lib/api/endpoints";
import type { DocumentoContrato, ResultadoVerificacion, SolicitudVerificacion, VistaExpediente } from "@/lib/api/types";
import { parseCurp, todayInMexicoCity } from "@/lib/kyc";
import { EXPIRED_LOGIN_PATH } from "@/lib/session";
import { useAuthStore } from "@/store/auth";

/*
 * Identity verification (KYC), inside the dashboard: the wizard behind the
 * "Complete verification" button, in four steps and the Sovereign Gold
 * identity (black and charcoal, square, a gold progress bar).
 *
 * 1. Identity: legal name, CURP, RFC (checked as typed), the date of birth
 *    and nationality read from the CURP, and the home address. Saved on its
 *    own (POST /kyc/perfil), so a customer resumes at step 2.
 * 2. Official ID: the INE's two sides (camera, upload or drag and drop,
 *    previewed at once) and its printed details. Passports are not offered
 *    yet.
 * 3. Liveness and funds: biometric consent, a live camera check (frames kept
 *    in memory only), and the declarations Mexican regulation requires:
 *    occupation, source of funds, politically exposed person.
 * 4. Review and sign: everything above with a way back to each part, the
 *    three agreements, and "Submit for Verification" (POST /kyc/verificar,
 *    the backend's real contract: nothing parallel).
 *
 * Then the status page: verified (in demo mode the simulated provider
 * answers at once), in review (a person approves it; the page checks every
 * 10 s), or not approved with the reasons and the attempts left. Once the
 * file is VERIFICADO the dashboard reloads its summary, so deposits,
 * withdrawals and trading unlock without a page reload.
 */

/** How often the status page asks for news while a file is in review. */
const REVIEW_POLL_MS = 10_000;

const DESCRIPTIONS: Record<View, string> = {
  1: "Exactly as on your INE. We check your CURP and RFC as you type.",
  2: "Photograph or upload both sides of your INE, and type the details printed on it.",
  3: "A short live check that you are the person on the INE, and three questions about you and your funds.",
  4: "Check everything, read and accept the agreements, and submit.",
  status: "Where your identity verification stands.",
};

function isAbort(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

/** Failures after which the submission may or may not have changed the file: reload it. */
function outcomeUnknown(error: unknown): boolean {
  if (!(error instanceof ApiError)) return true;
  return (
    error.code === "KYC_PROVEEDOR_ERROR" ||
    error.code === "TIEMPO_AGOTADO" ||
    error.code === "RED" ||
    error.status >= 500
  );
}

export default function VerifyPage() {
  const router = useRouter();
  const { token, client, refresh } = useDashboard();
  const [file, setFile] = useState<VistaExpediente | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [step, setStep] = useState<View>(1);
  const [ine, setIne] = useState<IneDraft>(EMPTY_INE);
  const [capture, setCapture] = useState<LivenessCapture | null>(null);
  const [consent, setConsent] = useState(false);
  const [declarations, setDeclarations] = useState<DeclarationsDraft>(() => declarationsFromFile(null));
  const [showDeclarationErrors, setShowDeclarationErrors] = useState(false);
  const [accepted, setAccepted] = useState<ReadonlySet<DocumentoContrato>>(new Set());
  const [immediate, setImmediate] = useState<ResultadoVerificacion | null>(null);
  const [checking, setChecking] = useState(false);
  const [lastChecked, setLastChecked] = useState<Date | null>(null);
  // Page-level, so it survives leaving the review: why the last submission was refused.
  const [notice, setNotice] = useState<SubmitNotice | null>(null);
  const [problem, setProblem] = useState<SubmitProblem | null>(null);
  // POST /kyc/verificar in flight: the progress bar is locked until it answers.
  const [submitting, setSubmitting] = useState(false);
  // The identity (CURP|RFC|voter key) last refused with IDENTIDAD_EN_USO.
  const [conflictKey, setConflictKey] = useState<string | null>(null);
  const [livenessMissing, setLivenessMissing] = useState(false);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const focusPending = useRef(false);
  const unlocked = useRef(false);

  const expire = useCallback(() => {
    useAuthStore.getState().expire();
    router.replace(EXPIRED_LOGIN_PATH);
  }, [router]);

  /** GET /kyc/expediente; null (and `loadError`) when it fails. */
  const fetchFile = useCallback(
    async (signal?: AbortSignal): Promise<VistaExpediente | null> => {
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

  // First load: resume where the customer left off (a retry starts from what was declared).
  useEffect(() => {
    const ctrl = new AbortController();
    void fetchFile(ctrl.signal).then((fresh) => {
      if (!fresh) return;
      setStep(resumeStep(fresh));
      setIne(ineDraftFromFile(fresh));
      setDeclarations(declarationsFromFile(fresh));
    });
    return () => ctrl.abort();
  }, [fetchFile]);

  // Verified: the dashboard reloads its summary once, which unlocks deposits,
  // withdrawals and trading everywhere without a page reload.
  useEffect(() => {
    if (file?.estado_kyc !== "VERIFICADO" || unlocked.current) return;
    unlocked.current = true;
    void refresh();
  }, [file?.estado_kyc, refresh]);

  const today = useMemo(() => todayInMexicoCity(), []);
  const curp = useMemo(() => {
    const parsed = parseCurp(file?.curp ?? "");
    return parsed.ok ? parsed.value : null;
  }, [file?.curp]);
  const declarationsCheck = useMemo(() => checkDeclarations(declarations), [declarations]);
  const credential = curp ? checkCredentialDraft(ine, curp, today).input : null;
  const ineReady = credential !== null && Object.keys(checkIneImages(ine)).length === 0;
  const livenessReady = capture !== null && consent && declarationsCheck.input !== null;
  const routing = file
    ? {
        estado_kyc: file.estado_kyc,
        profileSaved: isProfileSaved(file),
        ineReady,
        livenessReady,
        hasAttempt: file.intentos > 0 || file.ultimo_resultado !== null,
      }
    : null;
  const locked = notice?.lock ?? false;
  const current: View = locked ? "status" : routing ? effectiveStep(step, routing) : step;
  const identityConflict =
    conflictKey !== null && file !== null && conflictKey === identityKey(file.curp, file.rfc, ine.clave_elector);
  const suspended = client?.estatus_kyc === "SUSPENDIDO";

  const goTo = useCallback((next: View) => {
    focusPending.current = true;
    // A notice about a refused submission belongs to the status page; a lock stays until a reload.
    if (next !== "status") setNotice((n) => (n?.lock ? n : null));
    setProblem(null);
    setStep(next);
  }, []);

  // Move focus to the new view's heading so assistive technology announces it.
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
  const polling = current === "status" && file?.estado_kyc === "EN_REVISION";
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

  /** Step 3 -> 4: the liveness frames and all three declarations. */
  const continueToReview = () => {
    setShowDeclarationErrors(true);
    if (!capture || !consent) {
      setLivenessMissing(true);
      document.getElementById(consent ? "liveness-title" : "consentimiento")?.focus();
      return;
    }
    if (!declarationsCheck.input) {
      const first = (Object.keys(DECLARATION_INPUT_IDS) as DeclarationField[]).find((f) => declarationsCheck.errors[f]);
      if (first) document.getElementById(DECLARATION_INPUT_IDS[first])?.focus();
      return;
    }
    goTo(4);
  };

  const submit = useCallback(async () => {
    if (!file || !credential || !ine.frente || !ine.reverso) {
      setProblem({ message: "Complete the photos and details of your INE first.", action: "id" });
      return;
    }
    if (!capture || !consent) {
      setProblem({ message: "Complete the liveness check first.", action: "selfie" });
      return;
    }
    const contratos = acceptanceFor(file.contratos_vigentes, accepted);
    if (!declarationsCheck.input || !contratos) return;
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
      selfie: capture,
      consentimiento_biometrico: true,
      declaraciones: declarationsCheck.input,
      contratos,
    };
    setSubmitting(true);
    setProblem(null);
    try {
      const sent = await api.kycVerify(token, body);
      setNotice(null);
      setConflictKey(null);
      setImmediate(sent.resultado);
      // The frames did their job: nothing biometric stays in this tab. A retry takes new ones.
      setCapture(null);
      const fresh = await fetchFile();
      if (!fresh) {
        setFile((prev) =>
          prev
            ? {
                ...prev,
                estado_kyc: sent.estado_kyc,
                intentos: sent.intentos,
                intentos_restantes:
                  sent.resultado?.intentos_restantes ?? Math.max(0, prev.max_intentos - sent.intentos),
                verificacion_en_curso: sent.verificacion_id,
              }
            : prev,
        );
      }
      goTo("status");
    } catch (error) {
      if (error instanceof ApiError) {
        if (error.isUnauthorized) {
          expire();
          return;
        }
        const found = verificationProblem(error.code, error.message);
        if (found) {
          if (found.action === "identity") setConflictKey(identityKey(file.curp, file.rfc, credential.clave_elector));
          if (found.action === "agreements" && error.code === "KYC_CONTRATOS_DESACTUALIZADOS") {
            // A new version: read it again, and accept it again.
            setAccepted(new Set());
            await fetchFile();
          }
          if (found.action === "status") {
            setNotice({ message: found.message, lock: found.lock ?? false });
            await fetchFile();
            goTo("status");
            return;
          }
          setProblem({ message: found.message, action: found.action });
          return;
        }
      }
      const message = describeError(error);
      if (outcomeUnknown(error)) {
        // The submission may have been settled meanwhile (e.g. no refund): show the real state.
        const fresh = await fetchFile();
        if (fresh && fresh.estado_kyc !== "PENDIENTE") {
          setNotice({ message, lock: false });
          goTo("status");
          return;
        }
      }
      setProblem({ message, action: null });
    } finally {
      setSubmitting(false);
    }
  }, [token, file, credential, ine, capture, consent, declarationsCheck.input, accepted, fetchFile, goTo, expire]);

  // ---------------------------------------------------------------------------
  // Render
  // ---------------------------------------------------------------------------

  const header = (
    <header className="space-y-2">
      <p className="text-[11px] font-semibold tracking-[0.24em] text-synthetic-yellow uppercase">
        Identity verification
      </p>
      <div className="flex flex-wrap items-end justify-between gap-x-6 gap-y-2">
        <h2 className="text-2xl font-semibold tracking-tight text-ink sm:text-3xl">Verify your identity</h2>
        <p className="flex items-center gap-1.5 text-xs text-muted">
          <Clock className="size-3.5" aria-hidden />
          About 4 minutes
        </p>
      </div>
      <p className="max-w-2xl text-sm text-ink-2">
        Mexican financial regulation requires it before you can deposit, withdraw or invest. Keep your INE at hand.
      </p>
    </header>
  );

  if (!file) {
    return (
      <div className="mx-auto max-w-3xl space-y-6" data-testid="verify-page">
        {header}
        {loadError ? (
          <Alert tone="error" title="Could not load your verification file">
            <p>{loadError}</p>
            <Button
              type="button"
              variant="secondary"
              size="sm"
              className="mt-2"
              onClick={() => void checkNow()}
              loading={checking}
            >
              Retry
            </Button>
          </Alert>
        ) : (
          <div className="space-y-4" role="status" aria-busy="true">
            <span className="sr-only">Loading your verification file…</span>
            <Skeleton className="h-2 w-full" />
            <Skeleton className="h-96 w-full" />
          </div>
        )}
      </div>
    );
  }

  if (file.tipo_persona === "MORAL") {
    return (
      <div className="mx-auto max-w-3xl space-y-6" data-testid="verify-page">
        {header}
        <section
          data-testid="onboarding-moral"
          aria-labelledby="moral-title"
          className="space-y-4 border border-border bg-surface p-7 text-center"
        >
          <Building2 className="mx-auto size-8 text-synthetic-yellow" aria-hidden />
          <h2 id="moral-title" className="text-lg font-semibold">
            Remote verification is for individuals
          </h2>
          <p className="text-sm text-ink-2">{MORAL_EXPLANATION}</p>
          <KycStatusBadge estado={file.estado_kyc} />
        </section>
      </div>
    );
  }

  const title = current === "status" ? "Your verification" : (STEPS.find((s) => s.n === current)?.title ?? "");
  const lastRejected =
    canRetry(file) && file.ultimo_resultado?.resultado === "RECHAZADO" ? file.ultimo_resultado : null;

  return (
    <div className="mx-auto max-w-3xl space-y-6" data-testid="verify-page">
      {header}
      {routing ? (
        <Progress
          current={current}
          canVisit={(s: Step) => !submitting && !locked && canVisitStep(s, routing)}
          // A file created in a final state (administrative, no attempt) completed none of the steps.
          isDone={(s: Step) => (current === "status" ? routing.hasAttempt : s < current)}
          onVisit={goTo}
        />
      ) : null}
      {loadError ? <Alert tone="warning">Could not refresh your file: {loadError}</Alert> : null}

      <StepFrame
        key={String(current)}
        view={current}
        title={title}
        description={DESCRIPTIONS[current]}
        headingRef={headingRef}
      >
        {current === 1 ? (
          <StepPersonal
            token={token}
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
            onViewStatus={() => goTo("status")}
            onBack={() => goTo(1)}
            onContinue={() => goTo(3)}
          />
        ) : null}
        {current === 3 && ine.frente && ine.reverso ? (
          <div className="space-y-8">
            <StepLiveness
              ineImages={{ frente: ine.frente, reverso: ine.reverso }}
              captured={capture}
              initialConsent={consent}
              onConsentChange={setConsent}
              onCaptured={(c) => {
                setCapture(c);
                setLivenessMissing(false);
              }}
              onRetake={() => setCapture(null)}
            />
            <div className="h-px bg-border" aria-hidden />
            <StepDeclarations
              draft={declarations}
              errors={showDeclarationErrors ? declarationsCheck.errors : {}}
              onChange={setDeclarations}
            />
            {livenessMissing && !capture ? (
              <Alert tone="error">Complete the liveness check above before you continue.</Alert>
            ) : null}
            <div className="flex gap-3">
              <Button type="button" variant="secondary" size="lg" onClick={() => goTo(2)}>
                <ArrowLeft className="size-4" aria-hidden />
                Back
              </Button>
              <Button
                type="button"
                size="lg"
                className="flex-1"
                onClick={continueToReview}
                data-testid="step3-continue"
              >
                Continue to review
                <ArrowRight className="size-4" aria-hidden />
              </Button>
            </div>
          </div>
        ) : null}
        {current === 4 && capture && declarationsCheck.input ? (
          <StepReview
            file={file}
            ine={ine}
            capture={capture}
            declarations={declarationsCheck.input}
            accepted={accepted}
            onAccept={(id, on) =>
              setAccepted((prev) => {
                const next = new Set(prev);
                if (on) next.add(id);
                else next.delete(id);
                return next;
              })
            }
            onEdit={goTo}
            onSubmit={() => void submit()}
            submitting={submitting}
            problem={problem}
            identityConflict={identityConflict}
          />
        ) : null}
        {current === "status" ? (
          <StepStatus
            file={file}
            immediate={immediate}
            notice={notice}
            suspended={suspended}
            polling={{ active: polling, checking, lastChecked, onCheckNow: () => void checkNow() }}
            onTryAgain={() => goTo(2)}
            onResume={() => goTo(resumeStep(file))}
            onDashboard={() => router.push("/dashboard")}
          />
        ) : null}
      </StepFrame>
    </div>
  );
}
