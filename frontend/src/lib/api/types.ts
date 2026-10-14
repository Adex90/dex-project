/**
 * DTOs returned by the Rust backend (backend/src/models, backend/src/routes).
 * Amounts (`Decimal`) are strings with 8 decimals; timestamps are ISO-8601
 * UTC; dates (`IsoDate`) are "YYYY-MM-DD".
 */

export type Decimal = string;
export type Uuid = string;
export type IsoDateTime = string;
export type IsoDate = string;

export type Rol = "CLIENTE" | "OPERADOR" | "AUDITOR";
export type TipoPersona = "FISICA" | "MORAL";
export type EstatusKyc = "PENDIENTE" | "EN_REVISION" | "VERIFICADO" | "RECHAZADO" | "SUSPENDIDO";
export type NivelRiesgo = "BAJO" | "MEDIO" | "ALTO";
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
  nivel_riesgo_pld: NivelRiesgo;
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
  umbral_aplicado: Decimal;
  timestamp: IsoDateTime;
  detalles_fallo: DetalleKyc[];
}

/** GET /kyc/expediente. */
export interface VistaExpediente extends ExpedienteKyc {
  tipo_persona: TipoPersona;
  max_intentos: number;
  intentos_restantes: number;
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
