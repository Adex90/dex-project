"use client";

import { FileSearch, LockKeyhole, UserRound } from "lucide-react";

import { Facts, MxTime, Section } from "@/components/admin/aml/parts";
import { Money, formatDate } from "@/components/ui/data";
import { Badge, Button, type Tone } from "@/components/ui/primitives";
import type { ClienteAml, CuentasAml, EstadoCuentaInternaAml } from "@/lib/api/types";
import { ACCOUNT_STATE_LABEL, KYC_STATUS_LABEL, LEVEL_LABEL } from "@/lib/aml";
import { entidadName } from "@/lib/kyc";

const ACCOUNT_TONES: Record<EstadoCuentaInternaAml, Tone> = {
  ACTIVA: "verified",
  CONGELADA: "danger",
  CONGELADO_PREVENTIVO: "danger",
  CERRADA: "neutral",
};

function AccountState({ estado }: { estado: EstadoCuentaInternaAml }) {
  return <Badge tone={ACCOUNT_TONES[estado]}>{ACCOUNT_STATE_LABEL[estado]}</Badge>;
}

function formatClabe(clabe: string): string {
  return `${clabe.slice(0, 3)} ${clabe.slice(3, 6)} ${clabe.slice(6, 17)} ${clabe.slice(17)}`;
}

/**
 * KYC identity, risk profile and accounts of the client, with the accounts'
 * real state (staff only: clients see a preventive restriction as ACTIVA).
 */
export function ClientPanel({
  cliente,
  cuentas,
  onFreezeClient,
  onOpenReview,
}: {
  cliente: ClienteAml;
  cuentas: CuentasAml;
  /** Opens the manual-freeze dialog; absent when the client is already restricted or the viewer cannot act. */
  onFreezeClient?: () => void;
  /** Opens the full client review (aggregates, orders, contracts, every alert and restriction). */
  onOpenReview?: () => void;
}) {
  const d = cliente.domicilio;
  const address = [d.municipio, d.estado ? (entidadName(d.estado) ?? d.estado) : null, d.codigo_postal].filter(Boolean).join(", ");
  const { ifpe, sofom } = cuentas;

  return (
    <Section
      title="Client"
      icon={<UserRound className="size-3.5" aria-hidden />}
      action={
        onFreezeClient || onOpenReview ? (
          <span className="flex flex-wrap items-center justify-end gap-2">
            {onOpenReview ? (
              <Button variant="ghost" size="sm" onClick={onOpenReview}>
                <FileSearch className="size-3.5" aria-hidden />
                Open client review
              </Button>
            ) : null}
            {onFreezeClient ? (
              <Button variant="danger" size="sm" onClick={onFreezeClient}>
                <LockKeyhole className="size-3.5" aria-hidden />
                Freeze client
              </Button>
            ) : null}
          </span>
        ) : null
      }
    >
      <div className="flex flex-wrap items-center gap-2">
        <p className="text-base font-semibold text-ink">{cliente.nombre_completo}</p>
        {cliente.es_interno ? <Badge tone="accent">Staff</Badge> : null}
        {cliente.es_pep ? <Badge tone="warning">PEP</Badge> : null}
        {cliente.en_lista_bloqueadas ? <Badge tone="danger">Blocked-persons list</Badge> : null}
        <Badge tone={cliente.nivel_riesgo_pld === "ALTO" ? "danger" : cliente.nivel_riesgo_pld === "MEDIO" ? "warning" : "neutral"}>
          {LEVEL_LABEL[cliente.nivel_riesgo_pld]} risk
        </Badge>
      </div>
      <div className="grid gap-4 lg:grid-cols-2">
        <Facts
          rows={[
            ["Type", cliente.tipo_persona === "FISICA" ? "Individual" : "Company"],
            ["Email", <span key="e" className="break-all">{cliente.email}</span>],
            ["CURP", cliente.curp ? <span key="c" className="font-mono">{cliente.curp}</span> : null],
            ["RFC", cliente.rfc ? <span key="r" className="font-mono">{cliente.rfc}</span> : null],
            ["Birth date", cliente.fecha_nacimiento ? formatDate(cliente.fecha_nacimiento) : null],
            ["Nationality", cliente.nacionalidad],
            ["Residence", cliente.pais_residencia],
            ["Birth state", cliente.entidad_nacimiento ? (entidadName(cliente.entidad_nacimiento) ?? cliente.entidad_nacimiento) : null],
            ["Address", address || null],
          ]}
        />
        <Facts
          rows={[
            ["Economic activity", cliente.actividad_economica],
            ["Occupation", cliente.ocupacion],
            [
              "Declared monthly income",
              cliente.ingreso_mensual_estimado ? <Money key="i" value={cliente.ingreso_mensual_estimado} /> : null,
            ],
            [
              "Risk tier",
              <span key="t">
                {LEVEL_LABEL[cliente.nivel_riesgo_pld]}
                {cliente.riesgo_evaluado_en ? (
                  <span className="text-muted">
                    {" "}
                    · reviewed <MxTime at={cliente.riesgo_evaluado_en} style="date" />
                  </span>
                ) : null}
              </span>,
            ],
            ["PEP", cliente.es_pep ? "Yes" : "No"],
            ["Blocked-persons list", cliente.en_lista_bloqueadas ? "Yes" : "No"],
            [
              "KYC status",
              <span key="k">
                {KYC_STATUS_LABEL[cliente.estatus_kyc]}
                {cliente.kyc_verificado_en ? (
                  <span className="text-muted">
                    {" "}
                    · <MxTime at={cliente.kyc_verificado_en} style="date" />
                  </span>
                ) : null}
              </span>,
            ],
            ["Client since", <MxTime key="s" at={cliente.cliente_desde} style="date" />],
            ["Staff member", cliente.es_interno ? "Yes (holds a staff credential)" : "No"],
          ]}
        />
      </div>

      <div className="grid gap-3 sm:grid-cols-2">
        <div className="space-y-2 rounded-lg border border-border p-3">
          <div className="flex items-center justify-between gap-2">
            <p className="text-xs font-semibold text-ink">Vault (IFPE)</p>
            {ifpe ? <AccountState estado={ifpe.estado} /> : null}
          </div>
          {ifpe ? (
            <Facts
              className="text-xs"
              rows={[
                ["CLABE", <span key="c" className="font-mono">{formatClabe(ifpe.clabe)}</span>],
                ["Available", <Money key="a" value={ifpe.saldo_disponible} />],
                ["Blocked", <Money key="b" value={ifpe.saldo_bloqueado} />],
                ["Held by compliance", <Money key="h" value={ifpe.retenido_aml} />],
              ]}
            />
          ) : (
            <p className="text-xs text-muted">No vault account.</p>
          )}
        </div>
        <div className="space-y-2 rounded-lg border border-border p-3">
          <div className="flex items-center justify-between gap-2">
            <p className="text-xs font-semibold text-ink">Fixed term (SOFOM)</p>
            {sofom ? <AccountState estado={sofom.estado} /> : null}
          </div>
          {sofom ? (
            <Facts
              className="text-xs"
              rows={[
                ["Available", <Money key="a" value={sofom.saldo_disponible} />],
                ["Blocked", <Money key="b" value={sofom.saldo_bloqueado} />],
              ]}
            />
          ) : (
            <p className="text-xs text-muted">No fixed-term account.</p>
          )}
        </div>
      </div>
    </Section>
  );
}
