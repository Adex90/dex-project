/**
 * DTOs returned by the Rust backend (backend/src/models, backend/src/routes).
 * Amounts (`Decimal`) are strings with 8 decimals; timestamps are ISO-8601
 * UTC; dates (`IsoDate`) are "YYYY-MM-DD".
 */

export type Decimal = string;
export type Uuid = string;
export type IsoDateTime = string;
export type IsoDate = string;

export type Rol = "CLIENTE" | "OPERADOR" | "AUDITOR" | "CUMPLIMIENTO";
export type TipoPersona = "FISICA" | "MORAL";
export type EstatusKyc = "PENDIENTE" | "EN_REVISION" | "VERIFICADO" | "RECHAZADO" | "SUSPENDIDO";
export type NivelRiesgo = "BAJO" | "MEDIO" | "ALTO";
/** Client view of an account state. Clients never receive CONGELADO_PREVENTIVO (it arrives as ACTIVA). */
export type EstadoCuenta = "ACTIVA" | "CONGELADA" | "CERRADA";

export interface ApiErrorBody {
  error: { codigo: string; mensaje: string };
}

export interface Sesion {
  token: string;
  expira_en: IsoDateTime;
  cliente_id: Uuid;
  rol: Rol;
}

/**
 * POST /api/v1/auth/login, and the steps that complete a session
 * (verify-mfa, mfa/enable). Staff never get a session from a password alone.
 */
export type ResultadoLogin =
  | ({ estado: "OK" } & Sesion)
  /** Password accepted; the authenticator code is still missing (POST /auth/verify-mfa). */
  | { estado: "2FA_REQUIRED"; token_mfa: string; expira_en: IsoDateTime }
  /** Staff without two-step verification: enroll it first (mfa/setup, mfa/enable). */
  | { estado: "MFA_ENROLLMENT_REQUIRED"; token_inscripcion: string; expira_en: IsoDateTime };

/** POST /api/v1/auth/mfa/setup. Shown once: the secret is never sent again. */
export interface ConfiguracionMfa {
  /** Base32, for typing into the app when the QR code cannot be scanned. */
  secreto: string;
  otpauth_url: string;
  /** The QR code of `otpauth_url`, as an SVG data URL. */
  qr_svg: string;
  emisor: string;
  cuenta: string;
  digitos: number;
  periodo_seg: number;
}

/**
 * The client's own view (`ClienteVista`): no risk tier, PEP flag or risk
 * review date. Those are staff-only (see `ClienteAml`).
 */
export interface Cliente {
  id: Uuid;
  tipo_persona: TipoPersona;
  nombre: string;
  apellido_paterno: string | null;
  apellido_materno: string | null;
  email: string;
  rfc: string | null;
  curp: string | null;
  nivel_cuenta: number;
  estatus_kyc: EstatusKyc;
  kyc_verificado_en: IsoDateTime | null;
  creado_en: IsoDateTime;
}

export interface RegistroInput {
  tipo_persona: TipoPersona;
  nombre: string;
  apellido_paterno?: string;
  apellido_materno?: string;
  email: string;
  password: string;
  curp?: string;
  rfc?: string;
}

export interface RegistroRespuesta {
  sesion: Sesion;
  cliente: Cliente;
  cuenta_ifpe: { id: Uuid; clabe: string };
}

export interface CuentaIfpe {
  id: Uuid;
  cliente_id: Uuid;
  tipo_entidad: "IFPE";
  clabe: string;
  moneda: string;
  saldo_disponible: Decimal;
  saldo_bloqueado: Decimal;
  estado: EstadoCuenta;
  creado_en: IsoDateTime;
  actualizado_en: IsoDateTime;
}

export interface CuentaSofom {
  id: Uuid;
  cliente_id: Uuid;
  tipo_entidad: "SOFOM";
  numero_cuenta: number;
  moneda: string;
  saldo_disponible: Decimal;
  saldo_bloqueado: Decimal;
  estado: EstadoCuenta;
}

export type EstadoMutuo = "VIGENTE" | "VENCIDO" | "LIQUIDADO";

export interface ContratoMutuo {
  id: Uuid;
  folio: number;
  cliente_id: Uuid;
  cuenta_sofom_id: Uuid;
  cuenta_ifpe_origen_id: Uuid;
  tx_aportacion_ifpe_id: Uuid;
  principal: Decimal;
  tasa_anual: Decimal;
  plazo_dias: number;
  fecha_inicio: IsoDate;
  fecha_vencimiento: IsoDate;
  principal_asignado: Decimal;
  interes_pagado: Decimal;
  estado: EstadoMutuo;
  hash_terminos: string;
  creado_en: IsoDateTime;
}

export interface Posicion {
  cuenta: CuentaSofom | null;
  contratos: ContratoMutuo[];
  total_invertido: Decimal;
  intereses_por_recibir: Decimal;
}

export interface EstadoReservaVista {
  pasivo_clientes: Decimal;
  activo_reserva: Decimal;
  coeficiente_reserva: Decimal;
  reserva_100: boolean;
  custodia: { institucion: string; tipo: string; saldo: Decimal }[];
  ledger: { ultima_secuencia: number; ultimo_hash: string };
  actualizado_en: IsoDateTime;
}

export interface Tablero {
  cliente: Cliente;
  boveda: CuentaIfpe | null;
  inversion: Posicion;
  reserva: EstadoReservaVista;
}

export type TipoTxIfpe =
  | "DEPOSITO"
  | "RETIRO"
  | "TRANSFERENCIA"
  | "TRASPASO_A_SOFOM"
  | "RETORNO_DE_SOFOM"
  | "RETENCION"
  | "LIBERACION_RETENCION";

export interface TransaccionIfpe {
  id: Uuid;
  secuencia: number;
  tipo: TipoTxIfpe;
  cuenta_origen_id: Uuid | null;
  cuenta_destino_id: Uuid | null;
  reserva_id: Uuid | null;
  monto: Decimal;
  moneda: string;
  concepto: string | null;
  referencia_externa: string | null;
  clave_idempotencia: string;
  ref_movimiento_espejo: Uuid | null;
  hash_anterior: string;
  tx_hash: string;
  creado_en: IsoDateTime;
}

export interface TransaccionSofom {
  id: Uuid;
  secuencia: number;
  tipo: string;
  monto: Decimal;
  tx_hash: string;
  hash_anterior: string;
  ref_movimiento_espejo: Uuid | null;
  creado_en: IsoDateTime;
}

export interface Movimiento {
  transaccion: TransaccionIfpe;
  idempotente: boolean;
}

export interface PlazoTasa {
  plazo_dias: number;
  tasa_anual: Decimal;
  etiqueta: string;
}

export interface TerminosMutuo {
  version: string;
  principal: string;
  tasa_anual: string;
  plazo_dias: number;
  fecha_inicio: IsoDate;
  fecha_vencimiento: IsoDate;
  convencion: string;
  retiro_anticipado: boolean;
  hash: string;
}

export interface ResultadoMutuo {
  contrato: ContratoMutuo;
  terminos: TerminosMutuo;
  interes_al_vencimiento: Decimal;
  tx_ifpe: TransaccionIfpe;
  tx_sofom: TransaccionSofom;
  idempotente: boolean;
}

export interface ResultadoLiquidacion {
  contrato: ContratoMutuo;
  interes: Decimal;
  total_retornado: Decimal;
  tx_ifpe: TransaccionIfpe;
  idempotente: boolean;
}

export interface CorteReserva {
  id: Uuid;
  merkle_root: string;
  num_hojas: number;
  total_pasivo: Decimal;
  total_reserva: Decimal;
  secuencia_ledger: number;
  hash_ledger: string;
  algoritmo: string;
  creado_en: IsoDateTime;
}

export type LadoMerkle = "IZQUIERDA" | "DERECHA";

export interface PruebaReserva {
  corte: CorteReserva;
  indice: number;
  hoja: { cuenta_id: Uuid; saldo: Decimal; sal: string; hash: string | null };
  ruta: { lado: LadoMerkle; hash: string; suma: Decimal }[];
  valida: boolean;
  saldo_actual: Decimal;
}

export interface ResultadoVerificacionPrueba {
  valida: boolean;
  raiz_calculada: string;
  suma_calculada: Decimal;
  raiz_publicada: string;
  total_pasivo_publicado: Decimal;
  total_reserva_publicado: Decimal;
}

// -----------------------------------------------------------------------------
// Digital onboarding / KYC (backend/src/routes/kyc.rs, backend/src/kyc/*.rs).
// Messages (`mensaje`, `advertencias`, `detalles[].mensaje`, `motivo_rechazo`)
// are Spanish: translate them with `kycMessageInEnglish` (src/lib/kyc.ts).
// -----------------------------------------------------------------------------

/** State of the KYC file (`kyc.kyc_perfiles.estado_kyc`). */
export type EstadoKyc = "PENDIENTE" | "EN_REVISION" | "VERIFICADO" | "RECHAZADO";
/** H: male, M: female, X: non-binary (as in the CURP). */
export type Sexo = "H" | "M" | "X";
/** Automatic verdict on a biometric result. */
export type ResultadoKyc = "APROBADO" | "REVISION" | "RECHAZADO";

/** Address of step 1. `codigo_postal` has 5 digits. */
export interface Domicilio {
  calle: string;
  numero_exterior: string;
  numero_interior?: string | null;
  colonia: string;
  municipio: string;
  estado: string;
  codigo_postal: string;
}

/**
 * Body of POST /kyc/perfil (unknown fields are rejected). Birth date, sex and
 * birth state are derived from the CURP by the backend.
 */
export interface DatosPersonalesInput {
  nombres: string;
  apellido_paterno: string;
  apellido_materno?: string | null;
  curp: string;
  rfc: string;
  domicilio: Domicilio;
}

/** One finding of a verification: errors reject it, warnings send it to human review. */
export interface DetalleKyc {
  tipo: "ERROR" | "ADVERTENCIA";
  mensaje: string;
}

/** The KYC file as stored (`Expediente`; no IP or image fingerprints). */
export interface ExpedienteKyc {
  cliente_id: Uuid;
  nombres: string | null;
  apellido_paterno: string | null;
  apellido_materno: string | null;
  apellidos: string | null;
  curp: string | null;
  rfc: string | null;
  fecha_nacimiento: IsoDate | null;
  sexo: Sexo | null;
  /** RENAPO code ("DF", "NE"...). */
  entidad_nacimiento: string | null;
  nacionalidad: string;
  /** `{}` until step 1 is saved. */
  domicilio: Partial<Domicilio>;
  ine_clave_elector: string | null;
  ine_cic: string | null;
  ine_identificador_ciudadano: string | null;
  ine_emision: number | null;
  ine_vigencia: number | null;
  estado_kyc: EstadoKyc;
  /** Submissions so far (each POST /kyc/verificar that reaches the provider counts). */
  intentos: number;
  verificacion_en_curso: Uuid | null;
  enviado_en: IsoDateTime | null;
  metadata_proveedor: Record<string, unknown>;
  /** Spanish; set when the file is RECHAZADO. */
  motivo_rechazo: string | null;
  verificado_en: IsoDateTime | null;
  revisado_por: Uuid | null;
  creado_en: IsoDateTime;
  actualizado_en: IsoDateTime;
}

/** Last biometric result of the file (`kyc.kyc_auditoria_biometrica`). */
export interface ResumenResultado {
  verificacion_id: Uuid;
  proveedor: string;
  /** Decimal string such as "0.9300". */
  score_similitud: Decimal | null;
  prueba_vida_exitosa: boolean;
  documento_valido: boolean;
  coincide_identidad: boolean;
  resultado: ResultadoKyc;
  /** Hidden from clients (null): only compliance sees the threshold and the exact score. */
  umbral_aplicado: Decimal | null;
  timestamp: IsoDateTime;
  detalles_fallo: DetalleKyc[];
}

/** GET /kyc/expediente. */
/** Why a PENDIENTE file cannot be submitted (same rules the database applies on submission). */
export type BloqueoKyc =
  /** All attempts used. */
  | "SIN_INTENTOS"
  /** Too many submissions with an identity registered to another account (30 days). */
  | "CONFLICTOS_IDENTIDAD"
  /** The CURP has rejected verifications in other accounts (30 days). */
  | "IDENTIDAD_BLOQUEADA";

export interface VistaExpediente extends ExpedienteKyc {
  tipo_persona: TipoPersona;
  /** The account status (`estatus_kyc` of the client): mirrors the file, except SUSPENDIDO. */
  estatus_cliente: EstatusKyc;
  max_intentos: number;
  /** 0 while `bloqueo` is set. */
  intentos_restantes: number;
  bloqueo: BloqueoKyc | null;
  ultimo_resultado: ResumenResultado | null;
}

/** Response of POST /kyc/perfil. */
export interface DatosGuardados {
  expediente: ExpedienteKyc;
  /** Non-blocking findings (Spanish) that will send the file to human review. */
  advertencias: string[];
}

/** Credential data and photos (`ine` of POST /kyc/verificar). */
export interface DatosIneInput {
  /** 18 characters: 6 letters, birth date YYMMDD, state, sex and 3 digits. */
  clave_elector: string;
  /** 9 digits. */
  cic: string;
  /** 9 or 13 digits (optional). */
  identificador_ciudadano?: string | null;
  /** Issue year. */
  emision: number;
  /** Expiry year (valid through December 31). */
  vigencia: number;
  /** Data URL (JPEG/PNG/WebP, 1 KB to 3 MB decoded). */
  frente: string;
  reverso: string;
}

export interface SelfieInput {
  /** 3 to 8 distinct camera frames (data URLs). */
  frames: string[];
  /** Liveness challenge shown to the customer (informative). */
  reto?: string | null;
}

/** Body of POST /kyc/verificar (unknown fields are rejected; 12 MB body limit). */
export interface SolicitudVerificacion {
  ine: DatosIneInput;
  selfie: SelfieInput;
  /** Explicit consent to process biometric data; without it the API answers KYC_SIN_CONSENTIMIENTO. */
  consentimiento_biometrico: true;
}

/** Verdict on a verification (POST /kyc/verificar and the provider webhook). */
export interface ResultadoVerificacion {
  cliente_id: Uuid;
  verificacion_id: Uuid;
  /** APROBADO -> VERIFICADO; REVISION -> stays EN_REVISION; RECHAZADO -> PENDIENTE (retry) or RECHAZADO (no attempts left). */
  resultado: ResultadoKyc;
  estado_kyc: EstadoKyc;
  score_similitud: Decimal | null;
  detalles: DetalleKyc[];
  intentos: number;
  intentos_restantes: number;
  /** The same result had already been processed. */
  idempotente: boolean;
}

/**
 * Response of POST /kyc/verificar: 200 with `resultado`, or 202 with
 * `resultado: null` when the provider answers later (poll GET /kyc/expediente).
 */
export interface EnvioVerificacion {
  verificacion_id: Uuid;
  estado_kyc: EstadoKyc;
  intentos: number;
  resultado: ResultadoVerificacion | null;
}

// -----------------------------------------------------------------------------
// AML/PLD compliance console (backend/src/routes/aml.rs, /api/v1/aml). Staff
// only: CUMPLIMIENTO (full) and AUDITOR (read-only). The JSON bodies are built
// by SQL functions (aml.*_json); these shapes mirror them exactly. Scores and
// points (`puntaje`, `puntos`) are strings with 2 decimals ("100.00").
// -----------------------------------------------------------------------------

export type NivelRiesgoAml = "BAJO" | "MEDIO" | "ALTO" | "CRITICO";
export type EstadoAlertaAml = "PENDIENTE" | "EN_REVISION" | "CONFIRMADO" | "FALSO_POSITIVO";
export type TipoReporteAml = "INUSUAL" | "INTERNA_PREOCUPANTE";
export type OrigenAlertaAml = "EN_LINEA" | "INTENTO_BLOQUEADO" | "LOTE" | "DIFERIDA" | "MANUAL" | "LISTA_BLOQUEADAS" | "LEGADO";
export type DecisionAlertaAml = "REGISTRADA" | "RETENIDA" | "BLOQUEADA";
export type EntidadAml = "IFPE" | "SOFOM";
export type SentidoAlertaAml = "ENTRADA" | "SALIDA" | "NINGUNO";
export type SentidoMovimientoAml = "ENTRADA" | "SALIDA";
export type CanalAml = "SPEI_WEBHOOK" | "SPEI_OPERADOR" | "SPEI_DEMO" | "TRANSFERENCIA_RECIBIDA" | "TRANSFERENCIA_ENVIADA"
  | "SPEI_ENVIADO" | "INVERSION_MUTUO" | "DESEMBOLSO_CREDITO" | "COBRO_CREDITO";
export type EstadoCongelamientoAml = "ACTIVO" | "LEVANTADO";
export type OrigenCongelamientoAml = "AUTOMATICO" | "LOTE" | "MANUAL" | "LISTA_BLOQUEADAS";
export type TipoSolicitudAml = "LEVANTAR_Y_LIBERAR" | "LEVANTAR_CONSERVANDO_RETENCIONES" | "LIBERAR_RETENCIONES";
export type EstadoSolicitudAml = "PENDIENTE" | "APROBADA" | "RECHAZADA" | "ANULADA";
export type EstadoRetencionAml = "VIGENTE" | "LIBERADA";
export type ResultadoEvaluacionAml = "PERMITIDA" | "ALERTADA" | "RETENIDA" | "BLOQUEADA" | "PENDIENTE_REEVALUACION" | "REEVALUADA";
export type OrigenEvaluacionAml = "EN_LINEA" | "DIFERIDA" | "REEVALUACION";
export type AccionResolucionAml = "TOMAR" | "LIBERAR" | "ESCALAR" | "CONFIRMAR" | "FALSO_POSITIVO";
export type EstadoCuentaInternaAml = "ACTIVA" | "CONGELADA" | "CONGELADO_PREVENTIVO" | "CERRADA"; // staff view only
export type TipoNotificacionAml = "ALERTA_NUEVA" | "ALERTA_CRITICA" | "CONGELAMIENTO" | "SOLICITUD_LEVANTAMIENTO"
  | "LEVANTAMIENTO_DECIDIDO" | "ESCALAMIENTO" | "SLA_CONGELAMIENTO" | "PLAZO_UIF_PROXIMO" | "PLAZO_UIF_VENCIDO"
  | "COBERTURA" | "EVALUACION_FALLIDA" | "CARGO_EN_CUENTA_CONGELADA" | "PROPUESTA_PENDIENTE";
export type PrioridadAml = "ALTA" | "MEDIA";
export type ActorTipoAml = "SISTEMA" | "CUMPLIMIENTO" | "AUDITOR" | "OPERADOR" | "MIGRACION";
export type NivelClienteReglaAml = "BAJO" | "MEDIO" | "ALTO" | "TODOS";
export type ClaseReglaAml = "RELEVANTE" | "PRIMARIA" | "MODIFICADOR" | "LOTE" | "SISTEMA" | "LEGADO";
export type SentidoReglaAml = "ENTRADA" | "SALIDA" | "AMBOS" | "NINGUNO";
export type TipoPropuestaAml = "REGLA" | "PARAMETROS";
export type EstadoPropuestaAml = "PENDIENTE" | "APROBADA" | "RECHAZADA";

/** A person (client or officer) by id and display name: kyc.clientes nombre || apellidos. */
export interface PersonaRefAml { id: Uuid; nombre_completo: string }

/** Keys of aml.evaluaciones.contraparte. */
export interface ContraparteAml { nombre: string | null; cuenta: string | null; rfc_curp: string | null; cliente_id: Uuid | null }

// ---------- builders (§2.6.6) ----------
/** aml.alerta_json(id): one feed row. No `detalles` (case file only). */
export interface AlertaAml {
  id: Uuid; folio: number;
  estado: EstadoAlertaAml; nivel_riesgo: NivelRiesgoAml; puntaje: Decimal;
  tipo_reporte: TipoReporteAml; origen: OrigenAlertaAml; decision: DecisionAlertaAml;
  regla: { id: Uuid; codigo: string; version: number; nombre: string };
  cliente: { id: Uuid; nombre_completo: string; nivel_riesgo_pld: "BAJO" | "MEDIO" | "ALTO"; es_pep: boolean; es_interno: boolean };
  entidad: EntidadAml; sentido: SentidoAlertaAml; transaccion_id: Uuid | null;
  monto: Decimal | null; monto_acumulado: Decimal | null; ocurrencias: number;
  timestamp: IsoDateTime; ultima_ocurrencia_en: IsoDateTime; actualizado_en: IsoDateTime;
  fecha_limite_reporte: IsoDateTime; urgente: boolean;
  congelamiento: { id: Uuid; folio: number; estado: EstadoCongelamientoAml; confirmado: boolean; sla_vence_en: IsoDateTime } | null;
  asignada_a: PersonaRefAml | null; tomada_en: IsoDateTime | null; escalada: boolean;
  resolucion_propuesta: "FALSO_POSITIVO" | null; propuesta_por: PersonaRefAml | null;
  dictamen: string | null; resuelta_por: PersonaRefAml | null; resuelta_en: IsoDateTime | null; aprobada_por: PersonaRefAml | null;
  folio_uif: string | null; reportada_uif_en: IsoDate | null;
}

/** aml.retencion_json(id) */
export interface RetencionAml {
  id: Uuid; congelamiento_id: Uuid; cuenta_id: Uuid; evaluacion_id: Uuid;
  transaccion_origen_id: Uuid; tx_retencion_id: Uuid; monto: Decimal; estado: EstadoRetencionAml;
  solicitud_id: Uuid | null; tx_liberacion_id: Uuid | null; creado_en: IsoDateTime; liberada_en: IsoDateTime | null;
}

/** aml.solicitud_json(id) */
export interface SolicitudAml {
  id: Uuid; folio: number; congelamiento_id: Uuid; cliente: PersonaRefAml;
  tipo: TipoSolicitudAml; justificacion: string;
  solicitada_por: PersonaRefAml; solicitada_en: IsoDateTime;
  estado: EstadoSolicitudAml; decidida_por: PersonaRefAml | null; decidida_en: IsoDateTime | null; comentario: string | null;
}

/** aml.congelamiento_json(id) */
export interface CongelamientoAml {
  id: Uuid; folio: number; cliente: PersonaRefAml; cuenta_ifpe_id: Uuid | null;
  estado: EstadoCongelamientoAml; origen: OrigenCongelamientoAml;
  alerta_id: Uuid | null; evaluacion_id: Uuid | null; puntaje: Decimal | null; motivo: string; confirmado: boolean;
  congelado_por: PersonaRefAml | null;            // null = system
  congelado_en: IsoDateTime; sla_vence_en: IsoDateTime;
  intentos_bloqueados: number; ultimo_intento_en: IsoDateTime | null;
  levantado_por: PersonaRefAml | null; levantado_en: IsoDateTime | null; solicitud_id: Uuid | null;
  retenido_vigente: Decimal;                      // Σ VIGENTE holds of this freeze, "0.00000000" if none
  retenciones: RetencionAml[];                    // creado_en ascending
  solicitud_pendiente: SolicitudAml | null;
  actualizado_en: IsoDateTime;
}

/** aml.cliente_json(id): KYC identity and risk profile (staff only). */
export interface ClienteAml {
  id: Uuid; tipo_persona: "FISICA" | "MORAL"; nombre_completo: string; email: string;
  curp: string | null; rfc: string | null; fecha_nacimiento: IsoDate | null;
  nacionalidad: string; pais_residencia: string; entidad_nacimiento: string | null;
  domicilio: { estado: string | null; municipio: string | null; codigo_postal: string | null };
  actividad_economica: string | null; ocupacion: string | null; ingreso_mensual_estimado: Decimal | null;
  nivel_riesgo_pld: "BAJO" | "MEDIO" | "ALTO"; riesgo_evaluado_en: IsoDateTime | null;
  es_pep: boolean; en_lista_bloqueadas: boolean;
  estatus_kyc: "PENDIENTE" | "EN_REVISION" | "VERIFICADO" | "RECHAZADO" | "SUSPENDIDO"; kyc_verificado_en: IsoDateTime | null;
  es_interno: boolean;                            // holds a non-CLIENTE credential
  cliente_desde: IsoDateTime;
}

/** aml.cuentas_json(cliente_id): real states (CONGELADO_PREVENTIVO is visible here). */
export interface CuentasAml {
  ifpe: { id: Uuid; clabe: string; estado: EstadoCuentaInternaAml; saldo_disponible: Decimal; saldo_bloqueado: Decimal;
          retenido_aml: Decimal; creado_en: IsoDateTime } | null;
  sofom: { id: Uuid; estado: "ACTIVA" | "CONGELADA" | "CERRADA"; saldo_disponible: Decimal; saldo_bloqueado: Decimal;
           creado_en: IsoDateTime } | null;
}

/** One element of CalculoAml.factores (§2.6.3 step 4/5). */
export interface FactorAml {
  codigo: string; regla_id: Uuid; version: number; tipo: "PRIMARIA" | "MODIFICADOR";
  puntos: Decimal; permite_congelar: boolean; evidencia: Record<string, unknown> | null;
}
/** The aml.calcular output stored in aml.evaluaciones.factores (§2.6.3 step 8). */
export interface CalculoAml {
  version: number; parametros_id: number; ahora: IsoDateTime; nivel_cliente: "BAJO" | "MEDIO" | "ALTO";
  perfil: { nivel_riesgo_pld: "BAJO" | "MEDIO" | "ALTO"; es_pep: boolean; nacionalidad: string | null; pais_residencia: string | null;
            entidad_nacimiento: string | null; estado_domicilio: string | null; ingreso_mensual_estimado: Decimal | null;
            antiguedad_dias: number; es_interno: boolean };
  ventana: { desde: IsoDateTime; hasta: IsoDateTime; horas: number; entradas_n: number; entradas_monto: Decimal;
             salidas_n: number; salidas_monto: Decimal; operaciones_n: number; volumen: Decimal };
  factores: FactorAml[]; primarias: string[]; reglas_aplicadas: Uuid[];
  puntaje_bruto: Decimal; puntaje: Decimal; nivel: NivelRiesgoAml;
  dominante: string | null; dominante_regla_id: Uuid | null; congelar: boolean;
  relevante: boolean; regla_relevante_id: Uuid | null;
  transacciones_patron: Uuid[]; monto_patron: Decimal;
}

/** aml.evaluacion_json(id) */
export interface EvaluacionAml {
  id: Uuid; creado_en: IsoDateTime; cliente_id: Uuid; entidad: EntidadAml; sentido: SentidoMovimientoAml; canal: CanalAml;
  monto: Decimal; transaccion_id: Uuid | null; clave_operacion: string; contraparte: ContraparteAml;
  puntaje: Decimal | null; nivel: NivelRiesgoAml | null; resultado: ResultadoEvaluacionAml;
  motivo_bloqueo: "CONGELADO" | "PUNTAJE_CRITICO" | null; origen: OrigenEvaluacionAml; evaluacion_origen_id: Uuid | null;
  factores: CalculoAml | { error: string } | Record<string, never>;   // {} for blocked-while-frozen and markers
  reglas_aplicadas: Uuid[]; parametros_id: number | null; monto_retener: Decimal;
}

/** One row of aml.movimientos_cliente plus a held flag (staff view). */
export interface MovimientoAml {
  entidad: EntidadAml; transaccion_id: Uuid; sentido: SentidoMovimientoAml; canal: CanalAml; monto: Decimal;
  contraparte: string | null;      // text key of §2.6.2, e.g. "cuenta:012180001234567899"
  creado_en: IsoDateTime;
  retenido: boolean;               // a VIGENTE aml.retenciones row has transaccion_origen_id = transaccion_id
}

/** One aml.bitacora row. */
export interface BitacoraAml {
  id: number; ocurrido_en: IsoDateTime; evento: string; objeto: string; objeto_id: string | null;
  actor: PersonaRefAml | null; actor_tipo: ActorTipoAml;
  estado_anterior: string | null; estado_nuevo: string | null; motivo: string | null; datos: Record<string, unknown>;
}

/** aml.regla_json(id): every column flat, so `cambios` keys map 1:1. */
export interface ReglaAml {
  id: Uuid; codigo: string; nivel_cliente: NivelClienteReglaAml; version: number; nombre: string; descripcion: string;
  clase: ClaseReglaAml; sentido: SentidoReglaAml; tipo_reporte: "RELEVANTE" | TipoReporteAml | null;
  activa: boolean; puntos: Decimal; permite_congelar: boolean;
  umbral_monto: Decimal | null; banda_inferior: Decimal | null; frecuencia_min: number | null; ventana_horas: number | null;
  volumen_max: Decimal | null; operaciones_max: number | null; multiplo_ingreso: Decimal | null; porcentaje_min: Decimal | null;
  contrapartes_min: number | null; dias_antiguedad: number | null;
  paises_alto_riesgo: string[]; entidades_alto_riesgo: string[]; estados_alto_riesgo: string[];
  propuesta_id: Uuid | null; creado_en: IsoDateTime; desactivada_en: IsoDateTime | null;
}

/** aml.parametros_json(): the current version. */
export interface ParametrosAml {
  id: number; umbral_medio: Decimal; umbral_alto: Decimal; umbral_critico: Decimal; modo_observacion: boolean;
  sla_critica_horas: number; sla_congelamiento_horas: number; dias_reporte_inusual: number;
  horas_reporte_urgente: number; dias_aviso_plazo: number; alertar_desde: IsoDateTime;
  propuesta_id: Uuid | null; creado_en: IsoDateTime;
}

/** aml.propuesta_json(id) */
export interface PropuestaAml {
  id: Uuid; folio: number; tipo: TipoPropuestaAml; regla_codigo: string | null; nivel_cliente: NivelClienteReglaAml | null;
  cambios: Record<string, string | number | boolean | string[]>; justificacion: string;
  propuesta_por: PersonaRefAml; propuesta_en: IsoDateTime; estado: EstadoPropuestaAml;
  decidida_por: PersonaRefAml | null; decidida_en: IsoDateTime | null; comentario: string | null; resultado_id: string | null;
}

/** aml.resumen() */
export interface ResumenAml {
  pendientes: number; en_revision: number;
  abiertas_por_nivel: { BAJO: number; MEDIO: number; ALTO: number; CRITICO: number };   // PENDIENTE + EN_REVISION
  criticas_abiertas: number;
  congelamientos_activos: number; solicitudes_pendientes: number; propuestas_pendientes: number;
  sla_congelamiento_vencidos: number;   // ACTIVO, sla_vence_en < now, no PENDIENTE request
  reportes_por_vencer: number;          // open, or CONFIRMADO without folio_uif, with fecha_limite_reporte in [now, now + dias_aviso_plazo]
  reportes_vencidos: number;            // same set, fecha_limite_reporte < now
  notificaciones_sin_entregar: number; modo_observacion: boolean;
  ultimo_ciclo_en: IsoDateTime | null;  // aml.monitoreo_estado
  servidor_en: IsoDateTime;
}

/** aml.consultar_alerta(actor, id) (§2.9). */
export interface DetalleAlertaAml {
  alerta: AlertaAml & { detalles: Record<string, unknown>; dictamen_propuesto: string | null; legado_analista: string | null };
  evaluaciones: EvaluacionAml[];   // evaluacion_id plus aml.alerta_evaluaciones, creado_en ascending, max 50
  movimientos: MovimientoAml[];    // ids in detalles.transacciones resolved through aml.movimientos_cliente over
                                   // [timestamp - 30 days, now]; ids not found are omitted; creado_en ascending, max 50
  cliente: ClienteAml; cuentas: CuentasAml;
  congelamiento: CongelamientoAml | null;   // the client's ACTIVO freeze, else the alert's congelamiento_id, else null
  otras_alertas: AlertaAml[];      // other alerts of the client, folio descending, max 20
  historial: BitacoraAml[];        // rows with alerta_id = id or congelamiento_id = congelamiento.id; id descending, max 100
}

export interface AgregadoAml { entradas_n: number; entradas_monto: Decimal; salidas_n: number; salidas_monto: Decimal }
/** aml.consultar_cliente(actor, id) (§2.9). */
export interface RevisionClienteAml {
  cliente: ClienteAml; cuentas: CuentasAml;
  agregados: { h24: AgregadoAml; d7: AgregadoAml; d30: AgregadoAml };     // from aml.movimientos_cliente
  movimientos: MovimientoAml[];                                           // last 30 days, newest first, max 100
  ordenes_spei_pendientes: { id: Uuid; clave_rastreo: string; monto: Decimal; beneficiario_clabe: string;
    beneficiario_nombre: string; estado: "PENDIENTE" | "ENVIADA"; intentos_envio: number; creado_en: IsoDateTime }[];
  mutuos_vigentes: { id: Uuid; folio: number; principal: Decimal; fecha_vencimiento: IsoDate }[];
  creditos: { id: Uuid; folio: number; principal: Decimal; saldo_insoluto: Decimal;
    estado: "EN_FONDEO" | "VIGENTE" | "VENCIDO" | "LIQUIDADO" | "CASTIGADO"; fecha_vencimiento: IsoDate }[];
  alertas: AlertaAml[];                 // folio descending, max 50
  congelamientos: CongelamientoAml[];   // congelado_en descending
  relevantes_n: number;
}

/** Built in Rust from aml.notificaciones columns (no amounts). */
export interface NotificacionAml {
  id: number; creado_en: IsoDateTime; tipo: TipoNotificacionAml; prioridad: PrioridadAml;
  cliente_id: Uuid | null; alerta_id: Uuid | null; congelamiento_id: Uuid | null; solicitud_id: Uuid | null; propuesta_id: Uuid | null;
  resumen: string; entregada_en: IsoDateTime | null;
}

/** One aml.operaciones_relevantes row. */
export interface RelevanteAml {
  id: Uuid; cliente: PersonaRefAml; entidad: EntidadAml; transaccion_id: Uuid; sentido: SentidoAlertaAml;
  canal: CanalAml | null; monto: Decimal; trimestre: string; creado_en: IsoDateTime;
  reporte_folio: string | null; reportada_en: IsoDate | null;
}

export interface VerificacionBitacoraAml { valido: boolean; filas: number; sellos: number; primer_error: string | null }

// ---------- list wrappers, requests and responses (§4.2 GET/POST) ----------
export interface ListaAlertasAml { alertas: AlertaAml[]; siguiente_antes_de_folio: number | null; servidor_en: IsoDateTime }
export interface AmlAlertsQuery { estado?: string; nivel?: string; regla?: string; cliente_id?: Uuid; asignada?: "yo" | "nadie";
  antes_de_folio?: number; actualizadas_desde?: IsoDateTime; limite?: number }
export interface ListaCongelamientosAml { congelamientos: CongelamientoAml[]; siguiente_antes_de_folio: number | null }
export interface ListaSolicitudesAml { solicitudes: SolicitudAml[] }
export interface ListaNotificacionesAml { notificaciones: NotificacionAml[]; ultimo_id: number | null }
export interface ReglasAml { parametros: ParametrosAml; reglas: ReglaAml[]; propuestas_pendientes: PropuestaAml[] }
export interface ListaPropuestasAml { propuestas: PropuestaAml[] }
export interface ListaRelevantesAml { relevantes: RelevanteAml[]; siguiente_antes_de: IsoDateTime | null }
export interface ResolverAlertaInput { accion: AccionResolucionAml; dictamen?: string | null; tipo_reporte?: TipoReporteAml | null;
  urgente?: boolean; congelar?: boolean; tipo_levantamiento?: "LEVANTAR_Y_LIBERAR" | "LEVANTAR_CONSERVANDO_RETENCIONES" | null;
  clave_idempotencia: string }
export interface ResolucionAlertaAml { alerta: AlertaAml; congelamiento: CongelamientoAml | null; solicitud: SolicitudAml | null; idempotente: boolean }
export interface ReporteUifInput { folio_uif: string; reportada_en: IsoDate; clave_idempotencia: string }
export interface ReporteUifAml { alerta: AlertaAml; idempotente: boolean }
export interface CongelarClienteInput { motivo: string; alerta_id?: Uuid | null; clave_idempotencia: string }
export interface CongelamientoManualAml { congelamiento: CongelamientoAml; alerta: AlertaAml | null; ya_congelado: boolean; idempotente: boolean }
export interface SolicitudLevantamientoInput { tipo: TipoSolicitudAml; justificacion: string; clave_idempotencia: string }
export interface SolicitudCreadaAml { solicitud: SolicitudAml; congelamiento: CongelamientoAml; idempotente: boolean }
export interface DecisionInput { aprobar: boolean; comentario: string; clave_idempotencia: string }
export interface DecisionLevantamientoAml { solicitud: SolicitudAml; congelamiento: CongelamientoAml; alertas_resueltas: AlertaAml[];
  liberaciones: { retencion_id: Uuid; transaccion_id: Uuid; monto: Decimal }[]; idempotente: boolean }
export interface PropuestaInput { tipo: TipoPropuestaAml; regla_codigo?: string | null; nivel_cliente?: NivelClienteReglaAml | null;
  cambios: Record<string, string | number | boolean | string[]>; justificacion: string; clave_idempotencia: string }
export interface PropuestaCreadaAml { propuesta: PropuestaAml; idempotente: boolean }
export interface DecisionPropuestaAml { propuesta: PropuestaAml; regla: ReglaAml | null; parametros: ParametrosAml | null; idempotente: boolean }
