"use client";

import { useState } from "react";
import { FileCheck2 } from "lucide-react";

import { Section } from "@/components/admin/aml/parts";
import { useSubmit } from "@/components/admin/aml/use-submit";
import { Alert, Button, Field, Input, fieldDescribedBy } from "@/components/ui/primitives";
import { api } from "@/lib/api/endpoints";
import type { AlertaAml, ReporteUifAml } from "@/lib/api/types";
import { mexicoCityDate, validateUifFolio } from "@/lib/aml";

/**
 * Records the folio the UIF (SITI) gave the report of a CONFIRMADO alert, and
 * the date it was filed. It can be recorded once; the backend checks the date
 * window in Mexico City time as this form does.
 */
export function UifFolioForm({
  alerta,
  token,
  onDone,
}: {
  alerta: AlertaAml;
  token: string;
  onDone: (message: string, result: ReporteUifAml) => void;
}) {
  const [folio, setFolio] = useState("");
  const [fecha, setFecha] = useState(() => mexicoCityDate(new Date()));
  const [touched, setTouched] = useState(false);
  const { busy, error, run } = useSubmit();
  const problem = validateUifFolio(folio, fecha, alerta);

  const submit = async () => {
    setTouched(true);
    if (problem) return;
    const result = await run((clave_idempotencia) =>
      api.amlRecordUifReport(token, alerta.id, { folio_uif: folio.trim(), reportada_en: fecha, clave_idempotencia }),
    );
    if (result) onDone(`UIF folio ${result.alerta.folio_uif ?? folio.trim()} recorded on alert #${alerta.folio}.`, result);
  };

  const hint = "Capital letters, digits and hyphens, as issued by SITI.";
  return (
    <Section title="UIF report" icon={<FileCheck2 className="size-3.5" aria-hidden />}>
      <form
        className="grid gap-3 sm:grid-cols-[1fr_12rem_auto] sm:items-start"
        onSubmit={(e) => {
          e.preventDefault();
          void submit();
        }}
      >
        <Field label="UIF folio" htmlFor={`uif-folio-${alerta.id}`} hint={hint} error={touched ? problem : null}>
          <Input
            id={`uif-folio-${alerta.id}`}
            value={folio}
            onChange={(e) => setFolio(e.target.value.toUpperCase())}
            placeholder="SITI-2026-0001"
            autoComplete="off"
            invalid={touched && Boolean(problem)}
            aria-describedby={fieldDescribedBy(`uif-folio-${alerta.id}`, touched ? problem : null, hint)}
            disabled={busy}
          />
        </Field>
        <Field label="Filed on (Mexico City)" htmlFor={`uif-date-${alerta.id}`}>
          <Input id={`uif-date-${alerta.id}`} type="date" value={fecha} onChange={(e) => setFecha(e.target.value)} disabled={busy} />
        </Field>
        <Button type="submit" loading={busy} className="sm:mt-[22px]">
          Record UIF folio
        </Button>
      </form>
      {error ? <Alert tone="error">{error}</Alert> : null}
    </Section>
  );
}
