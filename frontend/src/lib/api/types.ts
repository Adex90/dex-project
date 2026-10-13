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
