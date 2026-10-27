-- =============================================================================
--  Synthetic Capital — 0009: PLD/FT (AML/CFT) en la base de datos
-- =============================================================================
--
--  Monitoreo transaccional, alertas, congelamiento preventivo y retenciones
--  como reglas de la base de datos (Disposiciones de carácter general en
--  materia de PLD/FT para IFPE y SOFOM). Reemplaza el PLD heredado
--  (`kyc.alertas_pld` queda como historia de solo lectura).
--
--  1. Esquema `aml` y rol NOLOGIN `sc_aml` (consola de cumplimiento y monitoreo
--     por lotes): solo lee `aml` y ejecuta funciones definer; ningún DML. Los
--     roles transaccionales (`sc_ifpe`, `sc_spei`, `sc_puente`, `sc_sofom`,
--     `sc_kyc`, `sc_kyc_verificador`) no tienen USAGE sobre `aml`: llegan al
--     motor solo por cuatro envolturas en sus propios esquemas, que reciben
--     hechos de la operación (cliente, sentido, canal, monto, id y clave del
--     movimiento, contraparte), nunca un veredicto.
--  2. Un solo calificador: `aml.evaluar_operacion` calcula el puntaje con datos
--     del ledger (ventanas de 24 h por cliente, todos los canales), bajo un
--     candado por cliente (fila IFPE y luego candado consultivo) y en READ
--     COMMITTED. Matriz de riesgo versionada e inmutable (`aml.reglas_riesgo`),
--     parámetros versionados (`aml.parametros`); todo cambio exige cuatro ojos.
--  3. Congelamiento preventivo por cliente: `ifpe.cuentas.estado =
--     'CONGELADO_PREVENTIVO'`, reflejado uno a uno por un `aml.congelamientos`
--     ACTIVO (invariante diferida SC_AML_ESPEJO). Guardia BEFORE INSERT en
--     ambos ledgers: un cliente congelado no retira, no envía transferencias,
--     no invierte en mutuo ni recibe crédito (ni siquiera el dueño del esquema).
--     Solo funciones definer cambian el estado de una cuenta.
--  4. El dinero entrante nunca se rechaza: se abona y se retiene (RETENCION /
--     LIBERACION_RETENCION con claves neutras `ret:`/`lib:`), así que la reserva
--     del 100%, las sombras y las cadenas de hash no cambian. Una retención
--     solo se libera con una aprobación de cuatro ojos de la misma transacción;
--     ningún otro camino puede tocar el saldo retenido (SC_RETENCION_PROTEGIDA).
--  5. Cobertura: todo movimiento de dinero de un cliente tiene su evaluación,
--     ligada al renglón del ledger (mismo xmin); uno sin evaluación recibe una
--     marca DIFERIDA que el monitoreo por lotes califica (nunca un error).
--  6. Alertas con máquina de estados, puntaje que solo sube, plazos UIF,
--     segregación de funciones (CUMPLIMIENTO; nadie resuelve sobre sí mismo;
--     cuatro ojos para levantar, liberar y cambiar reglas) y bitácora
--     inmutable con hash canónico por renglón y sellado por lotes.
--  7. Sin aviso al cliente: códigos y conceptos neutros (también en las
--     retenciones del operador, que tienen la misma forma que las de PLD); un
--     solo error constante para toda operación bloqueada; congelar, levantar o
--     cambiar el nivel de riesgo no mueve actualizado_en de lo que el cliente
--     lee. Nadie de cumplimiento ve los expedientes de los que es sujeto, y
--     sc_auditor (rutas del cliente) no tiene acceso a aml.
--  8. El nivel de riesgo PLD del cliente no baja fuera de cumplimiento
--     mientras el cliente está bajo revisión (alerta abierta o confirmada,
--     congelamiento activo), y todo cambio de nivel queda en la bitácora.
--
--  No cambian: SC_RESERVA_100, SC_SALDO_SIN_LEDGER, la lista blanca de 0005,
--  el RLS de 0006/0007, `ifpe.v_hojas_reserva` (una cuenta congelada sigue
--  siendo hoja del Proof of Reserve) ni ninguna función de 0001-0008. Los
--  triggers diferidos nuevos sobre tablas existentes se llaman `..._zz_aml_...`
--  para ordenarse después de los existentes (precedencia de errores).
--  `ifpe.registrar_abonos_legados()` no dispara lógica PLD (no hay trigger en
--  `ifpe.spei_inbound`).
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. Rol sc_aml, esquema y tipos
-- -----------------------------------------------------------------------------
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sc_aml') THEN
        CREATE ROLE sc_aml NOLOGIN;
    END IF;
    EXECUTE format('GRANT sc_aml TO %I', current_user);
    -- Verificación externa de la bitácora (un auditor de base de datos): solo
    -- ejecuta aml.verificar_bitacora. sc_auditor no toca aml porque lo usan
    -- rutas del cliente (/tablero, /reserva/prueba).
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sc_auditor_pld') THEN
        CREATE ROLE sc_auditor_pld NOLOGIN;
    END IF;
    EXECUTE format('GRANT sc_auditor_pld TO %I', current_user);
END
$$;

CREATE SCHEMA aml;
REVOKE ALL ON SCHEMA aml FROM PUBLIC;
COMMENT ON SCHEMA aml IS 'PLD/FT: matriz de riesgo, evaluaciones, alertas, congelamientos, retenciones y bitácora. Sin USAGE para los roles transaccionales.';

-- El orden importa: GREATEST y ORDER BY comparan niveles.
CREATE TYPE aml.nivel_riesgo  AS ENUM ('BAJO', 'MEDIO', 'ALTO', 'CRITICO');
CREATE TYPE aml.estado_alerta AS ENUM ('PENDIENTE', 'EN_REVISION', 'CONFIRMADO', 'FALSO_POSITIVO');

-- Un renglón de aml.movimientos_cliente (para guardarlo en un arreglo y leer
-- el ledger una sola vez por evaluación).
CREATE TYPE aml.movimiento AS (
    entidad        TEXT,
    transaccion_id UUID,
    sentido        TEXT,
    canal          TEXT,
    monto          NUMERIC,
    contraparte    TEXT,
    creado_en      TIMESTAMPTZ
);

-- -----------------------------------------------------------------------------
-- 2. Cambios a objetos existentes
-- -----------------------------------------------------------------------------
ALTER TABLE ifpe.cuentas DROP CONSTRAINT cuentas_estado_check,
    ADD CONSTRAINT ck_ifpe_cuentas_estado CHECK (estado IN ('ACTIVA', 'CONGELADA', 'CONGELADO_PREVENTIVO', 'CERRADA'));
ALTER TABLE kyc.credenciales DROP CONSTRAINT ck_credenciales_rol,
    ADD CONSTRAINT ck_credenciales_rol CHECK (rol IN ('CLIENTE', 'OPERADOR', 'AUDITOR', 'CUMPLIMIENTO'));

-- Nadie más que las funciones definer cambia el estado de una cuenta (0005 lo
-- concedió; ningún código de la aplicación lo usa).
REVOKE UPDATE (estado) ON ifpe.cuentas FROM sc_ifpe;
-- 0001 concedió al puente UPDATE de toda la fila de sofom.cuentas; solo mueve
-- saldos (ya no puede cambiar `estado`). `cliente_id` conserva su privilegio
-- solo para que un intento de cambiar el titular siga respondiendo con
-- SC_CUENTA_INMUTABLE (trg_sofom_cuentas_titular_inmutable de 0007 lo rechaza
-- siempre, incluso al dueño del esquema; tests/db/0007_kyc.sql lo verifica).
REVOKE UPDATE ON sofom.cuentas FROM sc_puente;
GRANT  UPDATE (saldo_disponible, saldo_bloqueado, cliente_id) ON sofom.cuentas TO sc_puente;

-- Bandeja PLD heredada (ver también la sección 13): nadie la escribe y los
-- roles de las rutas del cliente dejan de leerla.
REVOKE INSERT ON kyc.alertas_pld FROM sc_ifpe, sc_sofom, sc_puente, sc_spei;
REVOKE SELECT, UPDATE ON kyc.alertas_pld FROM sc_kyc;
REVOKE SELECT ON kyc.alertas_pld FROM sc_kyc_verificador;
-- `sofom.cuentas` conserva su CHECK de 0001 (ACTIVA|CONGELADA|CERRADA): el
-- congelamiento preventivo vive solo en la cuenta IFPE.

-- Retenciones y liberaciones del operador (POST /operaciones/retenciones). En
-- el ledger del cliente tienen la misma forma que las de PLD: clave neutra
-- 'ret:<id>' / 'lib:<id>' y concepto "Retención temporal" / "Liberación de
-- retención" (antes llevaban 'op:<operador>:<clave>' y el texto del operador,
-- así que una retención 'ret:' delataba que era de cumplimiento). Aquí quedan
-- la clave de idempotencia del operador y su motivo; solo inserción.
CREATE TABLE ifpe.retenciones_operador (
    id                 UUID PRIMARY KEY,
    clave_idempotencia TEXT NOT NULL UNIQUE CHECK (length(clave_idempotencia) BETWEEN 1 AND 200),
    tipo               TEXT NOT NULL CHECK (tipo IN ('RETENCION', 'LIBERACION_RETENCION')),
    cuenta_id          UUID NOT NULL REFERENCES ifpe.cuentas (id) ON DELETE RESTRICT,
    monto              NUMERIC(18, 8) NOT NULL CHECK (monto > 0),
    motivo             TEXT NOT NULL CHECK (length(btrim(motivo)) BETWEEN 1 AND 140),
    transaccion_id     UUID NOT NULL UNIQUE REFERENCES ifpe.transacciones (id) ON DELETE RESTRICT,
    creado_en          TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX ix_ifpe_retenciones_operador_cuenta ON ifpe.retenciones_operador (cuenta_id, creado_en);
CREATE TRIGGER trg_ifpe_retenciones_operador_inmutable BEFORE UPDATE OR DELETE ON ifpe.retenciones_operador
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_ifpe_retenciones_operador_no_truncate BEFORE TRUNCATE ON ifpe.retenciones_operador
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
REVOKE ALL ON ifpe.retenciones_operador FROM PUBLIC;
GRANT SELECT, INSERT ON ifpe.retenciones_operador TO sc_ifpe;

-- -----------------------------------------------------------------------------
-- 3. Tablas (orden de llaves foráneas; las circulares se agregan al final)
-- -----------------------------------------------------------------------------

-- 3.1 Propuestas de cambio a reglas y parámetros (cuatro ojos).
CREATE TABLE aml.propuestas_cambio (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    folio         BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE,
    tipo          TEXT NOT NULL CHECK (tipo IN ('REGLA', 'PARAMETROS')),
    regla_codigo  TEXT,
    nivel_cliente TEXT CHECK (nivel_cliente IN ('BAJO', 'MEDIO', 'ALTO', 'TODOS')),
    cambios       JSONB NOT NULL CHECK (jsonb_typeof(cambios) = 'object'),
    justificacion TEXT NOT NULL CHECK (length(btrim(justificacion)) BETWEEN 30 AND 2000),
    propuesta_por UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    propuesta_en  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    estado        TEXT NOT NULL DEFAULT 'PENDIENTE' CHECK (estado IN ('PENDIENTE', 'APROBADA', 'RECHAZADA')),
    decidida_por  UUID REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    decidida_en   TIMESTAMPTZ,
    comentario    TEXT CHECK (length(btrim(comentario)) BETWEEN 10 AND 2000),
    resultado_id  TEXT,

    CONSTRAINT ck_aml_propuesta_regla CHECK ((tipo = 'REGLA') = (regla_codigo IS NOT NULL AND nivel_cliente IS NOT NULL)),
    CONSTRAINT ck_aml_propuesta_cuatro_ojos CHECK (decidida_por IS DISTINCT FROM propuesta_por),
    CONSTRAINT ck_aml_propuesta_decision CHECK ((estado = 'PENDIENTE') = (decidida_por IS NULL)),
    CONSTRAINT ck_aml_propuesta_fecha CHECK ((estado = 'PENDIENTE') = (decidida_en IS NULL))
);
CREATE INDEX ix_aml_propuestas_estado ON aml.propuestas_cambio (estado, folio DESC);

-- 3.2 Parámetros (versiones de solo inserción; la vigente es la de mayor id).
CREATE TABLE aml.parametros (
    id                      BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    umbral_medio            NUMERIC(5, 2) NOT NULL DEFAULT 30,
    umbral_alto             NUMERIC(5, 2) NOT NULL DEFAULT 60,
    umbral_critico          NUMERIC(5, 2) NOT NULL DEFAULT 85,
    -- Modo observación: se califica y se alerta, pero no se congela, no se
    -- retiene ni se bloquea (despliegue gradual). Lo cambia una propuesta.
    modo_observacion        BOOLEAN NOT NULL DEFAULT FALSE,
    sla_critica_horas       INTEGER NOT NULL DEFAULT 24 CHECK (sla_critica_horas BETWEEN 1 AND 720),
    sla_congelamiento_horas INTEGER NOT NULL DEFAULT 72 CHECK (sla_congelamiento_horas BETWEEN 1 AND 720),
    dias_reporte_inusual    INTEGER NOT NULL DEFAULT 60 CHECK (dias_reporte_inusual BETWEEN 1 AND 365),
    horas_reporte_urgente   INTEGER NOT NULL DEFAULT 24 CHECK (horas_reporte_urgente BETWEEN 1 AND 168),
    dias_aviso_plazo        INTEGER NOT NULL DEFAULT 7 CHECK (dias_aviso_plazo BETWEEN 1 AND 60),
    -- El monitoreo por lotes ignora movimientos anteriores (sin barridos retroactivos).
    alertar_desde           TIMESTAMPTZ NOT NULL DEFAULT now(),
    propuesta_id            UUID REFERENCES aml.propuestas_cambio (id) ON DELETE RESTRICT,
    creado_en               TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),

    CONSTRAINT ck_aml_parametros_umbrales CHECK (
        0 < umbral_medio AND umbral_medio < umbral_alto AND umbral_alto < umbral_critico AND umbral_critico <= 100)
);

-- 3.3 Matriz de riesgo (versionada; los renglones no cambian).
CREATE TABLE aml.reglas_riesgo (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    codigo                TEXT NOT NULL CHECK (codigo ~ '^[RM][0-9]{2}_[A-Z0-9_]+$'),
    nivel_cliente         TEXT NOT NULL CHECK (nivel_cliente IN ('BAJO', 'MEDIO', 'ALTO', 'TODOS')),
    version               INTEGER NOT NULL CHECK (version > 0),
    nombre                TEXT NOT NULL CHECK (length(btrim(nombre)) BETWEEN 1 AND 120),
    descripcion           TEXT NOT NULL CHECK (length(btrim(descripcion)) BETWEEN 1 AND 2000),
    clase                 TEXT NOT NULL CHECK (clase IN ('RELEVANTE', 'PRIMARIA', 'MODIFICADOR', 'LOTE', 'SISTEMA', 'LEGADO')),
    sentido               TEXT NOT NULL CHECK (sentido IN ('ENTRADA', 'SALIDA', 'AMBOS', 'NINGUNO')),
    tipo_reporte          TEXT CHECK (tipo_reporte IN ('RELEVANTE', 'INUSUAL', 'INTERNA_PREOCUPANTE')),
    activa                BOOLEAN NOT NULL DEFAULT TRUE,
    puntos                NUMERIC(5, 2) NOT NULL DEFAULT 0 CHECK (puntos BETWEEN 0 AND 100),
    permite_congelar      BOOLEAN NOT NULL DEFAULT FALSE,
    umbral_monto          NUMERIC(18, 8) CHECK (umbral_monto > 0),
    banda_inferior        NUMERIC(5, 4) CHECK (banda_inferior > 0 AND banda_inferior < 1),
    frecuencia_min        INTEGER CHECK (frecuencia_min >= 2),
    ventana_horas         INTEGER CHECK (ventana_horas BETWEEN 1 AND 2160),
    volumen_max           NUMERIC(18, 8) CHECK (volumen_max > 0),
    operaciones_max       INTEGER CHECK (operaciones_max > 0),
    multiplo_ingreso      NUMERIC(6, 2) CHECK (multiplo_ingreso > 0),
    porcentaje_min        NUMERIC(5, 4) CHECK (porcentaje_min > 0 AND porcentaje_min <= 1),
    contrapartes_min      INTEGER CHECK (contrapartes_min >= 2),
    dias_antiguedad       INTEGER CHECK (dias_antiguedad > 0),
    -- ISO 3166-1 alfa-2 (nacionalidad y residencia).
    paises_alto_riesgo    CHAR(2)[] NOT NULL DEFAULT '{}',
    -- Claves RENAPO (kyc_perfiles.entidad_nacimiento).
    entidades_alto_riesgo CHAR(2)[] NOT NULL DEFAULT '{}',
    -- Nombres normalizados con aml.normalizar_texto (estado del domicilio).
    estados_alto_riesgo   TEXT[] NOT NULL DEFAULT '{}',
    propuesta_id          UUID REFERENCES aml.propuestas_cambio (id) ON DELETE RESTRICT,
    creado_en             TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    desactivada_en        TIMESTAMPTZ,

    CONSTRAINT ux_aml_reglas_version UNIQUE (codigo, nivel_cliente, version),
    CONSTRAINT ck_aml_regla_tipo_reporte CHECK ((tipo_reporte IS NULL) = (clase = 'MODIFICADOR')),
    CONSTRAINT ck_aml_regla_desactivada CHECK (NOT activa OR desactivada_en IS NULL),
    -- Las ventanas en línea leen a lo sumo 24 h del ledger (aml.calcular).
    CONSTRAINT ck_aml_regla_ventana_en_linea CHECK (clase <> 'PRIMARIA' OR ventana_horas IS NULL OR ventana_horas <= 24),
    -- Parámetros que usa cada regla (sección 5 y aml.calcular / aml.barrer_cliente).
    CONSTRAINT ck_aml_regla_r01 CHECK (codigo <> 'R01_OPERACION_RELEVANTE' OR umbral_monto IS NOT NULL),
    CONSTRAINT ck_aml_regla_r02 CHECK (codigo <> 'R02_FRACCIONAMIENTO'
        OR (umbral_monto IS NOT NULL AND frecuencia_min IS NOT NULL AND ventana_horas IS NOT NULL)),
    CONSTRAINT ck_aml_regla_r03 CHECK (codigo <> 'R03_CLIENTE_RIESGO_ALTO' OR umbral_monto IS NOT NULL),
    CONSTRAINT ck_aml_regla_r04 CHECK (codigo <> 'R04_FUERA_DE_PERFIL' OR multiplo_ingreso IS NOT NULL),
    CONSTRAINT ck_aml_regla_r05 CHECK (codigo <> 'R05_ESTRUCTURACION'
        OR (umbral_monto IS NOT NULL AND banda_inferior IS NOT NULL AND frecuencia_min IS NOT NULL
            AND ventana_horas IS NOT NULL)),
    CONSTRAINT ck_aml_regla_r07 CHECK (codigo <> 'R07_VOLUMEN_DIARIO' OR (volumen_max IS NOT NULL AND ventana_horas IS NOT NULL)),
    CONSTRAINT ck_aml_regla_r08 CHECK (codigo <> 'R08_FRECUENCIA' OR (operaciones_max IS NOT NULL AND ventana_horas IS NOT NULL)),
    CONSTRAINT ck_aml_regla_r11 CHECK (codigo <> 'R11_PASO_RAPIDO'
        OR (umbral_monto IS NOT NULL AND porcentaje_min IS NOT NULL AND banda_inferior IS NOT NULL
            AND ventana_horas IS NOT NULL)),
    CONSTRAINT ck_aml_regla_r12 CHECK (codigo <> 'R12_ESTRUCTURACION_FRECUENTE'
        OR (umbral_monto IS NOT NULL AND banda_inferior IS NOT NULL AND frecuencia_min IS NOT NULL
            AND volumen_max IS NOT NULL AND ventana_horas IS NOT NULL)),
    CONSTRAINT ck_aml_regla_r20 CHECK (codigo <> 'R20_ESTRUCTURACION_LENTA'
        OR (umbral_monto IS NOT NULL AND banda_inferior IS NOT NULL AND frecuencia_min IS NOT NULL
            AND operaciones_max IS NOT NULL AND ventana_horas IS NOT NULL)),
    CONSTRAINT ck_aml_regla_r21 CHECK (codigo <> 'R21_VOLUMEN_MENSUAL'
        OR (volumen_max IS NOT NULL AND multiplo_ingreso IS NOT NULL AND ventana_horas IS NOT NULL)),
    CONSTRAINT ck_aml_regla_r22 CHECK (codigo <> 'R22_CONCENTRACION_ENTRADAS'
        OR (contrapartes_min IS NOT NULL AND umbral_monto IS NOT NULL AND ventana_horas IS NOT NULL)),
    CONSTRAINT ck_aml_regla_r23 CHECK (codigo <> 'R23_DISPERSION_SALIDAS'
        OR (contrapartes_min IS NOT NULL AND umbral_monto IS NOT NULL AND ventana_horas IS NOT NULL)),
    CONSTRAINT ck_aml_regla_r24 CHECK (codigo <> 'R24_PASO_RAPIDO_48H'
        OR (umbral_monto IS NOT NULL AND porcentaje_min IS NOT NULL AND ventana_horas IS NOT NULL)),
    CONSTRAINT ck_aml_regla_r25 CHECK (codigo <> 'R25_CIRCULAR' OR (umbral_monto IS NOT NULL AND ventana_horas IS NOT NULL)),
    CONSTRAINT ck_aml_regla_r26 CHECK (codigo <> 'R26_RED_ORDENANTE'
        OR (contrapartes_min IS NOT NULL AND umbral_monto IS NOT NULL AND banda_inferior IS NOT NULL
            AND ventana_horas IS NOT NULL)),
    CONSTRAINT ck_aml_regla_r27 CHECK (codigo <> 'R27_RED_BENEFICIARIO'
        OR (contrapartes_min IS NOT NULL AND umbral_monto IS NOT NULL AND ventana_horas IS NOT NULL)),
    CONSTRAINT ck_aml_regla_m04 CHECK (codigo <> 'M04_CUENTA_NUEVA' OR dias_antiguedad IS NOT NULL)
);
CREATE UNIQUE INDEX ux_aml_reglas_vigente ON aml.reglas_riesgo (codigo, nivel_cliente) WHERE activa;

-- 3.4 Evaluaciones: una por movimiento evaluado y perspectiva (incluidos los
-- intentos bloqueados). Solo inserción.
CREATE TABLE aml.evaluaciones (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    creado_en            TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    cliente_id           UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    entidad              TEXT NOT NULL CHECK (entidad IN ('IFPE', 'SOFOM')),
    sentido              TEXT NOT NULL CHECK (sentido IN ('ENTRADA', 'SALIDA')),
    canal                TEXT NOT NULL CHECK (canal IN ('SPEI_WEBHOOK', 'SPEI_OPERADOR', 'SPEI_DEMO', 'TRANSFERENCIA_RECIBIDA',
                                                       'TRANSFERENCIA_ENVIADA', 'SPEI_ENVIADO', 'INVERSION_MUTUO',
                                                       'DESEMBOLSO_CREDITO', 'COBRO_CREDITO')),
    monto                NUMERIC(18, 8) NOT NULL CHECK (monto > 0),
    -- Id del renglón del ledger (NULL solo en un intento bloqueado).
    transaccion_id       UUID,
    -- clave_idempotencia del ledger que el camino del dinero escribe o escribió.
    clave_operacion      TEXT NOT NULL,
    contraparte          JSONB NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(contraparte) = 'object'),
    puntaje              NUMERIC(5, 2) CHECK (puntaje BETWEEN 0 AND 100),
    nivel                aml.nivel_riesgo,
    resultado            TEXT NOT NULL CHECK (resultado IN ('PERMITIDA', 'ALERTADA', 'RETENIDA', 'BLOQUEADA',
                                                            'PENDIENTE_REEVALUACION', 'REEVALUADA')),
    motivo_bloqueo       TEXT CHECK (motivo_bloqueo IN ('CONGELADO', 'PUNTAJE_CRITICO')),
    origen               TEXT NOT NULL CHECK (origen IN ('EN_LINEA', 'DIFERIDA', 'REEVALUACION')),
    evaluacion_origen_id UUID REFERENCES aml.evaluaciones (id) ON DELETE RESTRICT,
    -- Salida de aml.calcular; {"error": "<SQLERRM>"} si la calificación falló.
    factores             JSONB NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(factores) = 'object'),
    reglas_aplicadas     UUID[] NOT NULL DEFAULT '{}',
    parametros_id        BIGINT REFERENCES aml.parametros (id) ON DELETE RESTRICT,
    monto_retener        NUMERIC(18, 8) NOT NULL DEFAULT 0 CHECK (monto_retener >= 0),

    CONSTRAINT ck_aml_eval_bloqueada CHECK ((resultado = 'BLOQUEADA') = (transaccion_id IS NULL)),
    CONSTRAINT ck_aml_eval_motivo CHECK ((resultado = 'BLOQUEADA') = (motivo_bloqueo IS NOT NULL)),
    CONSTRAINT ck_aml_eval_reevaluacion CHECK ((origen = 'REEVALUACION') = (evaluacion_origen_id IS NOT NULL)),
    CONSTRAINT ck_aml_eval_retener CHECK (resultado = 'RETENIDA' OR monto_retener = 0)
);
CREATE UNIQUE INDEX ux_aml_eval_tx ON aml.evaluaciones (entidad, transaccion_id, sentido)
    WHERE transaccion_id IS NOT NULL AND origen <> 'REEVALUACION';
CREATE UNIQUE INDEX ux_aml_eval_intento ON aml.evaluaciones (cliente_id, clave_operacion) WHERE resultado = 'BLOQUEADA';
CREATE UNIQUE INDEX ux_aml_eval_origen ON aml.evaluaciones (evaluacion_origen_id) WHERE evaluacion_origen_id IS NOT NULL;
CREATE INDEX ix_aml_eval_cliente ON aml.evaluaciones (cliente_id, creado_en DESC);
CREATE INDEX ix_aml_eval_pendientes ON aml.evaluaciones (creado_en) WHERE resultado = 'PENDIENTE_REEVALUACION';

-- 3.5 Congelamientos preventivos (uno ACTIVO por cliente como máximo).
CREATE TABLE aml.congelamientos (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    folio               BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE,
    cliente_id          UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    -- Copia de la cuenta IFPE al congelar (NULL si el cliente no tiene bóveda).
    cuenta_ifpe_id      UUID,
    estado              TEXT NOT NULL DEFAULT 'ACTIVO' CHECK (estado IN ('ACTIVO', 'LEVANTADO')),
    origen              TEXT NOT NULL CHECK (origen IN ('AUTOMATICO', 'LOTE', 'MANUAL', 'LISTA_BLOQUEADAS')),
    alerta_id           UUID,
    evaluacion_id       UUID REFERENCES aml.evaluaciones (id) ON DELETE RESTRICT,
    puntaje             NUMERIC(5, 2) CHECK (puntaje BETWEEN 0 AND 100),
    motivo              TEXT NOT NULL CHECK (length(btrim(motivo)) BETWEEN 10 AND 2000),
    confirmado          BOOLEAN NOT NULL DEFAULT FALSE,
    congelado_por       UUID REFERENCES kyc.clientes (id) ON DELETE RESTRICT, -- NULL = sistema
    congelado_en        TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    sla_vence_en        TIMESTAMPTZ NOT NULL,
    intentos_bloqueados INTEGER NOT NULL DEFAULT 0 CHECK (intentos_bloqueados >= 0),
    ultimo_intento_en   TIMESTAMPTZ,
    levantado_por       UUID REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    levantado_en        TIMESTAMPTZ,
    solicitud_id        UUID,
    actualizado_en      TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),

    CONSTRAINT ck_aml_congelamiento_levantado CHECK (
        (estado = 'LEVANTADO') = (levantado_en IS NOT NULL AND levantado_por IS NOT NULL AND solicitud_id IS NOT NULL)),
    CONSTRAINT ck_aml_congelamiento_autoresolucion CHECK (
        congelado_por IS DISTINCT FROM cliente_id AND levantado_por IS DISTINCT FROM cliente_id)
);
CREATE UNIQUE INDEX ux_aml_congelamiento_vigente ON aml.congelamientos (cliente_id) WHERE estado = 'ACTIVO';
CREATE INDEX ix_aml_congelamientos_folio ON aml.congelamientos (estado, folio DESC);

-- 3.6 Alertas (bandeja de cumplimiento).
CREATE TABLE aml.alertas (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    folio                 BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE,
    cliente_id            UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    -- Movimiento que la originó (NULL en intentos bloqueados, lotes, manuales,
    -- Lista de Personas Bloqueadas y algunas heredadas).
    transaccion_id        UUID,
    entidad               TEXT NOT NULL CHECK (entidad IN ('IFPE', 'SOFOM')),
    sentido               TEXT NOT NULL CHECK (sentido IN ('ENTRADA', 'SALIDA', 'NINGUNO')),
    evaluacion_id         UUID REFERENCES aml.evaluaciones (id) ON DELETE RESTRICT,
    regla_id              UUID NOT NULL REFERENCES aml.reglas_riesgo (id) ON DELETE RESTRICT,
    regla_codigo          TEXT NOT NULL,
    tipo_reporte          TEXT NOT NULL CHECK (tipo_reporte IN ('INUSUAL', 'INTERNA_PREOCUPANTE')),
    origen                TEXT NOT NULL CHECK (origen IN ('EN_LINEA', 'INTENTO_BLOQUEADO', 'LOTE', 'DIFERIDA', 'MANUAL',
                                                          'LISTA_BLOQUEADAS', 'LEGADO')),
    decision              TEXT NOT NULL CHECK (decision IN ('REGISTRADA', 'RETENIDA', 'BLOQUEADA')),
    nivel_riesgo          aml.nivel_riesgo NOT NULL,
    puntaje               NUMERIC(5, 2) NOT NULL CHECK (puntaje BETWEEN 0 AND 100),
    estado                aml.estado_alerta NOT NULL DEFAULT 'PENDIENTE',
    "timestamp"           TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    detalles              JSONB NOT NULL CHECK (jsonb_typeof(detalles) = 'object'),
    -- Sumas de ventanas de hasta 30 días (no un solo movimiento): más anchas
    -- que un monto del ledger para que ninguna suma desborde la columna.
    monto                 NUMERIC(24, 8) CHECK (monto > 0),
    monto_acumulado       NUMERIC(24, 8),
    ocurrencias           INTEGER NOT NULL DEFAULT 1 CHECK (ocurrencias >= 1),
    ultima_ocurrencia_en  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    clave_deduplicacion   TEXT NOT NULL,
    urgente               BOOLEAN NOT NULL DEFAULT FALSE,
    fecha_limite_reporte  TIMESTAMPTZ NOT NULL,
    congelamiento_id      UUID REFERENCES aml.congelamientos (id) ON DELETE RESTRICT,
    asignada_a            UUID REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    tomada_en             TIMESTAMPTZ,
    escalada              BOOLEAN NOT NULL DEFAULT FALSE,
    resolucion_propuesta  TEXT CHECK (resolucion_propuesta = 'FALSO_POSITIVO'),
    propuesta_por         UUID REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    dictamen_propuesto    TEXT,
    -- Cuándo se propuso y bajo qué solicitud de levantamiento: la aprobación
    -- de esa solicitud (y solo de esa) resuelve la propuesta, y nunca sobre
    -- evidencia que llegó después de proponerla.
    propuesta_en          TIMESTAMPTZ,
    solicitud_propuesta_id UUID,
    dictamen              TEXT,
    resuelta_por          UUID REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    resuelta_en           TIMESTAMPTZ,
    aprobada_por          UUID REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    folio_uif             TEXT CHECK (folio_uif ~ '^[A-Z0-9-]{4,40}$'),
    reportada_uif_en      DATE,
    alerta_pld_id         UUID UNIQUE REFERENCES kyc.alertas_pld (id) ON DELETE RESTRICT,
    legado_analista       TEXT,
    actualizado_en        TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),

    CONSTRAINT ck_aml_alerta_cierre CHECK (origen = 'LEGADO' OR estado NOT IN ('CONFIRMADO', 'FALSO_POSITIVO')
        OR (length(btrim(dictamen)) BETWEEN 30 AND 2000 AND resuelta_por IS NOT NULL AND resuelta_en IS NOT NULL)),
    CONSTRAINT ck_aml_alerta_revision CHECK (origen = 'LEGADO' OR estado <> 'EN_REVISION' OR asignada_a IS NOT NULL),
    CONSTRAINT ck_aml_autoresolucion CHECK (resuelta_por IS DISTINCT FROM cliente_id AND asignada_a IS DISTINCT FROM cliente_id
        AND propuesta_por IS DISTINCT FROM cliente_id AND aprobada_por IS DISTINCT FROM cliente_id),
    CONSTRAINT ck_aml_cuatro_ojos_fp CHECK (aprobada_por IS NULL OR aprobada_por IS DISTINCT FROM resuelta_por),
    CONSTRAINT ck_aml_propuesta CHECK ((resolucion_propuesta IS NULL) = (propuesta_por IS NULL)
        AND (resolucion_propuesta IS NULL) = (dictamen_propuesto IS NULL)
        AND (resolucion_propuesta IS NULL) = (propuesta_en IS NULL)
        AND (resolucion_propuesta IS NULL) = (solicitud_propuesta_id IS NULL)
        AND (resolucion_propuesta IS NULL OR estado = 'EN_REVISION')),
    CONSTRAINT ck_aml_uif CHECK ((folio_uif IS NULL) = (reportada_uif_en IS NULL) AND (folio_uif IS NULL OR estado = 'CONFIRMADO'))
);
CREATE UNIQUE INDEX ux_aml_alertas_dedup ON aml.alertas (clave_deduplicacion) WHERE estado IN ('PENDIENTE', 'EN_REVISION');
CREATE INDEX ix_aml_alertas_bandeja ON aml.alertas (estado, nivel_riesgo, folio DESC);
CREATE INDEX ix_aml_alertas_cliente ON aml.alertas (cliente_id, folio DESC);
CREATE INDEX ix_aml_alertas_actualizado ON aml.alertas (actualizado_en);
CREATE INDEX ix_aml_alertas_congelamiento ON aml.alertas (congelamiento_id);
CREATE INDEX ix_aml_alertas_supresion ON aml.alertas (cliente_id, regla_codigo, sentido, resuelta_en)
    WHERE estado = 'FALSO_POSITIVO';
CREATE INDEX ix_aml_alertas_propuesta ON aml.alertas (solicitud_propuesta_id) WHERE solicitud_propuesta_id IS NOT NULL;
CREATE INDEX ix_aml_alertas_clave ON aml.alertas (clave_deduplicacion);

-- 3.7 Evaluaciones agrupadas en una alerta (además de la primera).
CREATE TABLE aml.alerta_evaluaciones (
    alerta_id     UUID NOT NULL REFERENCES aml.alertas (id) ON DELETE RESTRICT,
    evaluacion_id UUID NOT NULL REFERENCES aml.evaluaciones (id) ON DELETE RESTRICT,
    PRIMARY KEY (alerta_id, evaluacion_id)
);

-- 3.8 Solicitudes de levantamiento o liberación (cuatro ojos).
CREATE TABLE aml.solicitudes_levantamiento (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    folio            BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE,
    congelamiento_id UUID NOT NULL REFERENCES aml.congelamientos (id) ON DELETE RESTRICT,
    tipo             TEXT NOT NULL CHECK (tipo IN ('LEVANTAR_Y_LIBERAR', 'LEVANTAR_CONSERVANDO_RETENCIONES',
                                                   'LIBERAR_RETENCIONES')),
    justificacion    TEXT NOT NULL CHECK (length(btrim(justificacion)) BETWEEN 30 AND 2000),
    solicitada_por   UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    solicitada_en    TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    -- ANULADA: el sistema la retiró porque se confirmó una alerta del cliente.
    estado           TEXT NOT NULL DEFAULT 'PENDIENTE' CHECK (estado IN ('PENDIENTE', 'APROBADA', 'RECHAZADA', 'ANULADA')),
    decidida_por     UUID REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    decidida_en      TIMESTAMPTZ,
    comentario       TEXT CHECK (length(btrim(comentario)) BETWEEN 10 AND 2000),

    CONSTRAINT ck_aml_solicitud_cuatro_ojos CHECK (decidida_por IS DISTINCT FROM solicitada_por),
    CONSTRAINT ck_aml_solicitud_decisor CHECK ((estado IN ('APROBADA', 'RECHAZADA')) = (decidida_por IS NOT NULL)),
    CONSTRAINT ck_aml_solicitud_fecha CHECK ((estado = 'PENDIENTE') = (decidida_en IS NULL))
);
CREATE UNIQUE INDEX ux_aml_solicitud_pendiente ON aml.solicitudes_levantamiento (congelamiento_id) WHERE estado = 'PENDIENTE';
CREATE INDEX ix_aml_solicitudes_estado ON aml.solicitudes_levantamiento (estado, solicitada_en);

-- 3.9 Retenciones PLD (RETENCION / LIBERACION_RETENCION en el ledger IFPE).
CREATE TABLE aml.retenciones (
    -- También es el sufijo de la clave del movimiento: 'ret:<id>' / 'lib:<id>'.
    id                    UUID PRIMARY KEY,
    congelamiento_id      UUID NOT NULL REFERENCES aml.congelamientos (id) ON DELETE RESTRICT,
    cliente_id            UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    cuenta_id             UUID NOT NULL REFERENCES ifpe.cuentas (id) ON DELETE RESTRICT,
    evaluacion_id         UUID NOT NULL REFERENCES aml.evaluaciones (id) ON DELETE RESTRICT,
    -- El abono retenido.
    transaccion_origen_id UUID NOT NULL UNIQUE,
    tx_retencion_id       UUID NOT NULL UNIQUE REFERENCES ifpe.transacciones (id) ON DELETE RESTRICT,
    monto                 NUMERIC(18, 8) NOT NULL CHECK (monto > 0),
    estado                TEXT NOT NULL DEFAULT 'VIGENTE' CHECK (estado IN ('VIGENTE', 'LIBERADA')),
    solicitud_id          UUID REFERENCES aml.solicitudes_levantamiento (id) ON DELETE RESTRICT,
    tx_liberacion_id      UUID UNIQUE REFERENCES ifpe.transacciones (id) ON DELETE RESTRICT,
    creado_en             TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    liberada_en           TIMESTAMPTZ,

    CONSTRAINT ck_aml_retencion_liberada CHECK (
        (estado = 'LIBERADA') = (tx_liberacion_id IS NOT NULL AND liberada_en IS NOT NULL AND solicitud_id IS NOT NULL))
);
CREATE INDEX ix_aml_retenciones_vigentes ON aml.retenciones (cuenta_id) WHERE estado = 'VIGENTE';
CREATE INDEX ix_aml_retenciones_congelamiento ON aml.retenciones (congelamiento_id, creado_en);

-- 3.10 Registro trimestral de operaciones relevantes (no es la bandeja).
CREATE TABLE aml.operaciones_relevantes (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cliente_id     UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    entidad        TEXT NOT NULL CHECK (entidad IN ('IFPE', 'SOFOM')),
    transaccion_id UUID NOT NULL,
    sentido        TEXT NOT NULL CHECK (sentido IN ('ENTRADA', 'SALIDA', 'NINGUNO')),
    canal          TEXT, -- NULL en las heredadas
    monto          NUMERIC(18, 8) NOT NULL CHECK (monto >= 0),
    evaluacion_id  UUID REFERENCES aml.evaluaciones (id) ON DELETE RESTRICT,
    -- Trimestre según la fecha de la Ciudad de México.
    trimestre      TEXT NOT NULL CHECK (trimestre ~ '^[0-9]{4}-T[1-4]$'),
    creado_en      TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    reporte_folio  TEXT,
    reportada_en   DATE,

    CONSTRAINT ux_aml_relevantes_tx UNIQUE (entidad, transaccion_id, sentido)
);
CREATE INDEX ix_aml_relevantes_trimestre ON aml.operaciones_relevantes (trimestre, creado_en DESC);
CREATE INDEX ix_aml_relevantes_fecha ON aml.operaciones_relevantes (creado_en DESC);
CREATE INDEX ix_aml_relevantes_cliente ON aml.operaciones_relevantes (cliente_id);

-- 3.11 Idempotencia de las acciones de los oficiales (solo inserción).
CREATE TABLE aml.acciones (
    id                 BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    clave_idempotencia TEXT NOT NULL UNIQUE,
    accion             TEXT NOT NULL,
    actor_id           UUID NOT NULL,
    objeto_id          UUID NOT NULL,
    huella             CHAR(64) NOT NULL CHECK (huella ~ '^[0-9a-f]{64}$'),
    resultado          JSONB NOT NULL,
    creado_en          TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

-- 3.12 Bandeja de salida de notificaciones (sin montos ni datos personales).
CREATE TABLE aml.notificaciones (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    creado_en        TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    tipo             TEXT NOT NULL CHECK (tipo IN ('ALERTA_NUEVA', 'ALERTA_CRITICA', 'CONGELAMIENTO', 'SOLICITUD_LEVANTAMIENTO',
                                                   'LEVANTAMIENTO_DECIDIDO', 'ESCALAMIENTO', 'SLA_CONGELAMIENTO',
                                                   'PLAZO_UIF_PROXIMO', 'PLAZO_UIF_VENCIDO', 'COBERTURA',
                                                   'EVALUACION_FALLIDA', 'CARGO_EN_CUENTA_CONGELADA',
                                                   'PROPUESTA_PENDIENTE')),
    prioridad        TEXT NOT NULL CHECK (prioridad IN ('ALTA', 'MEDIA')),
    cliente_id       UUID,
    alerta_id        UUID,
    congelamiento_id UUID,
    solicitud_id     UUID,
    propuesta_id     UUID,
    -- Solo códigos y folios, p. ej. 'R05_ESTRUCTURACION · CRITICO · alerta 42'.
    resumen          TEXT NOT NULL CHECK (length(resumen) BETWEEN 1 AND 300),
    entregada_en     TIMESTAMPTZ
);
-- El monitoreo nunca repite una notificación de plazo.
CREATE UNIQUE INDEX ux_aml_notif_alerta ON aml.notificaciones (alerta_id, tipo)
    WHERE tipo IN ('ESCALAMIENTO', 'PLAZO_UIF_PROXIMO', 'PLAZO_UIF_VENCIDO');
CREATE UNIQUE INDEX ux_aml_notif_congelamiento ON aml.notificaciones (congelamiento_id, tipo)
    WHERE tipo = 'SLA_CONGELAMIENTO';
CREATE INDEX ix_aml_notif_pendientes ON aml.notificaciones (id) WHERE entregada_en IS NULL;

-- 3.13 Bitácora de cumplimiento (inmutable; hash canónico por renglón).
CREATE TABLE aml.bitacora (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    ocurrido_en      TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    evento           TEXT NOT NULL CHECK (evento IN (
                         'ALERTA_CREADA', 'ALERTA_AGRUPADA', 'ALERTA_TOMADA', 'ALERTA_LIBERADA', 'ALERTA_ESCALADA',
                         'ALERTA_FP_PROPUESTO', 'ALERTA_FP_RETIRADO', 'ALERTA_CONFIRMADA', 'ALERTA_FALSO_POSITIVO',
                         'REPORTE_UIF_REGISTRADO', 'CONGELAMIENTO_APLICADO', 'CONGELAMIENTO_CONFIRMADO',
                         'CONGELAMIENTO_LEVANTADO', 'RETENCION_APLICADA', 'RETENCION_LIBERADA',
                         'LEVANTAMIENTO_SOLICITADO', 'LEVANTAMIENTO_APROBADO', 'LEVANTAMIENTO_RECHAZADO',
                         'LEVANTAMIENTO_ANULADO', 'OPERACION_BLOQUEADA', 'CARGO_EN_CUENTA_CONGELADA',
                         'EVALUACION_DIFERIDA', 'EVALUACION_FALLIDA', 'CONSULTA_EXPEDIENTE', 'RIESGO_CLIENTE_ELEVADO',
                         'RIESGO_CLIENTE_REDUCIDO', 'RIESGO_CLIENTE_REDUCCION_DENEGADA', 'PROPUESTA_CREADA',
                         'PROPUESTA_APROBADA', 'PROPUESTA_RECHAZADA', 'REGLA_VERSION_NUEVA',
                         'REGLA_DESACTIVADA', 'PARAMETROS_VERSION_NUEVA', 'MIGRACION_0009')),
    objeto           TEXT NOT NULL CHECK (objeto IN ('ALERTA', 'CONGELAMIENTO', 'RETENCION', 'SOLICITUD', 'PROPUESTA',
                                                     'REGLA', 'PARAMETROS', 'EVALUACION', 'OPERACION', 'CLIENTE',
                                                     'SISTEMA')),
    objeto_id        TEXT,
    cliente_id       UUID,
    alerta_id        UUID,
    congelamiento_id UUID,
    solicitud_id     UUID,
    transaccion_id   UUID,
    actor_id         UUID, -- NULL = sistema
    actor_tipo       TEXT NOT NULL CHECK (actor_tipo IN ('SISTEMA', 'CUMPLIMIENTO', 'AUDITOR', 'OPERADOR', 'MIGRACION')),
    rol_bd           TEXT NOT NULL DEFAULT current_setting('role'),
    usuario_bd       TEXT NOT NULL DEFAULT session_user,
    txid             BIGINT NOT NULL DEFAULT (pg_current_xact_id()::text::bigint),
    estado_anterior  TEXT,
    estado_nuevo     TEXT,
    motivo           TEXT,
    datos            JSONB NOT NULL DEFAULT '{}',
    hash_contenido   CHAR(64) NOT NULL CHECK (hash_contenido ~ '^[0-9a-f]{64}$')
);
CREATE INDEX ix_aml_bitacora_cliente ON aml.bitacora (cliente_id, id);
CREATE INDEX ix_aml_bitacora_alerta ON aml.bitacora (alerta_id, id);
CREATE INDEX ix_aml_bitacora_congelamiento ON aml.bitacora (congelamiento_id, id);
-- El sellado y la verificación recorren la bitácora por transacción.
CREATE INDEX ix_aml_bitacora_txid ON aml.bitacora (txid, id);

-- 3.14 Sellos de la bitácora por lotes encadenados (solo inserción). Cada
-- sello cubre los renglones de las transacciones [desde_txid, hasta_txid):
-- hasta_txid es el xmin de la instantánea al sellar, así que todas esas
-- transacciones ya terminaron y el conjunto no cambia nunca (un renglón que
-- confirma tarde pertenece a una transacción posterior y a un sello posterior).
-- desde_id / hasta_id: el primer y el último renglón del lote (informativos).
CREATE TABLE aml.sellos_bitacora (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    desde_id      BIGINT NOT NULL,
    hasta_id      BIGINT NOT NULL,
    desde_txid    BIGINT NOT NULL CHECK (desde_txid >= 0),
    hasta_txid    BIGINT NOT NULL,
    filas         INTEGER NOT NULL CHECK (filas >= 0),
    hash_anterior CHAR(64) NOT NULL CHECK (hash_anterior ~ '^[0-9a-f]{64}$'),
    hash_lote     CHAR(64) NOT NULL CHECK (hash_lote ~ '^[0-9a-f]{64}$'),
    sellado_en    TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),

    CONSTRAINT ck_aml_sello_rango CHECK (hasta_id >= desde_id AND hasta_txid > desde_txid)
);

-- 3.15 Estado del monitoreo por lotes (renglón único: turno y marcas del ledger).
-- `turno` es la ficha del arrendamiento vigente: solo quien la tiene renueva,
-- fija el rango del ciclo, avanza la marca o libera el turno.
CREATE TABLE aml.monitoreo_estado (
    id                     BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    ultima_secuencia_ifpe  BIGINT NOT NULL,
    ultima_secuencia_sofom BIGINT NOT NULL,
    en_curso_hasta         TIMESTAMPTZ NOT NULL DEFAULT '-infinity',
    turno                  UUID,
    -- Rango del ciclo en curso (aml.monitoreo_rango); la marca solo avanza hasta él.
    rango_hasta_ifpe       BIGINT,
    rango_hasta_sofom      BIGINT,
    rezago_notificado_en   TIMESTAMPTZ,
    ultimo_ciclo_en        TIMESTAMPTZ,
    ultimo_resultado       JSONB
);

-- 3.16 Verificaciones de la bitácora (solo inserción): hasta qué sello quedó
-- verificada la cadena. La verificación de la consola parte del último sello
-- verificado (incremental); una verificación fallida queda para siempre.
CREATE TABLE aml.verificaciones_bitacora (
    id            BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    hasta_sello   BIGINT NOT NULL,
    valido        BOOLEAN NOT NULL,
    primer_error  TEXT,
    filas         BIGINT NOT NULL CHECK (filas >= 0),
    verificada_en TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),

    CONSTRAINT ck_aml_verificacion_error CHECK (valido = (primer_error IS NULL))
);

-- 3.17 Clientes cuyo barrido por lotes falla: tras tres ciclos fallidos el
-- cliente queda en cuarentena (aviso COBERTURA), la marca de agua sigue y el
-- ciclo lo reintenta aparte hasta que pasa.
CREATE TABLE aml.monitoreo_fallos (
    cliente_id       UUID PRIMARY KEY REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    fallos           INTEGER NOT NULL DEFAULT 1 CHECK (fallos >= 1),
    primer_fallo_en  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    ultimo_fallo_en  TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    ultimo_error     TEXT NOT NULL,
    cuarentena_desde TIMESTAMPTZ
);

-- Llaves foráneas circulares.
ALTER TABLE aml.congelamientos
    ADD CONSTRAINT congelamientos_alerta_id_fkey FOREIGN KEY (alerta_id) REFERENCES aml.alertas (id) ON DELETE RESTRICT,
    ADD CONSTRAINT congelamientos_solicitud_id_fkey FOREIGN KEY (solicitud_id)
        REFERENCES aml.solicitudes_levantamiento (id) ON DELETE RESTRICT;
ALTER TABLE aml.alertas
    ADD CONSTRAINT alertas_solicitud_propuesta_id_fkey FOREIGN KEY (solicitud_propuesta_id)
        REFERENCES aml.solicitudes_levantamiento (id) ON DELETE RESTRICT;

-- -----------------------------------------------------------------------------
-- 4. Semillas: parámetros versión 1, matriz de riesgo versión 1, monitoreo
-- -----------------------------------------------------------------------------
-- Calibración (vinculante): un cliente de prueba es MEDIO, con cuenta de menos
-- de 30 días, sin ingreso declarado y sin PEP; una vez que dispara una regla
-- primaria suma M03 5 + M04 10 = 15. 150k/80k/80k da R01 (registro) y R02 a 65
-- ALTO sin congelar; tres abonos de 145k dan R05 a 100 CRITICO (congela y
-- retiene 435k). Ninguna prueba existente congela.
-- Toda regla que congela (R05, R11, R12, R20) llega sola a CRITICO (>= 85):
-- congela igual a un cliente BAJO o MEDIO con cuenta antigua; los
-- modificadores de perfil nunca son los que deciden un congelamiento. Los
-- escenarios normales no congelan: nómina dispersa (R11 exige concentración),
-- un abono grande, dos abonos de 95k de un cliente ALTO/PEP (banda ALTO de 25%
-- y frecuencia 3) y el pago de un crédito seguido de un pago a proveedor (el
-- cobro de un crédito no es dinero que entre a la bóveda).
INSERT INTO aml.parametros DEFAULT VALUES;

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte, activa,
                               puntos, permite_congelar)
VALUES ('R00_LEGADO', 'TODOS', 1, 'Regla heredada',
        'Destino de las alertas heredadas de kyc.alertas_pld cuya regla no existe en la matriz vigente.',
        'LEGADO', 'NINGUNO', 'INUSUAL', FALSE, 0, FALSE);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar, umbral_monto)
VALUES ('R01_OPERACION_RELEVANTE', 'TODOS', 1, 'Operación relevante',
        'Operación individual igual o mayor al umbral: va al registro trimestral de operaciones relevantes (no es una alerta).',
        'RELEVANTE', 'AMBOS', 'RELEVANTE', 0, FALSE, 150000);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar, umbral_monto, frecuencia_min, ventana_horas)
VALUES ('R02_FRACCIONAMIENTO', 'TODOS', 1, 'Fraccionamiento',
        'Abonos individualmente menores al umbral cuya suma en la ventana lo alcanza; cada abono adicional vuelve a disparar (se agrupa en la alerta abierta). No suma si ya disparó R05 o R12 con la misma evidencia.',
        'PRIMARIA', 'ENTRADA', 'INUSUAL', 50, FALSE, 150000, 2, 24);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar, umbral_monto)
VALUES ('R03_CLIENTE_RIESGO_ALTO', 'ALTO', 1, 'Cliente de riesgo alto',
        'Operación igual o mayor al umbral de un cliente clasificado como de riesgo alto.',
        'PRIMARIA', 'AMBOS', 'INUSUAL', 35, FALSE, 50000);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar, multiplo_ingreso)
SELECT 'R04_FUERA_DE_PERFIL', n, 1, 'Fuera de perfil transaccional',
       'Operación mayor a un múltiplo del ingreso mensual declarado por el cliente.',
       'PRIMARIA', 'AMBOS', 'INUSUAL', 30, FALSE, m
  FROM (VALUES ('BAJO', 3), ('MEDIO', 3), ('ALTO', 2)) v (n, m);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar, umbral_monto, banda_inferior, frecuencia_min, ventana_horas)
SELECT 'R05_ESTRUCTURACION', n, 1, 'Estructuración',
       'Varias operaciones del mismo sentido en la banda inmediatamente inferior al umbral, cuya suma lo alcanza (todos los canales).',
       'PRIMARIA', 'AMBOS', 'INUSUAL', 90, TRUE, 150000, b, f, 24
  FROM (VALUES ('BAJO', 0.80, 3), ('MEDIO', 0.75, 3), ('ALTO', 0.75, 3)) v (n, b, f);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar, volumen_max, ventana_horas)
SELECT 'R07_VOLUMEN_DIARIO', n, 1, 'Volumen diario',
       'Suma de entradas y salidas del cliente en 24 h mayor al máximo de su nivel de riesgo.',
       'PRIMARIA', 'AMBOS', 'INUSUAL', 25, FALSE, v, 24
  FROM (VALUES ('BAJO', 1000000), ('MEDIO', 500000), ('ALTO', 250000)) x (n, v);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar, operaciones_max, ventana_horas)
SELECT 'R08_FRECUENCIA', n, 1, 'Frecuencia',
       'Número de operaciones del cliente en 24 h mayor al máximo de su nivel de riesgo.',
       'PRIMARIA', 'AMBOS', 'INUSUAL', 20, FALSE, o, 24
  FROM (VALUES ('BAJO', 20), ('MEDIO', 15), ('ALTO', 10)) x (n, o);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar)
VALUES ('R09_LISTA_BLOQUEADAS', 'TODOS', 1, 'Lista de Personas Bloqueadas',
        'El cliente aparece en la Lista de Personas Bloqueadas de la UIF: alerta urgente (24 h) y congelamiento inmediato.',
        'SISTEMA', 'NINGUNO', 'INUSUAL', 100, TRUE);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar, umbral_monto, porcentaje_min, ventana_horas, banda_inferior)
VALUES ('R11_PASO_RAPIDO', 'TODOS', 1, 'Paso rápido de fondos',
        'Salidas concentradas por SPEI o transferencia que se llevan casi todo lo recibido en la bóveda en la ventana. Solo cuentan las salidas a contrapartes que se llevan cada una al menos banda_inferior de lo recibido (una nómina dispersa no es paso rápido) y solo las entradas que abonan la bóveda (SPEI y transferencias recibidas, no el cobro de un crédito).',
        'PRIMARIA', 'SALIDA', 'INUSUAL', 85, TRUE, 150000, 0.90, 24, 0.10);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar, umbral_monto, banda_inferior, frecuencia_min, volumen_max,
                               ventana_horas)
VALUES ('R12_ESTRUCTURACION_FRECUENTE', 'TODOS', 1, 'Estructuración frecuente',
        'Muchos abonos cerca del umbral (banda amplia, debajo de la banda de R05) cuya suma en la ventana alcanza un múltiplo del umbral (volumen_max). Comparte la marca de supresión de R02 y R05.',
        'PRIMARIA', 'ENTRADA', 'INUSUAL', 85, TRUE, 150000, 0.50, 5, 600000, 24);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar, umbral_monto, banda_inferior, frecuencia_min, operaciones_max,
                               ventana_horas)
SELECT 'R20_ESTRUCTURACION_LENTA', n, 1, 'Estructuración lenta',
       'Abonos en banda durante 7 días en al menos 3 días distintos (Ciudad de México), o abonos bajo el umbral en 30 días que suman al menos el doble del umbral.',
       'LOTE', 'ENTRADA', 'INUSUAL', 85, TRUE, 150000, b, 5, 8, 168
  FROM (VALUES ('BAJO', 0.80), ('MEDIO', 0.75), ('ALTO', 0.75)) v (n, b);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar, volumen_max, multiplo_ingreso, ventana_horas)
SELECT 'R21_VOLUMEN_MENSUAL', n, 1, 'Volumen mensual',
       'Entradas de 30 días mayores al máximo del nivel de riesgo o a un múltiplo del ingreso declarado.',
       'LOTE', 'ENTRADA', 'INUSUAL', 40, FALSE, v, m, 720
  FROM (VALUES ('BAJO', 3000000, 3), ('MEDIO', 2000000, 3), ('ALTO', 1000000, 2)) x (n, v, m);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar, contrapartes_min, umbral_monto, ventana_horas)
VALUES ('R22_CONCENTRACION_ENTRADAS', 'TODOS', 1, 'Concentración de entradas',
        'Muchos ordenantes distintos abonan al cliente en 72 h por un total igual o mayor al umbral.',
        'LOTE', 'ENTRADA', 'INUSUAL', 45, FALSE, 6, 150000, 72),
       ('R23_DISPERSION_SALIDAS', 'TODOS', 1, 'Dispersión de salidas',
        'El cliente envía a muchos beneficiarios distintos en 72 h por un total igual o mayor al umbral.',
        'LOTE', 'SALIDA', 'INUSUAL', 45, FALSE, 6, 150000, 72);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar, umbral_monto, porcentaje_min, ventana_horas)
VALUES ('R24_PASO_RAPIDO_48H', 'TODOS', 1, 'Paso rápido de fondos en 48 h',
        'En 48 h las salidas por SPEI o transferencia se llevan la mayor parte de lo recibido.',
        'LOTE', 'SALIDA', 'INUSUAL', 45, FALSE, 150000, 0.80, 48);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar, umbral_monto, ventana_horas)
VALUES ('R25_CIRCULAR', 'TODOS', 1, 'Flujo circular',
        'El cliente envía y recibe transferencias internas con un mismo cliente en 7 días (cada tramo igual o mayor al umbral).',
        'LOTE', 'AMBOS', 'INUSUAL', 40, FALSE, 10000, 168);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar, contrapartes_min, umbral_monto, banda_inferior, ventana_horas)
VALUES ('R26_RED_ORDENANTE', 'TODOS', 1, 'Red de un ordenante',
        'Una misma cuenta ordenante abona en banda a varios clientes distintos en 7 días.',
        'LOTE', 'ENTRADA', 'INUSUAL', 45, FALSE, 3, 150000, 0.60, 168);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar, contrapartes_min, umbral_monto, ventana_horas)
VALUES ('R27_RED_BENEFICIARIO', 'TODOS', 1, 'Red de un beneficiario',
        'Varios clientes distintos envían a una misma CLABE en 7 días por un total igual o mayor al umbral.',
        'LOTE', 'SALIDA', 'INUSUAL', 45, FALSE, 3, 150000, 168);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte,
                               puntos, permite_congelar)
VALUES ('R30_CONGELAMIENTO_MANUAL', 'TODOS', 1, 'Congelamiento manual',
        'Un oficial de cumplimiento congela al cliente sin una alerta previa.',
        'SISTEMA', 'NINGUNO', 'INUSUAL', 100, TRUE);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, puntos,
                               paises_alto_riesgo)
VALUES ('M01_PEP', 'TODOS', 1, 'Persona políticamente expuesta',
        'Modificador: el cliente es PEP (diligencia reforzada).', 'MODIFICADOR', 'AMBOS', 15, '{}'),
       -- Llamado a la acción del GAFI. Entidades y estados de México: solo
       -- desde la evaluación nacional de riesgos, por propuesta de cuatro ojos.
       ('M02_GEOGRAFIA', 'TODOS', 1, 'Geografía de alto riesgo',
        'Modificador: nacionalidad, residencia, entidad de nacimiento o estado del domicilio de alto riesgo.',
        'MODIFICADOR', 'AMBOS', 15, '{KP,IR,MM}');

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, puntos)
SELECT 'M03_NIVEL_RIESGO', n, 1, 'Nivel de riesgo del cliente',
       'Modificador por el nivel de riesgo PLD asignado al cliente.', 'MODIFICADOR', 'AMBOS', p
  FROM (VALUES ('BAJO', 0), ('MEDIO', 5), ('ALTO', 10)) v (n, p);

INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, puntos,
                               dias_antiguedad)
VALUES ('M04_CUENTA_NUEVA', 'TODOS', 1, 'Cuenta nueva',
        'Modificador: la cuenta tiene menos días de antigüedad que el parámetro.', 'MODIFICADOR', 'AMBOS', 10, 30);

-- Sin barridos retroactivos: el monitoreo empieza en las cabezas actuales.
INSERT INTO aml.monitoreo_estado (ultima_secuencia_ifpe, ultima_secuencia_sofom)
SELECT (SELECT ultima_secuencia FROM ifpe.ledger_cabeza WHERE id),
       (SELECT ultima_secuencia FROM sofom.ledger_cabeza WHERE id);

-- -----------------------------------------------------------------------------
-- 5. Utilidades (solo el dueño) y constructores JSON
-- -----------------------------------------------------------------------------
-- Todas las funciones de 0009 son SECURITY DEFINER con search_path fijo y
-- nombres calificados. Las de solo el dueño no reciben GRANT (sección 15).

-- Actor de la operación en curso. Toda función definer que es punto de entrada
-- lo fija primero (NULL = sistema, o el usuario del JWT en las funciones de
-- oficiales); los triggers de bitácora lo leen solo con aml.actor_actual(). Un
-- GUC fijado por el cliente no suplanta al actor: cada punto de entrada lo
-- sobrescribe. El motivo (dictamen de un escalamiento) viaja igual.
CREATE FUNCTION aml.fijar_actor(p_actor UUID) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM set_config('aml.actor', coalesce(p_actor::text, ''), true);
    PERFORM set_config('aml.motivo', '', true);
END
$$;

CREATE FUNCTION aml.actor_actual() RETURNS UUID
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$ SELECT nullif(current_setting('aml.actor', true), '')::uuid $$;

CREATE FUNCTION aml.fijar_motivo(p_motivo TEXT) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM set_config('aml.motivo', coalesce(p_motivo, ''), true);
END
$$;

CREATE FUNCTION aml.motivo_actual() RETURNS TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$ SELECT nullif(current_setting('aml.motivo', true), '') $$;

-- Hash canónico de un renglón de la bitácora (codificación nucleo.campo; sin
-- row::text ni texto de fechas que dependa de la sesión).
CREATE FUNCTION aml.hash_bitacora(b aml.bitacora) RETURNS CHAR(64)
LANGUAGE sql IMMUTABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT nucleo.sha256_hex(concat_ws('|',
        nucleo.campo('SC-AML-BITACORA-v1'),
        nucleo.campo(b.id::text),
        nucleo.campo(nucleo.epoch_micros(b.ocurrido_en)),
        nucleo.campo(b.evento),
        nucleo.campo(b.objeto),
        nucleo.campo(b.objeto_id),
        nucleo.campo(b.cliente_id::text),
        nucleo.campo(b.alerta_id::text),
        nucleo.campo(b.congelamiento_id::text),
        nucleo.campo(b.solicitud_id::text),
        nucleo.campo(b.transaccion_id::text),
        nucleo.campo(b.actor_id::text),
        nucleo.campo(b.actor_tipo),
        nucleo.campo(b.rol_bd),
        nucleo.campo(b.usuario_bd),
        nucleo.campo(b.txid::text),
        nucleo.campo(b.estado_anterior),
        nucleo.campo(b.estado_nuevo),
        nucleo.campo(b.motivo),
        nucleo.campo(b.datos::text)))
$$;

CREATE FUNCTION aml.registrar_bitacora(p_evento TEXT, p_objeto TEXT, p_objeto_id TEXT, p_cliente UUID, p_alerta UUID,
                                       p_congelamiento UUID, p_solicitud UUID, p_transaccion UUID, p_anterior TEXT,
                                       p_nuevo TEXT, p_motivo TEXT, p_datos JSONB) RETURNS BIGINT
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_actor UUID := aml.actor_actual();
    v_tipo  TEXT := 'SISTEMA';
    v_id    BIGINT;
BEGIN
    IF v_actor IS NOT NULL THEN
        SELECT rol INTO v_tipo FROM kyc.credenciales WHERE cliente_id = v_actor;
    END IF;
    INSERT INTO aml.bitacora (evento, objeto, objeto_id, cliente_id, alerta_id, congelamiento_id, solicitud_id,
                              transaccion_id, actor_id, actor_tipo, estado_anterior, estado_nuevo, motivo, datos)
    VALUES (p_evento, p_objeto, p_objeto_id, p_cliente, p_alerta, p_congelamiento, p_solicitud, p_transaccion, v_actor,
            v_tipo, p_anterior, p_nuevo, p_motivo, coalesce(p_datos, '{}'))
    RETURNING id INTO v_id;
    RETURN v_id;
END
$$;

-- Bandeja de salida (los índices únicos evitan repetir avisos de plazos).
CREATE FUNCTION aml.notificar(p_tipo TEXT, p_prioridad TEXT, p_cliente UUID, p_alerta UUID, p_congelamiento UUID,
                              p_solicitud UUID, p_propuesta UUID, p_resumen TEXT) RETURNS BIGINT
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_id BIGINT;
BEGIN
    INSERT INTO aml.notificaciones (tipo, prioridad, cliente_id, alerta_id, congelamiento_id, solicitud_id, propuesta_id,
                                    resumen)
    VALUES (p_tipo, p_prioridad, p_cliente, p_alerta, p_congelamiento, p_solicitud, p_propuesta, left(p_resumen, 300))
    ON CONFLICT DO NOTHING
    RETURNING id INTO v_id;
    RETURN v_id;
END
$$;

CREATE FUNCTION aml.exigir_oficial(p_actor UUID) RETURNS void
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_actor IS NULL OR NOT EXISTS (SELECT 1 FROM kyc.credenciales WHERE cliente_id = p_actor AND rol = 'CUMPLIMIENTO') THEN
        RAISE EXCEPTION 'SC_AML_NO_AUTORIZADO: se requiere un oficial de cumplimiento';
    END IF;
END
$$;

CREATE FUNCTION aml.exigir_lector(p_actor UUID) RETURNS TEXT
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_rol TEXT;
BEGIN
    SELECT rol INTO v_rol FROM kyc.credenciales WHERE cliente_id = p_actor;
    IF v_rol IS NULL OR v_rol NOT IN ('CUMPLIMIENTO', 'AUDITOR') THEN
        RAISE EXCEPTION 'SC_AML_NO_AUTORIZADO: se requiere un oficial de cumplimiento o un auditor';
    END IF;
    RETURN v_rol;
END
$$;

-- Candado por cliente: la fila de su cuenta IFPE y luego un candado
-- consultivo. Todo débito actualiza esa misma fila, así que un congelamiento
-- (que siempre toma este candado) es linealizable con los débitos.
CREATE FUNCTION aml.bloquear_cliente(p_cliente UUID) RETURNS UUID
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_cuenta UUID;
BEGIN
    SELECT id INTO v_cuenta FROM ifpe.cuentas WHERE cliente_id = p_cliente AND moneda = 'MXN' FOR NO KEY UPDATE;
    PERFORM pg_advisory_xact_lock(hashtextextended('aml:cliente:' || p_cliente::text, 0));
    RETURN v_cuenta; -- NULL si el cliente no tiene cuenta en la bóveda
END
$$;

CREATE FUNCTION aml.cliente_congelado(p_cliente UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$ SELECT EXISTS (SELECT 1 FROM aml.congelamientos WHERE cliente_id = p_cliente AND estado = 'ACTIVO') $$;

CREATE FUNCTION aml.parametros_vigentes() RETURNS aml.parametros
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$ SELECT * FROM aml.parametros ORDER BY id DESC LIMIT 1 $$;

-- Regla activa para el nivel del cliente, si no la regla 'TODOS', si no NULL
-- (la regla no aplica a ese nivel).
CREATE FUNCTION aml.regla(p_codigo TEXT, p_nivel TEXT) RETURNS aml.reglas_riesgo
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT * FROM aml.reglas_riesgo
     WHERE codigo = p_codigo AND activa AND nivel_cliente IN (p_nivel, 'TODOS')
     ORDER BY (nivel_cliente = p_nivel) DESC
     LIMIT 1
$$;

CREATE FUNCTION aml.nivel_de_puntaje(p NUMERIC) RETURNS aml.nivel_riesgo
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT CASE WHEN p < par.umbral_medio THEN 'BAJO'
                WHEN p < par.umbral_alto THEN 'MEDIO'
                WHEN p < par.umbral_critico THEN 'ALTO'
                ELSE 'CRITICO' END::aml.nivel_riesgo
      FROM aml.parametros_vigentes() par
$$;

CREATE FUNCTION aml.normalizar_texto(t TEXT) RETURNS TEXT
LANGUAGE sql IMMUTABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$ SELECT upper(btrim(translate(t, 'áéíóúüñÁÉÍÓÚÜÑ', 'aeiouunAEIOUUN'))) $$;

-- Trimestre según la fecha de la Ciudad de México: 'AAAA-Tn'.
CREATE FUNCTION aml.trimestre(ts TIMESTAMPTZ) RETURNS TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT to_char(ts AT TIME ZONE 'America/Mexico_City', 'YYYY') || '-T'
           || extract(quarter FROM ts AT TIME ZONE 'America/Mexico_City')::int
$$;

-- Detalles de una alerta agrupada: los de la ocurrencia nueva, con la unión
-- (sin repetidos, en orden, máximo 50) de los movimientos de ambas.
CREATE FUNCTION aml.fusionar_detalles(a JSONB, b JSONB) RETURNS JSONB
LANGUAGE sql IMMUTABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT b || jsonb_build_object('transacciones', coalesce((
        SELECT jsonb_agg(x ORDER BY orden)
          FROM (SELECT x, min(orden) AS orden
                  FROM jsonb_array_elements(
                           CASE WHEN jsonb_typeof(a -> 'transacciones') = 'array' THEN a -> 'transacciones' ELSE '[]' END
                           || CASE WHEN jsonb_typeof(b -> 'transacciones') = 'array' THEN b -> 'transacciones' ELSE '[]' END)
                       WITH ORDINALITY e (x, orden)
                 GROUP BY x
                 ORDER BY min(orden)
                 LIMIT 50) u), '[]'))
$$;

-- Canales en los que cuenta la estructuración (R02, R05, R20) y canales de
-- salida para el paso rápido de fondos y la dispersión (R11, R23, R24).
CREATE FUNCTION aml.canal_en_banda(p_sentido TEXT, p_canal TEXT) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT CASE p_sentido
        WHEN 'ENTRADA' THEN p_canal IN ('SPEI_WEBHOOK', 'SPEI_OPERADOR', 'SPEI_DEMO', 'TRANSFERENCIA_RECIBIDA', 'COBRO_CREDITO')
        WHEN 'SALIDA'  THEN p_canal IN ('SPEI_ENVIADO', 'TRANSFERENCIA_ENVIADA')
        ELSE FALSE
    END
$$;

CREATE FUNCTION aml.canal_de_salida(p_canal TEXT) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$ SELECT p_canal IN ('SPEI_ENVIADO', 'TRANSFERENCIA_ENVIADA') $$;

-- Entradas que abonan la bóveda del cliente (lo único que puede pasar de
-- largo, R11 y R24): el cobro de un crédito es dinero que el cliente paga a
-- la SOFOM, no dinero que recibe.
CREATE FUNCTION aml.canal_abona_boveda(p_canal TEXT) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$ SELECT p_canal IN ('SPEI_WEBHOOK', 'SPEI_OPERADOR', 'SPEI_DEMO', 'TRANSFERENCIA_RECIBIDA') $$;

-- Clave de la contraparte de un movimiento con el mismo formato que
-- aml.movimientos_cliente ('cuenta:', 'rfc:', 'nombre:', 'cliente:', 'clabe:'),
-- a partir de los hechos que trae la envoltura. NULL si no hay.
CREATE FUNCTION aml.clave_contraparte(p_canal TEXT, p_contraparte JSONB) RETURNS TEXT
LANGUAGE sql IMMUTABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT CASE
        WHEN p_canal IN ('SPEI_WEBHOOK', 'SPEI_OPERADOR', 'SPEI_DEMO') THEN
            CASE WHEN nullif(btrim(p_contraparte ->> 'cuenta'), '') IS NOT NULL THEN 'cuenta:' || btrim(p_contraparte ->> 'cuenta')
                 WHEN nullif(btrim(p_contraparte ->> 'rfc_curp'), '') IS NOT NULL
                     THEN 'rfc:' || upper(btrim(p_contraparte ->> 'rfc_curp'))
                 WHEN nullif(btrim(p_contraparte ->> 'nombre'), '') IS NOT NULL
                     THEN 'nombre:' || aml.normalizar_texto(p_contraparte ->> 'nombre')
            END
        WHEN p_canal IN ('TRANSFERENCIA_ENVIADA', 'TRANSFERENCIA_RECIBIDA') THEN
            'cliente:' || nullif(btrim(p_contraparte ->> 'cliente_id'), '')
        WHEN p_canal IN ('SPEI_ENVIADO', 'DESEMBOLSO_CREDITO') THEN 'clabe:' || nullif(btrim(p_contraparte ->> 'cuenta'), '')
    END
$$;

-- Reloj de las funciones del monitoreo que reciben un instante: solo el
-- dueño del esquema (pruebas, tareas de operación) puede fijarlo; para sc_aml
-- (la consola y el ciclo) manda el reloj del servidor, así que nadie adelanta
-- plazos ni ventanas, ni consume avisos antes de tiempo.
CREATE FUNCTION aml.reloj(p_ahora TIMESTAMPTZ) RETURNS TIMESTAMPTZ
LANGUAGE sql VOLATILE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$ SELECT CASE WHEN p_ahora IS NOT NULL AND current_setting('role') = 'none' THEN p_ahora ELSE clock_timestamp() END $$;

-- Fuerza de una decisión de alerta: REGISTRADA < RETENIDA < BLOQUEADA.
CREATE FUNCTION aml.rango_decision(p TEXT) RETURNS INTEGER
LANGUAGE sql IMMUTABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$ SELECT CASE p WHEN 'REGISTRADA' THEN 1 WHEN 'RETENIDA' THEN 2 WHEN 'BLOQUEADA' THEN 3 END $$;

-- Texto narrativo (dictamen, motivo, justificación, comentario) recortado y
-- validado en plpgsql: un CHECK fallido llegaría como RESTRICCION_VIOLADA.
CREATE FUNCTION aml.texto_narrativo(p_texto TEXT, p_min INTEGER, p_campo TEXT) RETURNS TEXT
LANGUAGE plpgsql IMMUTABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v TEXT := btrim(p_texto);
BEGIN
    IF v IS NULL OR length(v) < p_min OR length(v) > 2000 THEN
        RAISE EXCEPTION 'SC_AML_DICTAMEN_INVALIDO: % debe tener entre % y 2000 caracteres', p_campo, p_min;
    END IF;
    RETURN v;
END
$$;

CREATE FUNCTION aml.nombre_de(p_id UUID) RETURNS TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT btrim(concat_ws(' ', c.nombre, c.apellido_paterno, c.apellido_materno)) FROM kyc.clientes c WHERE c.id = p_id
$$;

CREATE FUNCTION aml.es_interno(p_cliente UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$ SELECT EXISTS (SELECT 1 FROM kyc.credenciales WHERE cliente_id = p_cliente AND rol <> 'CLIENTE') $$;

-- 5.1 Constructores JSON (contrato HTTP; montos y puntajes como texto, fechas
-- ISO-8601, enumeraciones como texto y null cuando falta el valor).
CREATE FUNCTION aml.persona_json(p_id UUID) RETURNS JSONB
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT CASE WHEN p_id IS NULL THEN NULL
                ELSE jsonb_build_object('id', p_id, 'nombre_completo', coalesce(aml.nombre_de(p_id), '')) END
$$;

CREATE FUNCTION aml.alerta_json(p_id UUID) RETURNS JSONB
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT jsonb_build_object(
        'id', a.id, 'folio', a.folio, 'estado', a.estado, 'nivel_riesgo', a.nivel_riesgo, 'puntaje', a.puntaje::text,
        'tipo_reporte', a.tipo_reporte, 'origen', a.origen, 'decision', a.decision,
        'regla', jsonb_build_object('id', r.id, 'codigo', r.codigo, 'version', r.version, 'nombre', r.nombre),
        'cliente', jsonb_build_object('id', c.id, 'nombre_completo', aml.nombre_de(c.id),
                                      'nivel_riesgo_pld', c.nivel_riesgo_pld, 'es_pep', c.es_pep,
                                      'es_interno', aml.es_interno(c.id)),
        'entidad', a.entidad, 'sentido', a.sentido, 'transaccion_id', a.transaccion_id,
        'monto', a.monto::text, 'monto_acumulado', a.monto_acumulado::text, 'ocurrencias', a.ocurrencias,
        'timestamp', a."timestamp", 'ultima_ocurrencia_en', a.ultima_ocurrencia_en, 'actualizado_en', a.actualizado_en,
        'fecha_limite_reporte', a.fecha_limite_reporte, 'urgente', a.urgente,
        'congelamiento', (SELECT jsonb_build_object('id', f.id, 'folio', f.folio, 'estado', f.estado,
                                                    'confirmado', f.confirmado, 'sla_vence_en', f.sla_vence_en)
                            FROM aml.congelamientos f WHERE f.id = a.congelamiento_id),
        'asignada_a', aml.persona_json(a.asignada_a), 'tomada_en', a.tomada_en, 'escalada', a.escalada,
        'resolucion_propuesta', a.resolucion_propuesta, 'propuesta_por', aml.persona_json(a.propuesta_por),
        'dictamen', a.dictamen, 'resuelta_por', aml.persona_json(a.resuelta_por), 'resuelta_en', a.resuelta_en,
        'aprobada_por', aml.persona_json(a.aprobada_por),
        'folio_uif', a.folio_uif, 'reportada_uif_en', to_char(a.reportada_uif_en, 'YYYY-MM-DD'))
      FROM aml.alertas a
      JOIN aml.reglas_riesgo r ON r.id = a.regla_id
      JOIN kyc.clientes c ON c.id = a.cliente_id
     WHERE a.id = p_id
$$;

CREATE FUNCTION aml.retencion_json(p_id UUID) RETURNS JSONB
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT jsonb_build_object(
        'id', r.id, 'congelamiento_id', r.congelamiento_id, 'cuenta_id', r.cuenta_id, 'evaluacion_id', r.evaluacion_id,
        'transaccion_origen_id', r.transaccion_origen_id, 'tx_retencion_id', r.tx_retencion_id, 'monto', r.monto::text,
        'estado', r.estado, 'solicitud_id', r.solicitud_id, 'tx_liberacion_id', r.tx_liberacion_id,
        'creado_en', r.creado_en, 'liberada_en', r.liberada_en)
      FROM aml.retenciones r WHERE r.id = p_id
$$;

CREATE FUNCTION aml.solicitud_json(p_id UUID) RETURNS JSONB
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT jsonb_build_object(
        'id', s.id, 'folio', s.folio, 'congelamiento_id', s.congelamiento_id, 'cliente', aml.persona_json(f.cliente_id),
        'tipo', s.tipo, 'justificacion', s.justificacion, 'solicitada_por', aml.persona_json(s.solicitada_por),
        'solicitada_en', s.solicitada_en, 'estado', s.estado, 'decidida_por', aml.persona_json(s.decidida_por),
        'decidida_en', s.decidida_en, 'comentario', s.comentario)
      FROM aml.solicitudes_levantamiento s
      JOIN aml.congelamientos f ON f.id = s.congelamiento_id
     WHERE s.id = p_id
$$;

CREATE FUNCTION aml.congelamiento_json(p_id UUID) RETURNS JSONB
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT jsonb_build_object(
        'id', f.id, 'folio', f.folio, 'cliente', aml.persona_json(f.cliente_id), 'cuenta_ifpe_id', f.cuenta_ifpe_id,
        'estado', f.estado, 'origen', f.origen, 'alerta_id', f.alerta_id, 'evaluacion_id', f.evaluacion_id,
        'puntaje', f.puntaje::text, 'motivo', f.motivo, 'confirmado', f.confirmado,
        'congelado_por', aml.persona_json(f.congelado_por), 'congelado_en', f.congelado_en,
        'sla_vence_en', f.sla_vence_en, 'intentos_bloqueados', f.intentos_bloqueados,
        'ultimo_intento_en', f.ultimo_intento_en, 'levantado_por', aml.persona_json(f.levantado_por),
        'levantado_en', f.levantado_en, 'solicitud_id', f.solicitud_id,
        'retenido_vigente', (SELECT coalesce(sum(r.monto), 0)::numeric(20, 8)::text
                               FROM aml.retenciones r WHERE r.congelamiento_id = f.id AND r.estado = 'VIGENTE'),
        'retenciones', (SELECT coalesce(jsonb_agg(aml.retencion_json(r.id) ORDER BY r.creado_en, r.id), '[]')
                          FROM aml.retenciones r WHERE r.congelamiento_id = f.id),
        'solicitud_pendiente', (SELECT aml.solicitud_json(s.id) FROM aml.solicitudes_levantamiento s
                                 WHERE s.congelamiento_id = f.id AND s.estado = 'PENDIENTE'),
        'actualizado_en', f.actualizado_en)
      FROM aml.congelamientos f WHERE f.id = p_id
$$;

CREATE FUNCTION aml.cliente_json(p_id UUID) RETURNS JSONB
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT jsonb_build_object(
        'id', c.id, 'tipo_persona', c.tipo_persona, 'nombre_completo', aml.nombre_de(c.id), 'email', c.email,
        'curp', c.curp, 'rfc', c.rfc, 'fecha_nacimiento', to_char(c.fecha_nacimiento, 'YYYY-MM-DD'),
        'nacionalidad', c.nacionalidad::text, 'pais_residencia', c.pais_residencia::text,
        'entidad_nacimiento', p.entidad_nacimiento::text,
        'domicilio', jsonb_build_object('estado', d.dom ->> 'estado', 'municipio', d.dom ->> 'municipio',
                                        'codigo_postal', d.dom ->> 'codigo_postal'),
        'actividad_economica', c.actividad_economica, 'ocupacion', c.ocupacion,
        'ingreso_mensual_estimado', c.ingreso_mensual_estimado::text,
        'nivel_riesgo_pld', c.nivel_riesgo_pld, 'riesgo_evaluado_en', c.riesgo_evaluado_en,
        'es_pep', c.es_pep, 'en_lista_bloqueadas', c.en_lista_bloqueadas,
        'estatus_kyc', c.estatus_kyc, 'kyc_verificado_en', c.kyc_verificado_en,
        'es_interno', aml.es_interno(c.id), 'cliente_desde', c.creado_en)
      FROM kyc.clientes c
      LEFT JOIN kyc.kyc_perfiles p ON p.cliente_id = c.id
     CROSS JOIN LATERAL (SELECT CASE WHEN jsonb_typeof(p.domicilio) = 'object' AND p.domicilio <> '{}' THEN p.domicilio
                                     WHEN jsonb_typeof(c.domicilio) = 'object' THEN c.domicilio
                                     ELSE '{}'::jsonb END AS dom) d
     WHERE c.id = p_id
$$;

-- Estados reales de las cuentas (aquí sí se ve CONGELADO_PREVENTIVO).
CREATE FUNCTION aml.cuentas_json(p_cliente UUID) RETURNS JSONB
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT jsonb_build_object(
        'ifpe', (SELECT jsonb_build_object(
                     'id', i.id, 'clabe', i.clabe::text, 'estado', i.estado,
                     'saldo_disponible', i.saldo_disponible::text, 'saldo_bloqueado', i.saldo_bloqueado::text,
                     'retenido_aml', (SELECT coalesce(sum(r.monto), 0)::numeric(18, 8)::text
                                        FROM aml.retenciones r WHERE r.cuenta_id = i.id AND r.estado = 'VIGENTE'),
                     'creado_en', i.creado_en)
                   FROM ifpe.cuentas i WHERE i.cliente_id = p_cliente AND i.moneda = 'MXN'),
        'sofom', (SELECT jsonb_build_object(
                      'id', s.id, 'estado', s.estado, 'saldo_disponible', s.saldo_disponible::text,
                      'saldo_bloqueado', s.saldo_bloqueado::text, 'creado_en', s.creado_en)
                    FROM sofom.cuentas s WHERE s.cliente_id = p_cliente AND s.moneda = 'MXN'))
$$;

CREATE FUNCTION aml.regla_json(p_id UUID) RETURNS JSONB
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT jsonb_build_object(
        'id', r.id, 'codigo', r.codigo, 'nivel_cliente', r.nivel_cliente, 'version', r.version, 'nombre', r.nombre,
        'descripcion', r.descripcion, 'clase', r.clase, 'sentido', r.sentido, 'tipo_reporte', r.tipo_reporte,
        'activa', r.activa, 'puntos', r.puntos::text, 'permite_congelar', r.permite_congelar,
        'umbral_monto', r.umbral_monto::text, 'banda_inferior', r.banda_inferior::text,
        'frecuencia_min', r.frecuencia_min, 'ventana_horas', r.ventana_horas, 'volumen_max', r.volumen_max::text,
        'operaciones_max', r.operaciones_max, 'multiplo_ingreso', r.multiplo_ingreso::text,
        'porcentaje_min', r.porcentaje_min::text, 'contrapartes_min', r.contrapartes_min,
        'dias_antiguedad', r.dias_antiguedad, 'paises_alto_riesgo', to_jsonb(r.paises_alto_riesgo::text[]),
        'entidades_alto_riesgo', to_jsonb(r.entidades_alto_riesgo::text[]),
        'estados_alto_riesgo', to_jsonb(r.estados_alto_riesgo), 'propuesta_id', r.propuesta_id,
        'creado_en', r.creado_en, 'desactivada_en', r.desactivada_en)
      FROM aml.reglas_riesgo r WHERE r.id = p_id
$$;

CREATE FUNCTION aml.parametros_json() RETURNS JSONB
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT jsonb_build_object(
        'id', p.id, 'umbral_medio', p.umbral_medio::text, 'umbral_alto', p.umbral_alto::text,
        'umbral_critico', p.umbral_critico::text, 'modo_observacion', p.modo_observacion,
        'sla_critica_horas', p.sla_critica_horas, 'sla_congelamiento_horas', p.sla_congelamiento_horas,
        'dias_reporte_inusual', p.dias_reporte_inusual, 'horas_reporte_urgente', p.horas_reporte_urgente,
        'dias_aviso_plazo', p.dias_aviso_plazo, 'alertar_desde', p.alertar_desde, 'propuesta_id', p.propuesta_id,
        'creado_en', p.creado_en)
      FROM aml.parametros_vigentes() p
$$;

CREATE FUNCTION aml.propuesta_json(p_id UUID) RETURNS JSONB
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT jsonb_build_object(
        'id', p.id, 'folio', p.folio, 'tipo', p.tipo, 'regla_codigo', p.regla_codigo, 'nivel_cliente', p.nivel_cliente,
        'cambios', p.cambios, 'justificacion', p.justificacion, 'propuesta_por', aml.persona_json(p.propuesta_por),
        'propuesta_en', p.propuesta_en, 'estado', p.estado, 'decidida_por', aml.persona_json(p.decidida_por),
        'decidida_en', p.decidida_en, 'comentario', p.comentario, 'resultado_id', p.resultado_id)
      FROM aml.propuestas_cambio p WHERE p.id = p_id
$$;

CREATE FUNCTION aml.evaluacion_json(p_id UUID) RETURNS JSONB
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT jsonb_build_object(
        'id', e.id, 'creado_en', e.creado_en, 'cliente_id', e.cliente_id, 'entidad', e.entidad, 'sentido', e.sentido,
        'canal', e.canal, 'monto', e.monto::text, 'transaccion_id', e.transaccion_id,
        'clave_operacion', e.clave_operacion,
        'contraparte', jsonb_build_object('nombre', e.contraparte ->> 'nombre', 'cuenta', e.contraparte ->> 'cuenta',
                                          'rfc_curp', e.contraparte ->> 'rfc_curp',
                                          'cliente_id', e.contraparte ->> 'cliente_id'),
        'puntaje', e.puntaje::text, 'nivel', e.nivel, 'resultado', e.resultado, 'motivo_bloqueo', e.motivo_bloqueo,
        'origen', e.origen, 'evaluacion_origen_id', e.evaluacion_origen_id, 'factores', e.factores,
        'reglas_aplicadas', to_jsonb(e.reglas_aplicadas), 'parametros_id', e.parametros_id,
        'monto_retener', e.monto_retener::text)
      FROM aml.evaluaciones e WHERE e.id = p_id
$$;

-- Contadores de la barra de resumen. Sin los registros cuyo sujeto es el
-- propio lector (p_actor): un oficial o auditor nunca ve, ni contados, sus
-- alertas, congelamientos, solicitudes ni avisos (sin aviso al sujeto).
CREATE FUNCTION aml.resumen(p_actor UUID DEFAULT NULL) RETURNS JSONB
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    WITH par AS (SELECT * FROM aml.parametros_vigentes()),
         ahora AS (SELECT clock_timestamp() AS t),
         abiertas AS (SELECT nivel_riesgo, estado FROM aml.alertas
                       WHERE estado IN ('PENDIENTE', 'EN_REVISION') AND cliente_id IS DISTINCT FROM p_actor),
         plazos AS (SELECT a.fecha_limite_reporte FROM aml.alertas a
                     WHERE (a.estado IN ('PENDIENTE', 'EN_REVISION') OR (a.estado = 'CONFIRMADO' AND a.folio_uif IS NULL))
                       AND a.cliente_id IS DISTINCT FROM p_actor),
         activos AS (SELECT f.id, f.sla_vence_en FROM aml.congelamientos f
                      WHERE f.estado = 'ACTIVO' AND f.cliente_id IS DISTINCT FROM p_actor)
    SELECT jsonb_build_object(
        'pendientes', (SELECT count(*) FROM abiertas WHERE estado = 'PENDIENTE'),
        'en_revision', (SELECT count(*) FROM abiertas WHERE estado = 'EN_REVISION'),
        'abiertas_por_nivel', jsonb_build_object(
            'BAJO', (SELECT count(*) FROM abiertas WHERE nivel_riesgo = 'BAJO'),
            'MEDIO', (SELECT count(*) FROM abiertas WHERE nivel_riesgo = 'MEDIO'),
            'ALTO', (SELECT count(*) FROM abiertas WHERE nivel_riesgo = 'ALTO'),
            'CRITICO', (SELECT count(*) FROM abiertas WHERE nivel_riesgo = 'CRITICO')),
        'criticas_abiertas', (SELECT count(*) FROM abiertas WHERE nivel_riesgo = 'CRITICO'),
        'congelamientos_activos', (SELECT count(*) FROM activos),
        'solicitudes_pendientes', (SELECT count(*) FROM aml.solicitudes_levantamiento s
                                     JOIN aml.congelamientos f ON f.id = s.congelamiento_id
                                    WHERE s.estado = 'PENDIENTE' AND f.cliente_id IS DISTINCT FROM p_actor),
        'propuestas_pendientes', (SELECT count(*) FROM aml.propuestas_cambio WHERE estado = 'PENDIENTE'),
        'sla_congelamiento_vencidos', (
            SELECT count(*) FROM activos f, ahora
             WHERE f.sla_vence_en < ahora.t
               AND NOT EXISTS (SELECT 1 FROM aml.solicitudes_levantamiento s
                                WHERE s.congelamiento_id = f.id AND s.estado = 'PENDIENTE')),
        'reportes_por_vencer', (
            SELECT count(*) FROM plazos, ahora, par
             WHERE plazos.fecha_limite_reporte >= ahora.t
               AND plazos.fecha_limite_reporte <= ahora.t + par.dias_aviso_plazo * interval '1 day'),
        'reportes_vencidos', (SELECT count(*) FROM plazos, ahora WHERE plazos.fecha_limite_reporte < ahora.t),
        'notificaciones_sin_entregar', (SELECT count(*) FROM aml.notificaciones
                                         WHERE entregada_en IS NULL AND cliente_id IS DISTINCT FROM p_actor),
        'modo_observacion', (SELECT modo_observacion FROM par),
        'ultimo_ciclo_en', (SELECT ultimo_ciclo_en FROM aml.monitoreo_estado WHERE id),
        'servidor_en', (SELECT t FROM ahora))
$$;

-- -----------------------------------------------------------------------------
-- 6. Motor: velocidad desde el ledger, calificación, evaluación, alertas,
--    congelamientos y retenciones (solo el dueño; se llega por las envolturas)
-- -----------------------------------------------------------------------------

-- 6.1 Única fuente de velocidad: los movimientos de dinero de un cliente en el
-- ledger, en la ventana (p_desde, p_hasta], sin p_excluir. UNION ALL de rangos
-- de índice (nunca OR). Nunca cuentan: reversos 'spei-reverso:', correcciones
-- 'spei-correccion:', RETORNO_DE_SOFOM, retenciones ni las patas SOFOM del mutuo.
CREATE FUNCTION aml.movimientos_cliente(p_cliente UUID, p_desde TIMESTAMPTZ, p_hasta TIMESTAMPTZ, p_excluir UUID)
RETURNS TABLE (entidad TEXT, transaccion_id UUID, sentido TEXT, canal TEXT, monto NUMERIC, contraparte TEXT,
               creado_en TIMESTAMPTZ)
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    WITH cuenta AS MATERIALIZED (
        SELECT c.id FROM ifpe.cuentas c WHERE c.cliente_id = p_cliente AND c.moneda = 'MXN'
    )
    -- SPEI recibido (webhook, captura del operador, demostración), sin anulados.
    SELECT 'IFPE', t.id, 'ENTRADA',
           CASE s.origen WHEN 'OPERADOR' THEN 'SPEI_OPERADOR' WHEN 'DEMO' THEN 'SPEI_DEMO' ELSE 'SPEI_WEBHOOK' END,
           t.monto::numeric,
           CASE WHEN s.cuenta_ordenante IS NOT NULL THEN 'cuenta:' || s.cuenta_ordenante
                WHEN nullif(btrim(s.rfc_curp_ordenante), '') IS NOT NULL THEN 'rfc:' || upper(btrim(s.rfc_curp_ordenante))
                WHEN nullif(btrim(s.nombre_ordenante), '') IS NOT NULL
                    THEN 'nombre:' || aml.normalizar_texto(s.nombre_ordenante)
           END,
           t.creado_en
      FROM cuenta
      JOIN ifpe.transacciones t ON t.cuenta_destino_id = cuenta.id
      JOIN ifpe.spei_inbound s ON s.transaccion_id = t.id
     WHERE t.tipo = 'DEPOSITO' AND t.clave_idempotencia LIKE 'spei:%'
       AND t.creado_en > p_desde AND t.creado_en <= p_hasta AND t.id IS DISTINCT FROM p_excluir
       AND NOT EXISTS (SELECT 1 FROM ifpe.spei_anulaciones a WHERE a.spei_inbound_id = s.id)
    UNION ALL
    -- Transferencia interna recibida.
    SELECT 'IFPE', t.id, 'ENTRADA', 'TRANSFERENCIA_RECIBIDA', t.monto::numeric,
           'cliente:' || (SELECT o.cliente_id FROM ifpe.cuentas o WHERE o.id = t.cuenta_origen_id)::text,
           t.creado_en
      FROM cuenta
      JOIN ifpe.transacciones t ON t.cuenta_destino_id = cuenta.id
     WHERE t.tipo = 'TRANSFERENCIA'
       AND t.creado_en > p_desde AND t.creado_en <= p_hasta AND t.id IS DISTINCT FROM p_excluir
    UNION ALL
    -- SPEI enviado (la CLABE beneficiaria está en referencia_externa desde 0005).
    SELECT 'IFPE', t.id, 'SALIDA', 'SPEI_ENVIADO', t.monto::numeric, 'clabe:' || t.referencia_externa, t.creado_en
      FROM cuenta
      JOIN ifpe.transacciones t ON t.cuenta_origen_id = cuenta.id
     WHERE t.tipo = 'RETIRO' AND t.clave_idempotencia NOT LIKE 'spei-correccion:%'
       AND t.creado_en > p_desde AND t.creado_en <= p_hasta AND t.id IS DISTINCT FROM p_excluir
    UNION ALL
    -- Transferencia interna enviada.
    SELECT 'IFPE', t.id, 'SALIDA', 'TRANSFERENCIA_ENVIADA', t.monto::numeric,
           'cliente:' || (SELECT d.cliente_id FROM ifpe.cuentas d WHERE d.id = t.cuenta_destino_id)::text,
           t.creado_en
      FROM cuenta
      JOIN ifpe.transacciones t ON t.cuenta_origen_id = cuenta.id
     WHERE t.tipo = 'TRANSFERENCIA'
       AND t.creado_en > p_desde AND t.creado_en <= p_hasta AND t.id IS DISTINCT FROM p_excluir
    UNION ALL
    -- Inversión en mutuo.
    SELECT 'IFPE', t.id, 'SALIDA', 'INVERSION_MUTUO', t.monto::numeric, NULL::text, t.creado_en
      FROM cuenta
      JOIN ifpe.transacciones t ON t.cuenta_origen_id = cuenta.id
     WHERE t.tipo = 'TRASPASO_A_SOFOM'
       AND t.creado_en > p_desde AND t.creado_en <= p_hasta AND t.id IS DISTINCT FROM p_excluir
    UNION ALL
    -- Desembolso de un crédito del cliente.
    SELECT 'SOFOM', t.id, 'SALIDA', 'DESEMBOLSO_CREDITO', t.monto::numeric, 'clabe:' || c.clabe_desembolso::text,
           t.creado_en
      FROM sofom.creditos c
      JOIN sofom.transacciones t ON t.credito_id = c.id
     WHERE c.acreditado_id = p_cliente AND t.tipo = 'DESEMBOLSO_CREDITO'
       AND t.creado_en > p_desde AND t.creado_en <= p_hasta AND t.id IS DISTINCT FROM p_excluir
    UNION ALL
    -- Cobro de un crédito del cliente.
    SELECT 'SOFOM', t.id, 'ENTRADA', 'COBRO_CREDITO', t.monto::numeric, NULL::text, t.creado_en
      FROM sofom.creditos c
      JOIN sofom.transacciones t ON t.credito_id = c.id
     WHERE c.acreditado_id = p_cliente AND t.tipo = 'COBRO_CREDITO'
       AND t.creado_en > p_desde AND t.creado_en <= p_hasta AND t.id IS DISTINCT FROM p_excluir
$$;

-- Marca de supresión de una regla de patrón: la resolución FALSO_POSITIVO más
-- reciente del cliente para esa regla y ese sentido (el de la llave de la
-- alerta; NULL: cualquiera). Los movimientos revisados no vuelven a contar
-- (ni a congelar al cliente); los del otro sentido no se revisaron.
CREATE FUNCTION aml.marca_falso_positivo(p_cliente UUID, p_codigo TEXT, p_sentido TEXT) RETURNS TIMESTAMPTZ
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT coalesce(max(resuelta_en), '-infinity'::timestamptz) FROM aml.alertas
     WHERE cliente_id = p_cliente AND regla_codigo = p_codigo AND estado = 'FALSO_POSITIVO'
       AND (p_sentido IS NULL OR sentido = p_sentido)
$$;

-- Perfil del cliente que entra en la calificación (se guarda con ella).
CREATE FUNCTION aml.perfil_cliente(p_cliente UUID, p_ahora TIMESTAMPTZ) RETURNS JSONB
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT jsonb_build_object(
        'nivel_riesgo_pld', c.nivel_riesgo_pld,
        'es_pep', c.es_pep,
        'nacionalidad', coalesce(p.nacionalidad, c.nacionalidad)::text,
        'pais_residencia', c.pais_residencia::text,
        'entidad_nacimiento', p.entidad_nacimiento::text,
        'estado_domicilio', aml.normalizar_texto(coalesce(p.domicilio ->> 'estado',
                                                          CASE WHEN jsonb_typeof(c.domicilio) = 'object'
                                                               THEN c.domicilio ->> 'estado' END)),
        'ingreso_mensual_estimado', c.ingreso_mensual_estimado::text,
        'antiguedad_dias', floor(extract(epoch FROM p_ahora - coalesce(i.creado_en, c.creado_en)) / 86400)::int,
        'es_interno', aml.es_interno(c.id))
      FROM kyc.clientes c
      LEFT JOIN kyc.kyc_perfiles p ON p.cliente_id = c.id
      LEFT JOIN ifpe.cuentas i ON i.cliente_id = c.id AND i.moneda = 'MXN'
     WHERE c.id = p_cliente
$$;

-- Un factor de la calificación (regla primaria o modificador disparado).
CREATE FUNCTION aml.factor(r aml.reglas_riesgo, p_tipo TEXT, p_evidencia JSONB) RETURNS JSONB
LANGUAGE sql IMMUTABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT jsonb_build_object('codigo', r.codigo, 'regla_id', r.id, 'version', r.version, 'tipo', p_tipo,
                              'puntos', r.puntos::text, 'permite_congelar', r.permite_congelar, 'evidencia', p_evidencia)
$$;

-- Modificadores del perfil (solo cuentan si disparó una regla primaria).
CREATE FUNCTION aml.modificadores(p_perfil JSONB) RETURNS TABLE (factores JSONB, puntos NUMERIC, reglas UUID[])
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_tier TEXT := p_perfil ->> 'nivel_riesgo_pld';
    r      aml.reglas_riesgo;
BEGIN
    factores := '[]';
    puntos := 0;
    reglas := '{}';

    r := aml.regla('M01_PEP', v_tier);
    IF r.id IS NOT NULL THEN
        reglas := reglas || r.id;
        IF (p_perfil ->> 'es_pep')::boolean THEN
            factores := factores || jsonb_build_array(aml.factor(r, 'MODIFICADOR', jsonb_build_object('es_pep', TRUE)));
            puntos := puntos + r.puntos;
        END IF;
    END IF;

    r := aml.regla('M02_GEOGRAFIA', v_tier);
    IF r.id IS NOT NULL THEN
        reglas := reglas || r.id;
        IF (p_perfil ->> 'nacionalidad') = ANY (r.paises_alto_riesgo::text[])
           OR (p_perfil ->> 'pais_residencia') = ANY (r.paises_alto_riesgo::text[])
           OR (p_perfil ->> 'entidad_nacimiento') = ANY (r.entidades_alto_riesgo::text[])
           OR (p_perfil ->> 'estado_domicilio') = ANY (r.estados_alto_riesgo) THEN
            factores := factores || jsonb_build_array(aml.factor(r, 'MODIFICADOR', jsonb_build_object(
                'nacionalidad', p_perfil -> 'nacionalidad', 'pais_residencia', p_perfil -> 'pais_residencia',
                'entidad_nacimiento', p_perfil -> 'entidad_nacimiento',
                'estado_domicilio', p_perfil -> 'estado_domicilio')));
            puntos := puntos + r.puntos;
        END IF;
    END IF;

    r := aml.regla('M03_NIVEL_RIESGO', v_tier);
    IF r.id IS NOT NULL THEN
        reglas := reglas || r.id;
        IF r.puntos > 0 THEN
            factores := factores || jsonb_build_array(aml.factor(r, 'MODIFICADOR',
                jsonb_build_object('nivel_riesgo_pld', v_tier)));
            puntos := puntos + r.puntos;
        END IF;
    END IF;

    r := aml.regla('M04_CUENTA_NUEVA', v_tier);
    IF r.id IS NOT NULL THEN
        reglas := reglas || r.id;
        IF (p_perfil ->> 'antiguedad_dias')::int < r.dias_antiguedad THEN
            factores := factores || jsonb_build_array(aml.factor(r, 'MODIFICADOR', jsonb_build_object(
                'antiguedad_dias', (p_perfil ->> 'antiguedad_dias')::int, 'dias_antiguedad', r.dias_antiguedad)));
            puntos := puntos + r.puntos;
        END IF;
    END IF;
    RETURN NEXT;
END
$$;

-- 6.2 Calificación (solo lectura). Debe invocarse en una sentencia posterior
-- a la del candado del cliente, para ver una foto READ COMMITTED fresca.
-- Puntaje = LEAST(100, Σ primarias + Σ modificadores); los modificadores solo
-- cuentan si disparó una primaria. `congelar` exige CRITICO y una regla
-- disparada con permite_congelar (el modo observación lo aplica quien llama).
-- p_contraparte: clave de la contraparte del movimiento (aml.clave_contraparte).
CREATE FUNCTION aml.calcular(p_cliente UUID, p_entidad TEXT, p_sentido TEXT, p_canal TEXT, p_monto NUMERIC,
                             p_excluir UUID, p_ahora TIMESTAMPTZ, p_contraparte TEXT) RETURNS JSONB
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    par          aml.parametros := aml.parametros_vigentes();
    v_perfil     JSONB;
    v_tier       TEXT;
    v_ingreso    NUMERIC;
    v_w          aml.movimiento[];
    r            aml.reglas_riesgo;
    v_factores   JSONB := '[]';
    v_primarias  TEXT[] := '{}';
    v_aplicadas  UUID[] := '{}';
    v_puntos     NUMERIC := 0;
    v_congelable BOOLEAN := FALSE;
    v_desde      TIMESTAMPTZ;
    v_dia        TIMESTAMPTZ;
    v_wm_e       TIMESTAMPTZ;
    v_wm_s       TIMESTAMPTZ;
    v_n          BIGINT;
    v_suma       NUMERIC;
    v_ids        JSONB;
    v_lo         NUMERIC;
    v_hi         NUMERIC;
    v_e          NUMERIC;
    v_o          NUMERIC;
    v_oc         NUMERIC;
    v_nc         BIGINT;
    v_relevante  BOOLEAN := FALSE;
    v_regla_rel  UUID;
    v_estructura BOOLEAN := FALSE;
    v_p05_ids    JSONB := '[]';
    v_p05_suma   NUMERIC := 0;
    v_p12_ids    JSONB := '[]';
    v_p12_suma   NUMERIC := 0;
    v_b_ids      JSONB := '[]';
    v_b_suma     NUMERIC := 0;
    v_mod        RECORD;
    v_bruto      NUMERIC;
    v_puntaje    NUMERIC;
    v_dominante  JSONB;
    v_ventana    JSONB;
BEGIN
    IF NOT EXISTS (SELECT 1 FROM kyc.clientes WHERE id = p_cliente) THEN
        RAISE EXCEPTION 'SC_AML_PARAMETRO_INVALIDO: el cliente % no existe', p_cliente;
    END IF;
    v_perfil := aml.perfil_cliente(p_cliente, p_ahora);
    v_tier := v_perfil ->> 'nivel_riesgo_pld';
    v_ingreso := (v_perfil ->> 'ingreso_mensual_estimado')::numeric;
    -- Inicio del día (Ciudad de México) del movimiento: R03 y R04 se agrupan por día.
    v_dia := date_trunc('day', p_ahora AT TIME ZONE 'America/Mexico_City') AT TIME ZONE 'America/Mexico_City';

    -- Ventana de 24 h, leída una sola vez.
    v_w := ARRAY(SELECT ROW(m.entidad, m.transaccion_id, m.sentido, m.canal, m.monto, m.contraparte,
                            m.creado_en)::aml.movimiento
                   FROM aml.movimientos_cliente(p_cliente, p_ahora - interval '24 hours', p_ahora, p_excluir) m
                  ORDER BY m.creado_en, m.transaccion_id);

    -- R01: registro de operaciones relevantes (sin puntos).
    r := aml.regla('R01_OPERACION_RELEVANTE', v_tier);
    IF r.id IS NOT NULL THEN
        v_aplicadas := v_aplicadas || r.id;
        v_regla_rel := r.id;
        v_relevante := p_monto >= r.umbral_monto;
    END IF;

    -- R05: estructuración en la banda inferior al umbral, mismo sentido (marca
    -- de supresión de ese sentido).
    r := aml.regla('R05_ESTRUCTURACION', v_tier);
    IF r.id IS NOT NULL THEN
        v_aplicadas := v_aplicadas || r.id;
        IF aml.canal_en_banda(p_sentido, p_canal) THEN
            v_lo := r.banda_inferior * r.umbral_monto;
            v_hi := r.umbral_monto;
            v_desde := greatest(p_ahora - r.ventana_horas * interval '1 hour',
                                aml.marca_falso_positivo(p_cliente, r.codigo, p_sentido));
            SELECT count(*), coalesce(sum(m.monto), 0), coalesce(jsonb_agg(m.transaccion_id ORDER BY m.creado_en), '[]')
              INTO v_n, v_suma, v_ids
              FROM unnest(v_w) m
             WHERE m.sentido = p_sentido AND aml.canal_en_banda(m.sentido, m.canal)
               AND m.monto >= v_lo AND m.monto < v_hi AND m.creado_en > v_desde;
            IF p_monto >= v_lo AND p_monto < v_hi AND v_n + 1 >= r.frecuencia_min AND v_suma + p_monto >= r.umbral_monto THEN
                v_factores := v_factores || jsonb_build_array(aml.factor(r, 'PRIMARIA', jsonb_build_object(
                    'banda', jsonb_build_array(round(v_lo, 8)::text, round(v_hi, 8)::text), 'n', v_n + 1,
                    'monto', (v_suma + p_monto)::text, 'frecuencia_min', r.frecuencia_min, 'desde', v_desde,
                    'transacciones', v_ids)));
                v_primarias := v_primarias || r.codigo;
                v_puntos := v_puntos + r.puntos;
                v_congelable := v_congelable OR r.permite_congelar;
                v_estructura := TRUE;
                v_p05_ids := v_ids;
                v_p05_suma := v_suma;
            END IF;
        END IF;
    END IF;

    -- R12: estructuración frecuente. Muchos abonos en una banda amplia bajo el
    -- umbral cuya suma alcanza un múltiplo del umbral (volumen_max): atrapa a
    -- quien se queda un peso debajo de la banda de R05. Comparte la marca de
    -- supresión de la familia (R02, R05, R12): lo revisado no vuelve a contar.
    r := aml.regla('R12_ESTRUCTURACION_FRECUENTE', v_tier);
    IF r.id IS NOT NULL THEN
        v_aplicadas := v_aplicadas || r.id;
        IF p_sentido = 'ENTRADA' AND aml.canal_en_banda(p_sentido, p_canal) THEN
            v_lo := r.banda_inferior * r.umbral_monto;
            v_hi := r.umbral_monto;
            v_desde := greatest(p_ahora - r.ventana_horas * interval '1 hour',
                                aml.marca_falso_positivo(p_cliente, r.codigo, 'ENTRADA'),
                                aml.marca_falso_positivo(p_cliente, 'R05_ESTRUCTURACION', 'ENTRADA'),
                                aml.marca_falso_positivo(p_cliente, 'R02_FRACCIONAMIENTO', 'ENTRADA'));
            SELECT count(*), coalesce(sum(m.monto), 0), coalesce(jsonb_agg(m.transaccion_id ORDER BY m.creado_en), '[]')
              INTO v_n, v_suma, v_ids
              FROM unnest(v_w) m
             WHERE m.sentido = 'ENTRADA' AND aml.canal_en_banda(m.sentido, m.canal)
               AND m.monto >= v_lo AND m.monto < v_hi AND m.creado_en > v_desde;
            IF p_monto >= v_lo AND p_monto < v_hi AND v_n + 1 >= r.frecuencia_min AND v_suma + p_monto >= r.volumen_max THEN
                v_factores := v_factores || jsonb_build_array(aml.factor(r, 'PRIMARIA', jsonb_build_object(
                    'banda', jsonb_build_array(round(v_lo, 8)::text, round(v_hi, 8)::text), 'n', v_n + 1,
                    'monto', (v_suma + p_monto)::text, 'frecuencia_min', r.frecuencia_min,
                    'monto_min', r.volumen_max::text, 'desde', v_desde, 'transacciones', v_ids)));
                v_primarias := v_primarias || r.codigo;
                v_puntos := v_puntos + r.puntos;
                v_congelable := v_congelable OR r.permite_congelar;
                v_estructura := TRUE;
                v_p12_ids := v_ids;
                v_p12_suma := v_suma;
            END IF;
        END IF;
    END IF;

    -- R02: abonos bajo el umbral cuya suma lo alcanza. Vuelve a disparar con
    -- cada abono posterior (la alerta abierta los agrupa); no suma cuando R05 o
    -- R12 ya dispararon con la misma evidencia.
    r := aml.regla('R02_FRACCIONAMIENTO', v_tier);
    IF r.id IS NOT NULL THEN
        v_aplicadas := v_aplicadas || r.id;
        IF p_sentido = 'ENTRADA' AND aml.canal_en_banda(p_sentido, p_canal) AND NOT v_estructura THEN
            v_desde := greatest(p_ahora - r.ventana_horas * interval '1 hour',
                                aml.marca_falso_positivo(p_cliente, r.codigo, 'ENTRADA'));
            SELECT count(*), coalesce(sum(m.monto), 0), coalesce(jsonb_agg(m.transaccion_id ORDER BY m.creado_en), '[]')
              INTO v_n, v_suma, v_ids
              FROM unnest(v_w) m
             WHERE m.sentido = 'ENTRADA' AND aml.canal_en_banda(m.sentido, m.canal)
               AND m.monto < r.umbral_monto AND m.creado_en > v_desde;
            IF p_monto < r.umbral_monto AND v_n + 1 >= r.frecuencia_min AND v_suma + p_monto >= r.umbral_monto THEN
                v_factores := v_factores || jsonb_build_array(aml.factor(r, 'PRIMARIA', jsonb_build_object(
                    'n', v_n + 1, 'monto', (v_suma + p_monto)::text, 'umbral', r.umbral_monto::text, 'desde', v_desde,
                    'transacciones', v_ids)));
                v_primarias := v_primarias || r.codigo;
                v_puntos := v_puntos + r.puntos;
                v_congelable := v_congelable OR r.permite_congelar;
            END IF;
        END IF;
    END IF;

    -- R03: cliente de riesgo alto (la regla existe solo para ese nivel). Una
    -- alerta por cliente, sentido y día; un falso positivo la suprime el resto
    -- del día.
    r := aml.regla('R03_CLIENTE_RIESGO_ALTO', v_tier);
    IF r.id IS NOT NULL THEN
        v_aplicadas := v_aplicadas || r.id;
        IF p_monto >= r.umbral_monto AND aml.marca_falso_positivo(p_cliente, r.codigo, p_sentido) < v_dia THEN
            v_factores := v_factores || jsonb_build_array(aml.factor(r, 'PRIMARIA', jsonb_build_object(
                'monto', p_monto::text, 'umbral', r.umbral_monto::text)));
            v_primarias := v_primarias || r.codigo;
            v_puntos := v_puntos + r.puntos;
            v_congelable := v_congelable OR r.permite_congelar;
        END IF;
    END IF;

    -- R04: fuera del perfil transaccional declarado (misma agrupación y supresión que R03).
    r := aml.regla('R04_FUERA_DE_PERFIL', v_tier);
    IF r.id IS NOT NULL THEN
        v_aplicadas := v_aplicadas || r.id;
        IF v_ingreso > 0 AND p_monto > r.multiplo_ingreso * v_ingreso
           AND aml.marca_falso_positivo(p_cliente, r.codigo, p_sentido) < v_dia THEN
            v_factores := v_factores || jsonb_build_array(aml.factor(r, 'PRIMARIA', jsonb_build_object(
                'monto', p_monto::text, 'ingreso_mensual_estimado', v_ingreso::text,
                'multiplo_ingreso', r.multiplo_ingreso::text)));
            v_primarias := v_primarias || r.codigo;
            v_puntos := v_puntos + r.puntos;
            v_congelable := v_congelable OR r.permite_congelar;
        END IF;
    END IF;

    -- R07: volumen de la ventana (todos los canales, ambos sentidos; cada
    -- movimiento con la marca de supresión de su propio sentido).
    r := aml.regla('R07_VOLUMEN_DIARIO', v_tier);
    IF r.id IS NOT NULL THEN
        v_aplicadas := v_aplicadas || r.id;
        v_desde := p_ahora - r.ventana_horas * interval '1 hour';
        v_wm_e := aml.marca_falso_positivo(p_cliente, r.codigo, 'ENTRADA');
        v_wm_s := aml.marca_falso_positivo(p_cliente, r.codigo, 'SALIDA');
        SELECT coalesce(sum(m.monto), 0) INTO v_suma FROM unnest(v_w) m
         WHERE m.creado_en > v_desde AND m.creado_en > CASE m.sentido WHEN 'ENTRADA' THEN v_wm_e ELSE v_wm_s END;
        IF v_suma + p_monto > r.volumen_max THEN
            v_factores := v_factores || jsonb_build_array(aml.factor(r, 'PRIMARIA', jsonb_build_object(
                'volumen', (v_suma + p_monto)::text, 'volumen_max', r.volumen_max::text,
                'desde', greatest(v_desde, CASE p_sentido WHEN 'ENTRADA' THEN v_wm_e ELSE v_wm_s END))));
            v_primarias := v_primarias || r.codigo;
            v_puntos := v_puntos + r.puntos;
            v_congelable := v_congelable OR r.permite_congelar;
        END IF;
    END IF;

    -- R08: frecuencia de la ventana (misma marca por sentido que R07).
    r := aml.regla('R08_FRECUENCIA', v_tier);
    IF r.id IS NOT NULL THEN
        v_aplicadas := v_aplicadas || r.id;
        v_desde := p_ahora - r.ventana_horas * interval '1 hour';
        v_wm_e := aml.marca_falso_positivo(p_cliente, r.codigo, 'ENTRADA');
        v_wm_s := aml.marca_falso_positivo(p_cliente, r.codigo, 'SALIDA');
        SELECT count(*) INTO v_n FROM unnest(v_w) m
         WHERE m.creado_en > v_desde AND m.creado_en > CASE m.sentido WHEN 'ENTRADA' THEN v_wm_e ELSE v_wm_s END;
        IF v_n + 1 > r.operaciones_max THEN
            v_factores := v_factores || jsonb_build_array(aml.factor(r, 'PRIMARIA', jsonb_build_object(
                'operaciones', v_n + 1, 'operaciones_max', r.operaciones_max,
                'desde', greatest(v_desde, CASE p_sentido WHEN 'ENTRADA' THEN v_wm_e ELSE v_wm_s END))));
            v_primarias := v_primarias || r.codigo;
            v_puntos := v_puntos + r.puntos;
            v_congelable := v_congelable OR r.permite_congelar;
        END IF;
    END IF;

    -- R11: paso rápido concentrado. E = lo que abonó la bóveda en la ventana
    -- (SPEI y transferencias recibidas; el cobro de un crédito no). O = las
    -- salidas por SPEI o transferencia (incluida esta) hacia las contrapartes
    -- que se llevan cada una al menos banda_inferior × E: pagos dispersos de
    -- poco monto (una nómina) no son paso rápido.
    r := aml.regla('R11_PASO_RAPIDO', v_tier);
    IF r.id IS NOT NULL THEN
        v_aplicadas := v_aplicadas || r.id;
        IF p_sentido = 'SALIDA' AND aml.canal_de_salida(p_canal) THEN
            v_desde := greatest(p_ahora - r.ventana_horas * interval '1 hour',
                                aml.marca_falso_positivo(p_cliente, r.codigo, 'SALIDA'));
            SELECT coalesce(sum(m.monto) FILTER (WHERE m.sentido = 'ENTRADA' AND aml.canal_abona_boveda(m.canal)), 0)
              INTO v_e
              FROM unnest(v_w) m WHERE m.creado_en > v_desde;
            WITH s AS (
                SELECT coalesce(m.contraparte, 'movimiento:' || m.transaccion_id) AS c, m.monto
                  FROM unnest(v_w) m
                 WHERE m.sentido = 'SALIDA' AND aml.canal_de_salida(m.canal) AND m.creado_en > v_desde
                UNION ALL
                SELECT coalesce(p_contraparte, 'movimiento:actual'), p_monto),
            g AS (SELECT s.c, sum(s.monto) AS total FROM s GROUP BY s.c)
            SELECT coalesce(sum(g.total), 0), coalesce(sum(g.total) FILTER (WHERE g.total >= r.banda_inferior * v_e), 0),
                   count(*) FILTER (WHERE g.total >= r.banda_inferior * v_e)
              INTO v_o, v_oc, v_nc
              FROM g;
            IF v_e >= r.umbral_monto AND v_oc >= r.porcentaje_min * v_e THEN
                v_factores := v_factores || jsonb_build_array(aml.factor(r, 'PRIMARIA', jsonb_build_object(
                    'entradas', v_e::text, 'salidas', v_o::text, 'salidas_concentradas', v_oc::text,
                    'contrapartes', v_nc, 'porcentaje_min', r.porcentaje_min::text,
                    'fraccion_min', r.banda_inferior::text, 'desde', v_desde)));
                v_primarias := v_primarias || r.codigo;
                v_puntos := v_puntos + r.puntos;
                v_congelable := v_congelable OR r.permite_congelar;
            END IF;
        END IF;
    END IF;

    -- Modificadores del perfil.
    IF cardinality(v_primarias) > 0 THEN
        SELECT * INTO v_mod FROM aml.modificadores(v_perfil);
        v_factores := v_factores || v_mod.factores;
        v_puntos := v_puntos + v_mod.puntos;
        v_aplicadas := v_aplicadas || v_mod.reglas;
    END IF;

    v_bruto := v_puntos;
    v_puntaje := LEAST(100, v_bruto);
    -- Regla dominante: la primaria con más puntos (empates por código).
    SELECT f INTO v_dominante FROM jsonb_array_elements(v_factores) f
     WHERE f ->> 'tipo' = 'PRIMARIA' ORDER BY (f ->> 'puntos')::numeric DESC, f ->> 'codigo' LIMIT 1;
    -- Fondos del patrón de una entrada: los abonos de la estructuración que
    -- disparó (R05 o R12; si ambas, el patrón de mayor suma). Son las únicas
    -- primarias de entrada que congelan.
    IF p_sentido = 'ENTRADA' AND (v_p05_suma > 0 OR v_p12_suma > 0) THEN
        IF v_p12_suma > v_p05_suma THEN
            v_b_ids := v_p12_ids;
            v_b_suma := v_p12_suma;
        ELSE
            v_b_ids := v_p05_ids;
            v_b_suma := v_p05_suma;
        END IF;
    END IF;

    SELECT jsonb_build_object(
               'desde', p_ahora - interval '24 hours', 'hasta', p_ahora, 'horas', 24,
               'entradas_n', count(*) FILTER (WHERE m.sentido = 'ENTRADA'),
               'entradas_monto', coalesce(sum(m.monto) FILTER (WHERE m.sentido = 'ENTRADA'), 0)::text,
               'salidas_n', count(*) FILTER (WHERE m.sentido = 'SALIDA'),
               'salidas_monto', coalesce(sum(m.monto) FILTER (WHERE m.sentido = 'SALIDA'), 0)::text,
               'operaciones_n', count(*), 'volumen', coalesce(sum(m.monto), 0)::text)
      INTO v_ventana
      FROM unnest(v_w) m;

    RETURN jsonb_build_object(
        'version', 1, 'parametros_id', par.id, 'ahora', p_ahora, 'nivel_cliente', v_tier,
        'perfil', v_perfil, 'ventana', v_ventana,
        'factores', v_factores, 'primarias', to_jsonb(v_primarias), 'reglas_aplicadas', to_jsonb(v_aplicadas),
        'puntaje_bruto', round(v_bruto, 2)::text, 'puntaje', round(v_puntaje, 2)::text,
        'nivel', aml.nivel_de_puntaje(v_puntaje),
        'dominante', v_dominante -> 'codigo', 'dominante_regla_id', v_dominante -> 'regla_id',
        'congelar', v_puntaje >= par.umbral_critico AND v_congelable,
        'relevante', v_relevante, 'regla_relevante_id', v_regla_rel,
        'transacciones_patron', v_b_ids, 'monto_patron', (v_b_suma + p_monto)::text);
END
$$;

-- 6.3 Alertas: alta o agrupación con la alerta abierta del mismo patrón.
CREATE FUNCTION aml.registrar_alerta(p_cliente UUID, p_entidad TEXT, p_sentido TEXT, p_transaccion UUID, p_evaluacion UUID,
                                     p_regla_id UUID, p_origen TEXT, p_decision TEXT, p_puntaje NUMERIC, p_monto NUMERIC,
                                     p_detalles JSONB, p_urgente BOOLEAN, p_clave_dedup TEXT, p_agrupar BOOLEAN)
RETURNS UUID
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    par      aml.parametros := aml.parametros_vigentes();
    r        aml.reglas_riesgo;
    v_clave  TEXT;
    v_tipo   TEXT;
    v_limite TIMESTAMPTZ;
    v_id     UUID;
BEGIN
    SELECT * INTO r FROM aml.reglas_riesgo WHERE id = p_regla_id;
    IF r.id IS NULL THEN
        RAISE EXCEPTION 'SC_AML_PARAMETRO_INVALIDO: la regla % no existe', p_regla_id;
    END IF;
    -- Las reglas de patrón agrupan por cliente y sentido; las de perfil (R03,
    -- R04), por cliente, sentido y día de la Ciudad de México (un cliente ALTO
    -- no recibe una alerta por cada movimiento); las demás, por movimiento.
    v_clave := coalesce(p_clave_dedup,
        CASE WHEN r.codigo IN ('R02_FRACCIONAMIENTO', 'R05_ESTRUCTURACION', 'R07_VOLUMEN_DIARIO', 'R08_FRECUENCIA',
                               'R11_PASO_RAPIDO', 'R12_ESTRUCTURACION_FRECUENTE')
             THEN r.codigo || ':' || p_cliente || ':' || p_sentido
             WHEN r.codigo IN ('R03_CLIENTE_RIESGO_ALTO', 'R04_FUERA_DE_PERFIL')
             THEN r.codigo || ':' || p_cliente || ':' || p_sentido || ':'
                  || to_char(clock_timestamp() AT TIME ZONE 'America/Mexico_City', 'YYYY-MM-DD')
             ELSE r.codigo || ':' || p_entidad || ':' || coalesce(p_transaccion, p_evaluacion, gen_random_uuid()) || ':'
                  || p_sentido
        END);
    -- Personal del banco: reporte de operación interna preocupante.
    v_tipo := CASE WHEN aml.es_interno(p_cliente) THEN 'INTERNA_PREOCUPANTE' ELSE r.tipo_reporte END;
    v_limite := clock_timestamp() + CASE WHEN p_urgente THEN par.horas_reporte_urgente * interval '1 hour'
                                         ELSE par.dias_reporte_inusual * interval '1 day' END;

    IF p_agrupar THEN
        -- Evidencia nueva sobre una alerta con una propuesta de falso positivo
        -- pendiente: la propuesta se retira (se dictaminó sin esta evidencia;
        -- el segundo oficial no la aprueba a ciegas). Queda en la bitácora
        -- como ALERTA_FP_RETIRADO con este motivo.
        PERFORM aml.fijar_motivo('Evidencia nueva agrupada en la alerta después de la propuesta de falso positivo');
        INSERT INTO aml.alertas AS a (cliente_id, transaccion_id, entidad, sentido, evaluacion_id, regla_id, regla_codigo,
                                      tipo_reporte, origen, decision, nivel_riesgo, puntaje, detalles, monto,
                                      monto_acumulado, clave_deduplicacion, urgente, fecha_limite_reporte)
        VALUES (p_cliente, p_transaccion, p_entidad, p_sentido, p_evaluacion, r.id, r.codigo, v_tipo, p_origen, p_decision,
                aml.nivel_de_puntaje(p_puntaje), p_puntaje, p_detalles, p_monto,
                coalesce((p_detalles -> 'evaluacion' ->> 'monto_patron')::numeric, p_monto), v_clave, p_urgente, v_limite)
        ON CONFLICT (clave_deduplicacion) WHERE estado IN ('PENDIENTE', 'EN_REVISION') DO UPDATE SET
            ocurrencias          = a.ocurrencias + 1,
            ultima_ocurrencia_en = clock_timestamp(),
            puntaje              = GREATEST(a.puntaje, EXCLUDED.puntaje),
            nivel_riesgo         = GREATEST(a.nivel_riesgo, EXCLUDED.nivel_riesgo),
            decision             = CASE WHEN aml.rango_decision(EXCLUDED.decision) > aml.rango_decision(a.decision)
                                        THEN EXCLUDED.decision ELSE a.decision END,
            monto_acumulado      = GREATEST(a.monto_acumulado, EXCLUDED.monto_acumulado),
            urgente              = a.urgente OR EXCLUDED.urgente,
            fecha_limite_reporte = LEAST(a.fecha_limite_reporte, EXCLUDED.fecha_limite_reporte),
            detalles             = aml.fusionar_detalles(a.detalles, EXCLUDED.detalles),
            resolucion_propuesta = NULL,
            propuesta_por        = NULL,
            dictamen_propuesto   = NULL,
            propuesta_en         = NULL,
            solicitud_propuesta_id = NULL
        RETURNING a.id INTO v_id;
        PERFORM aml.fijar_motivo(NULL);
    ELSE
        INSERT INTO aml.alertas (cliente_id, transaccion_id, entidad, sentido, evaluacion_id, regla_id, regla_codigo,
                                 tipo_reporte, origen, decision, nivel_riesgo, puntaje, detalles, monto, monto_acumulado,
                                 clave_deduplicacion, urgente, fecha_limite_reporte)
        VALUES (p_cliente, p_transaccion, p_entidad, p_sentido, p_evaluacion, r.id, r.codigo, v_tipo, p_origen, p_decision,
                aml.nivel_de_puntaje(p_puntaje), p_puntaje, p_detalles, p_monto,
                coalesce((p_detalles -> 'evaluacion' ->> 'monto_patron')::numeric, p_monto), v_clave, p_urgente, v_limite)
        ON CONFLICT (clave_deduplicacion) WHERE estado IN ('PENDIENTE', 'EN_REVISION') DO NOTHING
        RETURNING id INTO v_id;
        IF v_id IS NULL THEN
            SELECT id INTO v_id FROM aml.alertas
             WHERE clave_deduplicacion = v_clave AND estado IN ('PENDIENTE', 'EN_REVISION');
        END IF;
    END IF;

    IF p_evaluacion IS NOT NULL THEN
        INSERT INTO aml.alerta_evaluaciones (alerta_id, evaluacion_id) VALUES (v_id, p_evaluacion) ON CONFLICT DO NOTHING;
    END IF;
    RETURN v_id;
END
$$;

-- 6.4 Congelamiento preventivo. Quien congela siempre toma primero el candado
-- del cliente (fila IFPE): es linealizable con los débitos.
CREATE FUNCTION aml.congelar(p_cliente UUID, p_origen TEXT, p_alerta UUID, p_evaluacion UUID, p_puntaje NUMERIC,
                             p_motivo TEXT, p_actor UUID) RETURNS UUID
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    par      aml.parametros := aml.parametros_vigentes();
    v_cuenta UUID;
    v_id     UUID;
    v_folio  BIGINT;
BEGIN
    v_cuenta := aml.bloquear_cliente(p_cliente); -- ya tomado en los caminos del dinero: no-op
    SELECT id INTO v_id FROM aml.congelamientos WHERE cliente_id = p_cliente AND estado = 'ACTIVO';
    IF v_id IS NOT NULL THEN
        UPDATE aml.alertas SET congelamiento_id = v_id WHERE id = p_alerta AND congelamiento_id IS NULL;
        RETURN v_id;
    END IF;
    INSERT INTO aml.congelamientos (cliente_id, cuenta_ifpe_id, origen, alerta_id, evaluacion_id, puntaje, motivo,
                                    congelado_por, sla_vence_en)
    VALUES (p_cliente, v_cuenta, p_origen, p_alerta, p_evaluacion, p_puntaje, p_motivo, p_actor,
            clock_timestamp() + par.sla_congelamiento_horas * interval '1 hour')
    RETURNING id, folio INTO v_id, v_folio;
    UPDATE ifpe.cuentas SET estado = 'CONGELADO_PREVENTIVO' WHERE id = v_cuenta AND estado = 'ACTIVA';
    UPDATE aml.alertas SET congelamiento_id = v_id WHERE id = p_alerta AND congelamiento_id IS NULL;
    PERFORM aml.notificar('CONGELAMIENTO', 'ALTA', p_cliente, p_alerta, v_id, NULL, NULL,
        format('%s · congelamiento %s%s', p_origen, v_folio,
               coalesce(' · alerta ' || (SELECT folio FROM aml.alertas WHERE id = p_alerta), '')));
    RETURN v_id;
END
$$;

-- 6.5 Retención de un abono (RETENCION: disponible -> bloqueado en la misma
-- cuenta; la reserva y las sombras no cambian). Cubre los fondos del patrón.
CREATE FUNCTION aml.aplicar_retencion(p_evaluacion UUID) RETURNS UUID
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    e        aml.evaluaciones;
    e_xmin   xid;
    t        ifpe.transacciones;
    t_xmin   xid;
    v_cuenta UUID;
    v_cong   UUID;
    v_monto  NUMERIC;
    v_ret    UUID := gen_random_uuid();
    v_tx     UUID := gen_random_uuid();
BEGIN
    PERFORM aml.fijar_actor(NULL);
    SELECT * INTO e FROM aml.evaluaciones WHERE id = p_evaluacion;
    SELECT x.xmin INTO e_xmin FROM aml.evaluaciones x WHERE x.id = p_evaluacion;
    SELECT * INTO t FROM ifpe.transacciones WHERE id = e.transaccion_id;
    SELECT x.xmin INTO t_xmin FROM ifpe.transacciones x WHERE x.id = e.transaccion_id;
    -- La evaluación es de esta misma transacción (una RETENIDA confirmada
    -- antes no se "reproduce" para retener otra vez ni fuera de su abono).
    IF e.id IS NULL OR e.origen <> 'EN_LINEA' OR e.resultado <> 'RETENIDA' OR e.entidad <> 'IFPE' OR e.sentido <> 'ENTRADA'
       OR t.id IS NULL OR t_xmin IS DISTINCT FROM e_xmin OR e_xmin IS DISTINCT FROM pg_current_xact_id()::xid THEN
        RAISE EXCEPTION 'SC_AML_EVALUACION_INCOHERENTE: la evaluación % no corresponde a un abono retenido de esta transacción',
            p_evaluacion;
    END IF;
    v_cuenta := t.cuenta_destino_id;
    IF NOT EXISTS (SELECT 1 FROM ifpe.cuentas WHERE id = v_cuenta AND cliente_id = e.cliente_id) THEN
        RAISE EXCEPTION 'SC_AML_EVALUACION_INCOHERENTE: el abono % no es del cliente evaluado', t.id;
    END IF;
    SELECT id INTO v_cong FROM aml.congelamientos WHERE cliente_id = e.cliente_id AND estado = 'ACTIVO';
    IF v_cong IS NULL THEN
        RAISE EXCEPTION 'SC_AML_EVALUACION_INCOHERENTE: el cliente % no tiene un congelamiento activo', e.cliente_id;
    END IF;
    SELECT LEAST(saldo_disponible, e.monto_retener) INTO v_monto FROM ifpe.cuentas WHERE id = v_cuenta FOR NO KEY UPDATE;
    IF v_monto IS NULL OR v_monto <= 0 THEN
        RAISE EXCEPTION 'SC_AML_EVALUACION_INCOHERENTE: no hay saldo que retener en la cuenta %', v_cuenta;
    END IF;
    UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - v_monto, saldo_bloqueado = saldo_bloqueado + v_monto
     WHERE id = v_cuenta;
    INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, cuenta_destino_id, monto, concepto, clave_idempotencia)
    VALUES (v_tx, 'RETENCION', v_cuenta, v_cuenta, v_monto, 'Retención temporal', 'ret:' || v_ret);
    INSERT INTO aml.retenciones (id, congelamiento_id, cliente_id, cuenta_id, evaluacion_id, transaccion_origen_id,
                                 tx_retencion_id, monto)
    VALUES (v_ret, v_cong, e.cliente_id, v_cuenta, e.id, t.id, v_tx, v_monto);
    RETURN v_tx;
END
$$;

-- Liberación de una retención: solo con una solicitud aprobada (cuatro ojos)
-- de la misma transacción (lo exige aml.fn_proteger_retencion).
CREATE FUNCTION aml.liberar_retencion(p_retencion UUID, p_solicitud UUID) RETURNS UUID
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    r    aml.retenciones;
    v_tx UUID := gen_random_uuid();
BEGIN
    SELECT * INTO r FROM aml.retenciones WHERE id = p_retencion FOR UPDATE;
    IF r.id IS NULL OR r.estado <> 'VIGENTE' THEN
        RAISE EXCEPTION 'SC_AML_SIN_RETENCIONES: la retención % no está vigente', p_retencion;
    END IF;
    UPDATE ifpe.cuentas SET saldo_bloqueado = saldo_bloqueado - r.monto, saldo_disponible = saldo_disponible + r.monto
     WHERE id = r.cuenta_id;
    INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, cuenta_destino_id, monto, concepto, clave_idempotencia)
    VALUES (v_tx, 'LIBERACION_RETENCION', r.cuenta_id, r.cuenta_id, r.monto, 'Liberación de retención', 'lib:' || r.id);
    UPDATE aml.retenciones SET estado = 'LIBERADA', solicitud_id = p_solicitud, tx_liberacion_id = v_tx,
                               liberada_en = clock_timestamp()
     WHERE id = r.id;
    RETURN v_tx;
END
$$;

-- 6.6 Evaluación de una operación (la invocan las envolturas ifpe.aml_evaluar
-- y sofom.aml_evaluar dentro de la transacción del dinero, antes de escribir
-- en el ledger y antes del candado de la reserva). BLOQUEAR implica que, al
-- regresar, el cliente tiene un congelamiento ACTIVO (la guardia del ledger
-- respalda al backend). Una ENTRADA nunca regresa BLOQUEAR; la SOFOM nunca
-- regresa RETENER.
CREATE FUNCTION aml.evaluar_operacion(p_entidad TEXT, p_cliente UUID, p_sentido TEXT, p_canal TEXT, p_monto NUMERIC,
                                      p_transaccion_id UUID, p_clave TEXT, p_contraparte JSONB)
RETURNS TABLE (evaluacion_id UUID, decision TEXT, puntaje NUMERIC, nivel TEXT, monto_retener NUMERIC)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    par         aml.parametros;
    v_congelado BOOLEAN;
    v           JSONB;
    v_error     TEXT;
    v_eval      UUID;
    v_resultado TEXT;
    v_motivo    TEXT;
    v_crit      BOOLEAN;
    v_alertar   BOOLEAN;
    v_puntaje   NUMERIC;
    v_nivel     aml.nivel_riesgo;
    v_retener   NUMERIC := 0;
    v_alerta    UUID;
    v_contra    JSONB := coalesce(p_contraparte, '{}');
    v_clave_c   TEXT;
BEGIN
    IF current_setting('transaction_isolation') <> 'read committed' THEN
        RAISE EXCEPTION 'SC_AML_AISLAMIENTO: la evaluación exige READ COMMITTED';
    END IF;
    IF ((p_entidad = 'IFPE' AND p_sentido = 'ENTRADA'
         AND p_canal IN ('SPEI_WEBHOOK', 'SPEI_OPERADOR', 'SPEI_DEMO', 'TRANSFERENCIA_RECIBIDA'))
        OR (p_entidad = 'IFPE' AND p_sentido = 'SALIDA'
            AND p_canal IN ('TRANSFERENCIA_ENVIADA', 'SPEI_ENVIADO', 'INVERSION_MUTUO'))
        OR (p_entidad = 'SOFOM' AND p_sentido = 'SALIDA' AND p_canal = 'DESEMBOLSO_CREDITO')
        OR (p_entidad = 'SOFOM' AND p_sentido = 'ENTRADA' AND p_canal = 'COBRO_CREDITO'))
       IS NOT TRUE
       OR p_monto IS NULL OR p_monto <= 0 OR p_transaccion_id IS NULL OR p_clave IS NULL OR p_cliente IS NULL
       OR jsonb_typeof(v_contra) <> 'object' THEN
        RAISE EXCEPTION 'SC_AML_PARAMETRO_INVALIDO: combinación inválida (%, %, %) o datos de la operación incompletos',
            p_entidad, p_sentido, p_canal;
    END IF;

    PERFORM aml.fijar_actor(NULL);
    PERFORM aml.bloquear_cliente(p_cliente);
    par := aml.parametros_vigentes();
    v_congelado := aml.cliente_congelado(p_cliente);
    v_clave_c := aml.clave_contraparte(p_canal, v_contra);

    -- Cliente congelado: toda salida se bloquea (un reintento con la misma
    -- clave no cuenta un intento nuevo).
    IF p_sentido = 'SALIDA' AND v_congelado THEN
        INSERT INTO aml.evaluaciones (cliente_id, entidad, sentido, canal, monto, transaccion_id, clave_operacion,
                                      contraparte, resultado, motivo_bloqueo, origen, parametros_id)
        VALUES (p_cliente, p_entidad, p_sentido, p_canal, p_monto, NULL, p_clave, v_contra, 'BLOQUEADA', 'CONGELADO',
                'EN_LINEA', par.id)
        ON CONFLICT (cliente_id, clave_operacion) WHERE resultado = 'BLOQUEADA' DO NOTHING
        RETURNING id INTO v_eval;
        IF v_eval IS NOT NULL THEN
            UPDATE aml.congelamientos SET intentos_bloqueados = intentos_bloqueados + 1, ultimo_intento_en = clock_timestamp()
             WHERE cliente_id = p_cliente AND estado = 'ACTIVO';
            PERFORM aml.registrar_bitacora('OPERACION_BLOQUEADA', 'OPERACION', v_eval::text, p_cliente, NULL,
                (SELECT id FROM aml.congelamientos WHERE cliente_id = p_cliente AND estado = 'ACTIVO'), NULL, NULL, NULL,
                NULL, NULL, jsonb_build_object('canal', p_canal, 'monto', p_monto::text, 'motivo', 'CONGELADO'));
        ELSE
            SELECT id INTO v_eval FROM aml.evaluaciones
             WHERE cliente_id = p_cliente AND clave_operacion = p_clave AND resultado = 'BLOQUEADA';
        END IF;
        RETURN QUERY SELECT v_eval, 'BLOQUEAR'::text, NULL::numeric, NULL::text, 0::numeric;
        RETURN;
    END IF;

    -- Calificación. Una ENTRADA nunca se rechaza: si calificar falla se abona
    -- y se reevalúa después (único bloque EXCEPTION del módulo, de solo
    -- lectura; los errores transitorios se propagan para reintentar todo).
    -- Una SALIDA falla cerrada.
    IF p_sentido = 'ENTRADA' THEN
        BEGIN
            v := aml.calcular(p_cliente, p_entidad, p_sentido, p_canal, p_monto, NULL, clock_timestamp(), v_clave_c);
        EXCEPTION WHEN OTHERS THEN
            IF SQLSTATE IN ('40001', '40P01', '55P03', '57014') THEN
                RAISE;
            END IF;
            v := NULL;
            v_error := SQLERRM;
        END;
    ELSE
        v := aml.calcular(p_cliente, p_entidad, p_sentido, p_canal, p_monto, NULL, clock_timestamp(), v_clave_c);
    END IF;

    IF v IS NULL THEN
        IF v_congelado AND p_entidad = 'IFPE' THEN
            v_resultado := 'RETENIDA';
            v_retener := p_monto;
        ELSE
            v_resultado := 'PENDIENTE_REEVALUACION';
        END IF;
        INSERT INTO aml.evaluaciones (cliente_id, entidad, sentido, canal, monto, transaccion_id, clave_operacion,
                                      contraparte, resultado, origen, factores, parametros_id, monto_retener)
        VALUES (p_cliente, p_entidad, p_sentido, p_canal, p_monto, p_transaccion_id, p_clave, v_contra, v_resultado,
                'EN_LINEA', jsonb_build_object('error', v_error), par.id, v_retener)
        RETURNING id INTO v_eval;
        PERFORM aml.registrar_bitacora('EVALUACION_FALLIDA', 'EVALUACION', v_eval::text, p_cliente, NULL, NULL, NULL,
            p_transaccion_id, NULL, v_resultado, v_error, jsonb_build_object('canal', p_canal, 'monto', p_monto::text));
        PERFORM aml.notificar('EVALUACION_FALLIDA', 'ALTA', p_cliente, NULL, NULL, NULL, NULL,
            format('%s · %s · evaluación fallida', p_canal, v_resultado));
        RETURN QUERY SELECT v_eval, CASE WHEN v_resultado = 'RETENIDA' THEN 'RETENER' ELSE 'PERMITIR' END,
                            NULL::numeric, NULL::text, v_retener;
        RETURN;
    END IF;

    v_crit := (v ->> 'congelar')::boolean AND NOT par.modo_observacion;
    v_alertar := jsonb_array_length(v -> 'primarias') > 0;
    v_puntaje := (v ->> 'puntaje')::numeric;
    v_nivel := (v ->> 'nivel')::aml.nivel_riesgo;
    IF p_sentido = 'SALIDA' AND v_crit THEN
        v_resultado := 'BLOQUEADA';
        v_motivo := 'PUNTAJE_CRITICO';
    ELSIF p_sentido = 'ENTRADA' AND p_entidad = 'IFPE' AND (v_crit OR v_congelado) THEN
        v_resultado := 'RETENIDA';
        v_retener := CASE WHEN v_crit AND NOT v_congelado THEN (v ->> 'monto_patron')::numeric ELSE p_monto END;
    ELSIF v_alertar THEN
        v_resultado := 'ALERTADA';
    ELSE
        v_resultado := 'PERMITIDA';
    END IF;

    IF v_resultado = 'BLOQUEADA' THEN
        INSERT INTO aml.evaluaciones (cliente_id, entidad, sentido, canal, monto, transaccion_id, clave_operacion,
                                      contraparte, puntaje, nivel, resultado, motivo_bloqueo, origen, factores,
                                      reglas_aplicadas, parametros_id)
        VALUES (p_cliente, p_entidad, p_sentido, p_canal, p_monto, NULL, p_clave, v_contra, v_puntaje, v_nivel,
                'BLOQUEADA', v_motivo, 'EN_LINEA', v,
                ARRAY(SELECT jsonb_array_elements_text(v -> 'reglas_aplicadas')::uuid), par.id)
        ON CONFLICT (cliente_id, clave_operacion) WHERE resultado = 'BLOQUEADA' DO NOTHING
        RETURNING id INTO v_eval;
        IF v_eval IS NULL THEN
            SELECT id INTO v_eval FROM aml.evaluaciones
             WHERE cliente_id = p_cliente AND clave_operacion = p_clave AND resultado = 'BLOQUEADA';
        END IF;
    ELSE
        INSERT INTO aml.evaluaciones (cliente_id, entidad, sentido, canal, monto, transaccion_id, clave_operacion,
                                      contraparte, puntaje, nivel, resultado, origen, factores, reglas_aplicadas,
                                      parametros_id, monto_retener)
        VALUES (p_cliente, p_entidad, p_sentido, p_canal, p_monto, p_transaccion_id, p_clave, v_contra, v_puntaje, v_nivel,
                v_resultado, 'EN_LINEA', v, ARRAY(SELECT jsonb_array_elements_text(v -> 'reglas_aplicadas')::uuid),
                par.id, v_retener)
        RETURNING id INTO v_eval;
    END IF;

    IF v_alertar THEN
        v_alerta := aml.registrar_alerta(
            p_cliente, p_entidad, p_sentido,
            CASE WHEN v_resultado = 'BLOQUEADA' THEN NULL ELSE p_transaccion_id END,
            v_eval, (v ->> 'dominante_regla_id')::uuid,
            CASE WHEN v_resultado = 'BLOQUEADA' THEN 'INTENTO_BLOQUEADO' ELSE 'EN_LINEA' END,
            CASE v_resultado WHEN 'BLOQUEADA' THEN 'BLOQUEADA' WHEN 'RETENIDA' THEN 'RETENIDA' ELSE 'REGISTRADA' END,
            v_puntaje, p_monto,
            jsonb_build_object(
                'operacion', jsonb_build_object(
                    'entidad', p_entidad, 'sentido', p_sentido, 'canal', p_canal, 'monto', p_monto::text,
                    'transaccion_id', CASE WHEN v_resultado = 'BLOQUEADA' THEN NULL ELSE p_transaccion_id END,
                    'contraparte', v_contra),
                'evaluacion', v,
                'transacciones', CASE WHEN jsonb_array_length(v -> 'transacciones_patron') > 0
                                      THEN (v -> 'transacciones_patron') || to_jsonb(p_transaccion_id::text)
                                      ELSE jsonb_build_array(p_transaccion_id::text) END),
            FALSE, NULL, TRUE);
    END IF;

    IF v_crit AND NOT v_congelado THEN
        PERFORM aml.congelar(p_cliente, 'AUTOMATICO', v_alerta, v_eval, v_puntaje,
            format('Congelamiento automático: %s, puntaje %s', v ->> 'dominante', v_puntaje), NULL);
    END IF;

    IF (v ->> 'relevante')::boolean AND v_resultado <> 'BLOQUEADA' THEN
        INSERT INTO aml.operaciones_relevantes (cliente_id, entidad, transaccion_id, sentido, canal, monto, evaluacion_id,
                                                trimestre)
        VALUES (p_cliente, p_entidad, p_transaccion_id, p_sentido, p_canal, p_monto, v_eval,
                aml.trimestre(clock_timestamp()))
        ON CONFLICT DO NOTHING;
    END IF;

    RETURN QUERY SELECT v_eval,
                        CASE v_resultado WHEN 'BLOQUEADA' THEN 'BLOQUEAR' WHEN 'RETENIDA' THEN 'RETENER' ELSE 'PERMITIR' END,
                        v_puntaje, v_nivel::text, v_retener;
END
$$;

-- -----------------------------------------------------------------------------
-- 7. Protección, bitácora y notificaciones de las tablas de `aml`
-- -----------------------------------------------------------------------------
-- 7.1 Solo inserción.
CREATE TRIGGER trg_aml_evaluaciones_inmutable BEFORE UPDATE OR DELETE ON aml.evaluaciones
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_aml_evaluaciones_no_truncate BEFORE TRUNCATE ON aml.evaluaciones
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_aml_alerta_evaluaciones_inmutable BEFORE UPDATE OR DELETE ON aml.alerta_evaluaciones
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_aml_alerta_evaluaciones_no_truncate BEFORE TRUNCATE ON aml.alerta_evaluaciones
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_aml_acciones_inmutable BEFORE UPDATE OR DELETE ON aml.acciones
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_aml_acciones_no_truncate BEFORE TRUNCATE ON aml.acciones
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_aml_sellos_inmutable BEFORE UPDATE OR DELETE ON aml.sellos_bitacora
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_aml_sellos_no_truncate BEFORE TRUNCATE ON aml.sellos_bitacora
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_aml_verificaciones_inmutable BEFORE UPDATE OR DELETE ON aml.verificaciones_bitacora
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_aml_verificaciones_no_truncate BEFORE TRUNCATE ON aml.verificaciones_bitacora
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- 7.2 Bitácora: hash canónico al insertar; nunca UPDATE, DELETE ni TRUNCATE.
-- No toma candados (no hay cabeza de cadena en el camino del dinero: el
-- encadenamiento lo hace el sellado por lotes).
CREATE FUNCTION aml.fn_bitacora_hash() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    NEW.hash_contenido := aml.hash_bitacora(NEW);
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_aml_bitacora_hash BEFORE INSERT ON aml.bitacora
    FOR EACH ROW EXECUTE FUNCTION aml.fn_bitacora_hash();

CREATE FUNCTION aml.fn_bitacora_inmutable() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    RAISE EXCEPTION 'SC_AML_BITACORA_INMUTABLE: la bitácora de cumplimiento solo admite INSERT'
        USING ERRCODE = 'restrict_violation';
END
$$;
CREATE TRIGGER trg_aml_bitacora_inmutable BEFORE UPDATE OR DELETE ON aml.bitacora
    FOR EACH ROW EXECUTE FUNCTION aml.fn_bitacora_inmutable();
CREATE TRIGGER trg_aml_bitacora_no_truncate BEFORE TRUNCATE ON aml.bitacora
    FOR EACH STATEMENT EXECUTE FUNCTION aml.fn_bitacora_inmutable();

-- 7.3 Parámetros: versiones de solo inserción; una versión nueva exige una
-- propuesta aprobada en esta misma transacción.
CREATE FUNCTION aml.fn_proteger_parametros() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF (NEW.id > 1 OR EXISTS (SELECT 1 FROM aml.parametros))
           AND NOT EXISTS (SELECT 1 FROM aml.propuestas_cambio p
                            WHERE p.id = NEW.propuesta_id AND p.tipo = 'PARAMETROS' AND p.estado = 'APROBADA'
                              AND p.xmin = pg_current_xact_id()::xid) THEN
            RAISE EXCEPTION 'SC_AML_REGLA_SIN_APROBACION: una versión nueva de los parámetros exige una propuesta aprobada en esta transacción';
        END IF;
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'SC_AML_PARAMETROS_INMUTABLE: los parámetros PLD son versiones de solo inserción'
        USING ERRCODE = 'restrict_violation';
END
$$;
CREATE TRIGGER trg_aml_parametros_proteger BEFORE INSERT OR UPDATE OR DELETE ON aml.parametros
    FOR EACH ROW EXECUTE FUNCTION aml.fn_proteger_parametros();
CREATE TRIGGER trg_aml_parametros_no_truncate BEFORE TRUNCATE ON aml.parametros
    FOR EACH STATEMENT EXECUTE FUNCTION aml.fn_proteger_parametros();

-- 7.4 Matriz de riesgo: renglones inmutables. Solo se desactiva (TRUE -> FALSE
-- con su fecha) y una versión nueva exige una propuesta aprobada en esta
-- misma transacción; desactivar también la exige.
CREATE FUNCTION aml.fn_proteger_regla() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'SC_AML_REGLA_INMUTABLE: las reglas de riesgo no se borran (se versionan)'
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.version > 1 AND NOT EXISTS (
               SELECT 1 FROM aml.propuestas_cambio p
                WHERE p.id = NEW.propuesta_id AND p.tipo = 'REGLA' AND p.estado = 'APROBADA'
                  AND p.regla_codigo = NEW.codigo AND p.nivel_cliente = NEW.nivel_cliente
                  AND p.xmin = pg_current_xact_id()::xid) THEN
            RAISE EXCEPTION 'SC_AML_REGLA_SIN_APROBACION: la versión % de % exige una propuesta aprobada en esta transacción',
                NEW.version, NEW.codigo;
        END IF;
        RETURN NEW;
    END IF;
    IF (to_jsonb(NEW) - ARRAY['activa', 'desactivada_en']) IS DISTINCT FROM (to_jsonb(OLD) - ARRAY['activa', 'desactivada_en'])
       OR NOT (OLD.activa AND NOT NEW.activa AND OLD.desactivada_en IS NULL AND NEW.desactivada_en IS NOT NULL) THEN
        RAISE EXCEPTION 'SC_AML_REGLA_INMUTABLE: la regla % versión % no cambia (los cambios son versiones nuevas)',
            OLD.codigo, OLD.version
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM aml.propuestas_cambio p
                    WHERE p.tipo = 'REGLA' AND p.estado = 'APROBADA' AND p.regla_codigo = OLD.codigo
                      AND p.nivel_cliente = OLD.nivel_cliente AND p.xmin = pg_current_xact_id()::xid) THEN
        RAISE EXCEPTION 'SC_AML_REGLA_SIN_APROBACION: desactivar % exige una propuesta aprobada en esta transacción', OLD.codigo;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_aml_reglas_proteger BEFORE INSERT OR UPDATE OR DELETE ON aml.reglas_riesgo
    FOR EACH ROW EXECUTE FUNCTION aml.fn_proteger_regla();
CREATE TRIGGER trg_aml_reglas_no_truncate BEFORE TRUNCATE ON aml.reglas_riesgo
    FOR EACH STATEMENT EXECUTE FUNCTION aml.fn_proteger_regla();

-- 7.5 Propuestas: nacen PENDIENTE, de un oficial de cumplimiento (el actor
-- de aml.proponer_cambio), y se deciden una sola vez PENDIENTE ->
-- APROBADA|RECHAZADA por otro oficial (el actor de aml.decidir_propuesta).
-- Una propuesta insertada ya APROBADA no desbloquea nada.
CREATE FUNCTION aml.fn_proteger_propuesta() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.estado <> 'PENDIENTE' OR NEW.decidida_por IS NOT NULL OR NEW.decidida_en IS NOT NULL
           OR NEW.comentario IS NOT NULL OR NEW.resultado_id IS NOT NULL THEN
            RAISE EXCEPTION 'SC_AML_REGISTRO_INMUTABLE: una propuesta nace PENDIENTE, sin decisión'
                USING ERRCODE = 'restrict_violation';
        END IF;
        IF NEW.propuesta_por IS DISTINCT FROM aml.actor_actual()
           OR NOT EXISTS (SELECT 1 FROM kyc.credenciales WHERE cliente_id = NEW.propuesta_por AND rol = 'CUMPLIMIENTO') THEN
            RAISE EXCEPTION 'SC_AML_NO_AUTORIZADO: una propuesta la hace un oficial de cumplimiento (aml.proponer_cambio)';
        END IF;
        RETURN NEW;
    END IF;
    IF TG_OP IN ('DELETE', 'TRUNCATE')
       OR (to_jsonb(NEW) - ARRAY['estado', 'decidida_por', 'decidida_en', 'comentario', 'resultado_id'])
          IS DISTINCT FROM (to_jsonb(OLD) - ARRAY['estado', 'decidida_por', 'decidida_en', 'comentario', 'resultado_id'])
       OR NOT (OLD.estado = 'PENDIENTE' AND NEW.estado IN ('APROBADA', 'RECHAZADA')) THEN
        RAISE EXCEPTION 'SC_AML_REGISTRO_INMUTABLE: una propuesta solo se decide una vez'
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF NEW.decidida_por IS DISTINCT FROM aml.actor_actual()
       OR NOT EXISTS (SELECT 1 FROM kyc.credenciales WHERE cliente_id = NEW.decidida_por AND rol = 'CUMPLIMIENTO') THEN
        RAISE EXCEPTION 'SC_AML_NO_AUTORIZADO: una propuesta la decide un oficial de cumplimiento (aml.decidir_propuesta)';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_aml_propuestas_proteger BEFORE INSERT OR UPDATE OR DELETE ON aml.propuestas_cambio
    FOR EACH ROW EXECUTE FUNCTION aml.fn_proteger_propuesta();
CREATE TRIGGER trg_aml_propuestas_no_truncate BEFORE TRUNCATE ON aml.propuestas_cambio
    FOR EACH STATEMENT EXECUTE FUNCTION aml.fn_proteger_propuesta();

-- 7.6 Solicitudes: nacen PENDIENTE, de un oficial de cumplimiento (el actor
-- de la función que la crea), y tienen una sola transición PENDIENTE ->
-- APROBADA|RECHAZADA (por el oficial que decide, el actor de
-- aml.decidir_levantamiento) o ANULADA (el sistema, al confirmar una alerta).
CREATE FUNCTION aml.fn_proteger_solicitud() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.estado <> 'PENDIENTE' OR NEW.decidida_por IS NOT NULL OR NEW.decidida_en IS NOT NULL
           OR NEW.comentario IS NOT NULL THEN
            RAISE EXCEPTION 'SC_AML_REGISTRO_INMUTABLE: una solicitud de levantamiento nace PENDIENTE, sin decisión'
                USING ERRCODE = 'restrict_violation';
        END IF;
        IF NEW.solicitada_por IS DISTINCT FROM aml.actor_actual()
           OR NOT EXISTS (SELECT 1 FROM kyc.credenciales WHERE cliente_id = NEW.solicitada_por AND rol = 'CUMPLIMIENTO') THEN
            RAISE EXCEPTION 'SC_AML_NO_AUTORIZADO: una solicitud de levantamiento la hace un oficial de cumplimiento';
        END IF;
        RETURN NEW;
    END IF;
    IF TG_OP IN ('DELETE', 'TRUNCATE')
       OR (to_jsonb(NEW) - ARRAY['estado', 'decidida_por', 'decidida_en', 'comentario'])
          IS DISTINCT FROM (to_jsonb(OLD) - ARRAY['estado', 'decidida_por', 'decidida_en', 'comentario'])
       OR NOT (OLD.estado = 'PENDIENTE' AND NEW.estado IN ('APROBADA', 'RECHAZADA', 'ANULADA')) THEN
        RAISE EXCEPTION 'SC_AML_REGISTRO_INMUTABLE: una solicitud de levantamiento solo se decide una vez'
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF NEW.estado IN ('APROBADA', 'RECHAZADA')
       AND (NEW.decidida_por IS DISTINCT FROM aml.actor_actual()
            OR NOT EXISTS (SELECT 1 FROM kyc.credenciales WHERE cliente_id = NEW.decidida_por AND rol = 'CUMPLIMIENTO')) THEN
        RAISE EXCEPTION 'SC_AML_NO_AUTORIZADO: una solicitud la decide un oficial de cumplimiento (aml.decidir_levantamiento)';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_aml_solicitudes_proteger BEFORE INSERT OR UPDATE OR DELETE ON aml.solicitudes_levantamiento
    FOR EACH ROW EXECUTE FUNCTION aml.fn_proteger_solicitud();
CREATE TRIGGER trg_aml_solicitudes_no_truncate BEFORE TRUNCATE ON aml.solicitudes_levantamiento
    FOR EACH STATEMENT EXECUTE FUNCTION aml.fn_proteger_solicitud();

-- 7.7 Congelamientos: identidad inmutable; ACTIVO -> LEVANTADO una vez y solo
-- con una solicitud aprobada (cuatro ojos) de esta misma transacción.
CREATE FUNCTION aml.fn_proteger_congelamiento() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    s aml.solicitudes_levantamiento;
    s_xmin xid;
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'SC_AML_REGISTRO_INMUTABLE: un congelamiento no se borra' USING ERRCODE = 'restrict_violation';
    END IF;
    IF OLD.estado = 'LEVANTADO'
       OR (NEW.id, NEW.folio, NEW.cliente_id, NEW.origen, NEW.congelado_en, NEW.congelado_por, NEW.motivo,
           NEW.evaluacion_id, NEW.puntaje, NEW.sla_vence_en, NEW.cuenta_ifpe_id)
          IS DISTINCT FROM
          (OLD.id, OLD.folio, OLD.cliente_id, OLD.origen, OLD.congelado_en, OLD.congelado_por, OLD.motivo,
           OLD.evaluacion_id, OLD.puntaje, OLD.sla_vence_en, OLD.cuenta_ifpe_id)
       OR (OLD.alerta_id IS NOT NULL AND NEW.alerta_id IS DISTINCT FROM OLD.alerta_id)
       OR (OLD.confirmado AND NOT NEW.confirmado)
       OR NEW.intentos_bloqueados < OLD.intentos_bloqueados
       OR (NEW.estado = 'ACTIVO' AND (NEW.levantado_por, NEW.levantado_en, NEW.solicitud_id)
                                      IS DISTINCT FROM (OLD.levantado_por, OLD.levantado_en, OLD.solicitud_id)) THEN
        RAISE EXCEPTION 'SC_AML_REGISTRO_INMUTABLE: el congelamiento % no admite ese cambio', OLD.folio
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF NEW.estado = 'LEVANTADO' THEN
        SELECT * INTO s FROM aml.solicitudes_levantamiento WHERE id = NEW.solicitud_id;
        SELECT x.xmin INTO s_xmin FROM aml.solicitudes_levantamiento x WHERE x.id = NEW.solicitud_id;
        IF s.id IS NULL OR s.congelamiento_id <> OLD.id OR s.estado <> 'APROBADA'
           OR s.tipo NOT IN ('LEVANTAR_Y_LIBERAR', 'LEVANTAR_CONSERVANDO_RETENCIONES')
           OR s.decidida_por IS DISTINCT FROM NEW.levantado_por OR s.decidida_por = s.solicitada_por
           OR s_xmin IS DISTINCT FROM pg_current_xact_id()::xid THEN
            RAISE EXCEPTION 'SC_AML_LEVANTAMIENTO_SIN_APROBACION: el congelamiento % solo se levanta con una solicitud aprobada por un segundo oficial en esta transacción',
                OLD.folio;
        END IF;
    END IF;
    NEW.actualizado_en := clock_timestamp();
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_aml_congelamientos_proteger BEFORE UPDATE OR DELETE ON aml.congelamientos
    FOR EACH ROW EXECUTE FUNCTION aml.fn_proteger_congelamiento();
CREATE TRIGGER trg_aml_congelamientos_no_truncate BEFORE TRUNCATE ON aml.congelamientos
    FOR EACH STATEMENT EXECUTE FUNCTION aml.fn_proteger_congelamiento();

-- 7.8 Retenciones: VIGENTE -> LIBERADA una vez, con una solicitud aprobada de
-- liberación de esta misma transacción.
CREATE FUNCTION aml.fn_proteger_retencion() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    s aml.solicitudes_levantamiento;
    s_xmin xid;
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE')
       OR (to_jsonb(NEW) - ARRAY['estado', 'solicitud_id', 'tx_liberacion_id', 'liberada_en'])
          IS DISTINCT FROM (to_jsonb(OLD) - ARRAY['estado', 'solicitud_id', 'tx_liberacion_id', 'liberada_en'])
       OR NOT (OLD.estado = 'VIGENTE' AND NEW.estado = 'LIBERADA') THEN
        RAISE EXCEPTION 'SC_AML_REGISTRO_INMUTABLE: una retención solo se libera una vez' USING ERRCODE = 'restrict_violation';
    END IF;
    SELECT * INTO s FROM aml.solicitudes_levantamiento WHERE id = NEW.solicitud_id;
    SELECT x.xmin INTO s_xmin FROM aml.solicitudes_levantamiento x WHERE x.id = NEW.solicitud_id;
    IF s.id IS NULL OR s.estado <> 'APROBADA' OR s.tipo NOT IN ('LEVANTAR_Y_LIBERAR', 'LIBERAR_RETENCIONES')
       OR s.congelamiento_id <> OLD.congelamiento_id OR s_xmin IS DISTINCT FROM pg_current_xact_id()::xid THEN
        RAISE EXCEPTION 'SC_AML_LIBERACION_SIN_APROBACION: la retención % solo se libera con una solicitud de liberación aprobada en esta transacción',
            OLD.id;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_aml_retenciones_proteger BEFORE UPDATE OR DELETE ON aml.retenciones
    FOR EACH ROW EXECUTE FUNCTION aml.fn_proteger_retencion();
CREATE TRIGGER trg_aml_retenciones_no_truncate BEFORE TRUNCATE ON aml.retenciones
    FOR EACH STATEMENT EXECUTE FUNCTION aml.fn_proteger_retencion();

-- 7.9 Alertas: máquina de estados; mientras están abiertas el puntaje, el
-- nivel y la decisión solo suben y los plazos solo se acortan.
CREATE FUNCTION aml.fn_proteger_alerta() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE') THEN
        RAISE EXCEPTION 'SC_AML_ALERTA_INMUTABLE: las alertas de cumplimiento no se borran'
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF (NEW.id, NEW.folio, NEW.cliente_id, NEW.transaccion_id, NEW.entidad, NEW.sentido, NEW.evaluacion_id, NEW.regla_id,
        NEW.regla_codigo, NEW.origen, NEW."timestamp", NEW.clave_deduplicacion, NEW.alerta_pld_id, NEW.legado_analista)
       IS DISTINCT FROM
       (OLD.id, OLD.folio, OLD.cliente_id, OLD.transaccion_id, OLD.entidad, OLD.sentido, OLD.evaluacion_id, OLD.regla_id,
        OLD.regla_codigo, OLD.origen, OLD."timestamp", OLD.clave_deduplicacion, OLD.alerta_pld_id, OLD.legado_analista) THEN
        RAISE EXCEPTION 'SC_AML_ALERTA_INMUTABLE: la identidad de la alerta % no cambia', OLD.folio
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF OLD.estado IN ('CONFIRMADO', 'FALSO_POSITIVO') THEN
        IF NEW.estado IS DISTINCT FROM OLD.estado THEN
            RAISE EXCEPTION 'SC_AML_TRANSICION_INVALIDA: la alerta % ya está cerrada (%)', OLD.folio, OLD.estado;
        END IF;
        -- Lo único que cambia después del cierre: el folio UIF, una vez.
        IF (to_jsonb(NEW) - ARRAY['folio_uif', 'reportada_uif_en', 'actualizado_en'])
           IS DISTINCT FROM (to_jsonb(OLD) - ARRAY['folio_uif', 'reportada_uif_en', 'actualizado_en'])
           OR ((NEW.folio_uif, NEW.reportada_uif_en) IS DISTINCT FROM (OLD.folio_uif, OLD.reportada_uif_en)
               AND (OLD.estado <> 'CONFIRMADO' OR OLD.folio_uif IS NOT NULL OR OLD.reportada_uif_en IS NOT NULL)) THEN
            RAISE EXCEPTION 'SC_AML_ALERTA_INMUTABLE: la alerta % está cerrada', OLD.folio
                USING ERRCODE = 'restrict_violation';
        END IF;
    ELSE
        IF NEW.estado IS DISTINCT FROM OLD.estado AND NOT (
               (OLD.estado = 'PENDIENTE' AND NEW.estado = 'EN_REVISION')
            OR (OLD.estado = 'EN_REVISION' AND NEW.estado IN ('PENDIENTE', 'CONFIRMADO', 'FALSO_POSITIVO'))) THEN
            RAISE EXCEPTION 'SC_AML_TRANSICION_INVALIDA: la alerta % no puede pasar de % a %',
                OLD.folio, OLD.estado, NEW.estado;
        END IF;
        IF NEW.puntaje < OLD.puntaje OR NEW.nivel_riesgo < OLD.nivel_riesgo
           OR aml.rango_decision(NEW.decision) < aml.rango_decision(OLD.decision)
           OR NEW.fecha_limite_reporte > OLD.fecha_limite_reporte
           OR (OLD.urgente AND NOT NEW.urgente) OR (OLD.escalada AND NOT NEW.escalada)
           OR (OLD.congelamiento_id IS NOT NULL AND NEW.congelamiento_id IS DISTINCT FROM OLD.congelamiento_id)
           OR NEW.ocurrencias < OLD.ocurrencias THEN
            RAISE EXCEPTION 'SC_AML_TRANSICION_INVALIDA: la alerta % solo puede agravarse mientras está abierta', OLD.folio;
        END IF;
    END IF;
    NEW.actualizado_en := clock_timestamp();
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_aml_alertas_proteger BEFORE UPDATE OR DELETE ON aml.alertas
    FOR EACH ROW EXECUTE FUNCTION aml.fn_proteger_alerta();
CREATE TRIGGER trg_aml_alertas_no_truncate BEFORE TRUNCATE ON aml.alertas
    FOR EACH STATEMENT EXECUTE FUNCTION aml.fn_proteger_alerta();

-- 7.10 Operaciones relevantes: solo el folio del reporte trimestral, una vez.
CREATE FUNCTION aml.fn_proteger_relevante() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE')
       OR (to_jsonb(NEW) - ARRAY['reporte_folio', 'reportada_en']) IS DISTINCT FROM (to_jsonb(OLD) - ARRAY['reporte_folio', 'reportada_en'])
       OR OLD.reporte_folio IS NOT NULL OR OLD.reportada_en IS NOT NULL THEN
        RAISE EXCEPTION 'SC_AML_REGISTRO_INMUTABLE: el registro de operaciones relevantes solo admite su folio de reporte, una vez'
            USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_aml_relevantes_proteger BEFORE UPDATE OR DELETE ON aml.operaciones_relevantes
    FOR EACH ROW EXECUTE FUNCTION aml.fn_proteger_relevante();
CREATE TRIGGER trg_aml_relevantes_no_truncate BEFORE TRUNCATE ON aml.operaciones_relevantes
    FOR EACH STATEMENT EXECUTE FUNCTION aml.fn_proteger_relevante();

-- 7.11 Notificaciones: solo se marca la entrega. Sin pg_notify: nadie escucha
-- (la consola consulta y el ciclo entrega la bandeja), y un NOTIFY serializaría
-- los COMMIT de los caminos del dinero que alertan o congelan.
CREATE FUNCTION aml.fn_proteger_notificacion() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP IN ('DELETE', 'TRUNCATE')
       OR (to_jsonb(NEW) - 'entregada_en') IS DISTINCT FROM (to_jsonb(OLD) - 'entregada_en')
       OR OLD.entregada_en IS NOT NULL OR NEW.entregada_en IS NULL THEN
        RAISE EXCEPTION 'SC_AML_REGISTRO_INMUTABLE: una notificación solo se marca como entregada, una vez'
            USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_aml_notificaciones_proteger BEFORE UPDATE OR DELETE ON aml.notificaciones
    FOR EACH ROW EXECUTE FUNCTION aml.fn_proteger_notificacion();
CREATE TRIGGER trg_aml_notificaciones_no_truncate BEFORE TRUNCATE ON aml.notificaciones
    FOR EACH STATEMENT EXECUTE FUNCTION aml.fn_proteger_notificacion();

-- 7.12 Bitácora y notificaciones de los cambios de estado (el actor lo fijó el
-- punto de entrada; ver aml.fijar_actor).
CREATE FUNCTION aml.fn_bitacora_alerta() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_resumen TEXT := format('%s · %s · alerta %s', NEW.regla_codigo, NEW.nivel_riesgo, NEW.folio);
    v_motivo  TEXT := aml.motivo_actual();
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM aml.registrar_bitacora('ALERTA_CREADA', 'ALERTA', NEW.id::text, NEW.cliente_id, NEW.id, NEW.congelamiento_id,
            NULL, NEW.transaccion_id, NULL, NEW.estado::text, NULL,
            jsonb_build_object('regla', NEW.regla_codigo, 'nivel', NEW.nivel_riesgo, 'puntaje', NEW.puntaje::text,
                               'decision', NEW.decision, 'origen', NEW.origen));
        IF NEW.nivel_riesgo = 'CRITICO' THEN
            PERFORM aml.notificar('ALERTA_CRITICA', 'ALTA', NEW.cliente_id, NEW.id, NULL, NULL, NULL, v_resumen);
        ELSIF NEW.nivel_riesgo = 'ALTO' THEN
            PERFORM aml.notificar('ALERTA_NUEVA', 'MEDIA', NEW.cliente_id, NEW.id, NULL, NULL, NULL, v_resumen);
        END IF;
        RETURN NULL;
    END IF;

    IF NEW.ocurrencias <> OLD.ocurrencias THEN
        PERFORM aml.registrar_bitacora('ALERTA_AGRUPADA', 'ALERTA', NEW.id::text, NEW.cliente_id, NEW.id, NEW.congelamiento_id,
            NULL, NULL, NULL, NULL, NULL,
            jsonb_build_object('ocurrencias', NEW.ocurrencias, 'puntaje', NEW.puntaje::text, 'nivel', NEW.nivel_riesgo,
                               'decision', NEW.decision));
    END IF;
    IF NEW.estado IS DISTINCT FROM OLD.estado THEN
        PERFORM aml.registrar_bitacora(
            CASE WHEN NEW.estado = 'EN_REVISION' THEN 'ALERTA_TOMADA'
                 WHEN NEW.estado = 'PENDIENTE' THEN 'ALERTA_LIBERADA'
                 WHEN NEW.estado = 'CONFIRMADO' THEN 'ALERTA_CONFIRMADA'
                 ELSE 'ALERTA_FALSO_POSITIVO' END,
            'ALERTA', NEW.id::text, NEW.cliente_id, NEW.id, NEW.congelamiento_id, NULL, NULL, OLD.estado::text,
            NEW.estado::text,
            CASE WHEN NEW.estado IN ('CONFIRMADO', 'FALSO_POSITIVO') THEN coalesce(v_motivo, NEW.dictamen) ELSE v_motivo END,
            jsonb_build_object('asignada_a', NEW.asignada_a, 'tipo_reporte', NEW.tipo_reporte,
                               'aprobada_por', NEW.aprobada_por));
    ELSIF OLD.estado = 'EN_REVISION' AND NEW.asignada_a IS DISTINCT FROM OLD.asignada_a THEN
        -- Alerta heredada EN_REVISION sin asignar que toma un oficial.
        PERFORM aml.registrar_bitacora('ALERTA_TOMADA', 'ALERTA', NEW.id::text, NEW.cliente_id, NEW.id, NEW.congelamiento_id,
            NULL, NULL, OLD.estado::text, NEW.estado::text, v_motivo, jsonb_build_object('asignada_a', NEW.asignada_a));
    END IF;
    IF NEW.escalada AND NOT OLD.escalada THEN
        PERFORM aml.registrar_bitacora('ALERTA_ESCALADA', 'ALERTA', NEW.id::text, NEW.cliente_id, NEW.id, NEW.congelamiento_id,
            NULL, NULL, OLD.nivel_riesgo::text, NEW.nivel_riesgo::text, v_motivo, '{}');
    END IF;
    IF NEW.resolucion_propuesta IS NOT NULL AND OLD.resolucion_propuesta IS NULL THEN
        PERFORM aml.registrar_bitacora('ALERTA_FP_PROPUESTO', 'ALERTA', NEW.id::text, NEW.cliente_id, NEW.id,
            NEW.congelamiento_id, NULL, NULL, NULL, NEW.resolucion_propuesta, NEW.dictamen_propuesto,
            jsonb_build_object('propuesta_por', NEW.propuesta_por));
    ELSIF NEW.resolucion_propuesta IS NULL AND OLD.resolucion_propuesta IS NOT NULL AND NEW.estado = OLD.estado THEN
        PERFORM aml.registrar_bitacora('ALERTA_FP_RETIRADO', 'ALERTA', NEW.id::text, NEW.cliente_id, NEW.id,
            NEW.congelamiento_id, NULL, NULL, OLD.resolucion_propuesta, NULL, v_motivo,
            jsonb_build_object('propuesta_por', OLD.propuesta_por));
    END IF;
    IF NEW.folio_uif IS NOT NULL AND OLD.folio_uif IS NULL THEN
        PERFORM aml.registrar_bitacora('REPORTE_UIF_REGISTRADO', 'ALERTA', NEW.id::text, NEW.cliente_id, NEW.id,
            NEW.congelamiento_id, NULL, NULL, NULL, NULL, NULL,
            jsonb_build_object('folio_uif', NEW.folio_uif, 'reportada_uif_en', to_char(NEW.reportada_uif_en, 'YYYY-MM-DD')));
    END IF;
    IF NEW.nivel_riesgo = 'CRITICO' AND OLD.nivel_riesgo <> 'CRITICO' THEN
        PERFORM aml.notificar('ALERTA_CRITICA', 'ALTA', NEW.cliente_id, NEW.id, NULL, NULL, NULL, v_resumen);
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_aml_alertas_bitacora AFTER INSERT OR UPDATE ON aml.alertas
    FOR EACH ROW EXECUTE FUNCTION aml.fn_bitacora_alerta();

CREATE FUNCTION aml.fn_bitacora_congelamiento() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM aml.registrar_bitacora('CONGELAMIENTO_APLICADO', 'CONGELAMIENTO', NEW.id::text, NEW.cliente_id, NEW.alerta_id,
            NEW.id, NULL, NULL, NULL, NEW.estado, NEW.motivo,
            jsonb_build_object('folio', NEW.folio, 'origen', NEW.origen, 'puntaje', NEW.puntaje::text,
                               'cuenta_ifpe_id', NEW.cuenta_ifpe_id, 'evaluacion_id', NEW.evaluacion_id,
                               'sla_vence_en', NEW.sla_vence_en));
        RETURN NULL;
    END IF;
    IF NEW.confirmado AND NOT OLD.confirmado THEN
        PERFORM aml.registrar_bitacora('CONGELAMIENTO_CONFIRMADO', 'CONGELAMIENTO', NEW.id::text, NEW.cliente_id,
            NEW.alerta_id, NEW.id, NULL, NULL, NULL, NULL, NULL, jsonb_build_object('folio', NEW.folio));
    END IF;
    IF NEW.estado = 'LEVANTADO' AND OLD.estado = 'ACTIVO' THEN
        PERFORM aml.registrar_bitacora('CONGELAMIENTO_LEVANTADO', 'CONGELAMIENTO', NEW.id::text, NEW.cliente_id,
            NEW.alerta_id, NEW.id, NEW.solicitud_id, NULL, OLD.estado, NEW.estado, NULL,
            jsonb_build_object('folio', NEW.folio, 'levantado_por', NEW.levantado_por));
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_aml_congelamientos_bitacora AFTER INSERT OR UPDATE ON aml.congelamientos
    FOR EACH ROW EXECUTE FUNCTION aml.fn_bitacora_congelamiento();

-- Marca de cambio de la bandeja: aml.alerta_json incrusta el estado del
-- congelamiento ligado (estado, confirmado, vencimiento del SLA), así que un
-- cambio de esos datos toca actualizado_en de sus alertas (también las
-- cerradas: fn_proteger_alerta lo admite) y el sondeo de la consola las vuelve
-- a enviar. Sin esto, una consola abierta seguía mostrando "restringido"
-- después de un levantamiento aprobado.
CREATE FUNCTION aml.fn_congelamiento_toca_alertas() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    UPDATE aml.alertas SET actualizado_en = clock_timestamp() WHERE congelamiento_id = NEW.id;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_aml_congelamientos_toca_alertas AFTER UPDATE ON aml.congelamientos
    FOR EACH ROW WHEN ((OLD.estado, OLD.confirmado, OLD.sla_vence_en) IS DISTINCT FROM
                       (NEW.estado, NEW.confirmado, NEW.sla_vence_en))
    EXECUTE FUNCTION aml.fn_congelamiento_toca_alertas();

CREATE FUNCTION aml.fn_bitacora_retencion() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM aml.registrar_bitacora('RETENCION_APLICADA', 'RETENCION', NEW.id::text, NEW.cliente_id, NULL,
            NEW.congelamiento_id, NULL, NEW.tx_retencion_id, NULL, NEW.estado, NULL,
            jsonb_build_object('monto', NEW.monto::text, 'transaccion_origen_id', NEW.transaccion_origen_id,
                               'cuenta_id', NEW.cuenta_id, 'evaluacion_id', NEW.evaluacion_id));
    ELSIF NEW.estado = 'LIBERADA' AND OLD.estado = 'VIGENTE' THEN
        PERFORM aml.registrar_bitacora('RETENCION_LIBERADA', 'RETENCION', NEW.id::text, NEW.cliente_id, NULL,
            NEW.congelamiento_id, NEW.solicitud_id, NEW.tx_liberacion_id, OLD.estado, NEW.estado, NULL,
            jsonb_build_object('monto', NEW.monto::text, 'transaccion_origen_id', NEW.transaccion_origen_id));
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_aml_retenciones_bitacora AFTER INSERT OR UPDATE ON aml.retenciones
    FOR EACH ROW EXECUTE FUNCTION aml.fn_bitacora_retencion();

CREATE FUNCTION aml.fn_bitacora_solicitud() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_cliente UUID := (SELECT cliente_id FROM aml.congelamientos WHERE id = NEW.congelamiento_id);
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM aml.registrar_bitacora('LEVANTAMIENTO_SOLICITADO', 'SOLICITUD', NEW.id::text, v_cliente, NULL,
            NEW.congelamiento_id, NEW.id, NULL, NULL, NEW.estado, NEW.justificacion,
            jsonb_build_object('folio', NEW.folio, 'tipo', NEW.tipo));
        PERFORM aml.notificar('SOLICITUD_LEVANTAMIENTO', 'ALTA', v_cliente, NULL, NEW.congelamiento_id, NEW.id, NULL,
            format('%s · solicitud %s', NEW.tipo, NEW.folio));
    ELSIF NEW.estado <> OLD.estado THEN
        PERFORM aml.registrar_bitacora(
            CASE NEW.estado WHEN 'APROBADA' THEN 'LEVANTAMIENTO_APROBADO' WHEN 'RECHAZADA' THEN 'LEVANTAMIENTO_RECHAZADO'
                            ELSE 'LEVANTAMIENTO_ANULADO' END,
            'SOLICITUD', NEW.id::text, v_cliente, NULL, NEW.congelamiento_id, NEW.id, NULL, OLD.estado, NEW.estado,
            NEW.comentario, jsonb_build_object('folio', NEW.folio, 'tipo', NEW.tipo, 'decidida_por', NEW.decidida_por));
        PERFORM aml.notificar('LEVANTAMIENTO_DECIDIDO', 'MEDIA', v_cliente, NULL, NEW.congelamiento_id, NEW.id, NULL,
            format('%s · solicitud %s · %s', NEW.tipo, NEW.folio, NEW.estado));
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_aml_solicitudes_bitacora AFTER INSERT OR UPDATE ON aml.solicitudes_levantamiento
    FOR EACH ROW EXECUTE FUNCTION aml.fn_bitacora_solicitud();

CREATE FUNCTION aml.fn_bitacora_propuesta() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM aml.registrar_bitacora('PROPUESTA_CREADA', 'PROPUESTA', NEW.id::text, NULL, NULL, NULL, NULL, NULL, NULL,
            NEW.estado, NEW.justificacion,
            jsonb_build_object('folio', NEW.folio, 'tipo', NEW.tipo, 'regla_codigo', NEW.regla_codigo,
                               'nivel_cliente', NEW.nivel_cliente, 'cambios', NEW.cambios));
        PERFORM aml.notificar('PROPUESTA_PENDIENTE', 'MEDIA', NULL, NULL, NULL, NULL, NEW.id,
            format('%s%s · propuesta %s', NEW.tipo, coalesce(' ' || NEW.regla_codigo, ''), NEW.folio));
    ELSIF NEW.estado <> OLD.estado THEN
        PERFORM aml.registrar_bitacora(
            CASE NEW.estado WHEN 'APROBADA' THEN 'PROPUESTA_APROBADA' ELSE 'PROPUESTA_RECHAZADA' END,
            'PROPUESTA', NEW.id::text, NULL, NULL, NULL, NULL, NULL, OLD.estado, NEW.estado, NEW.comentario,
            jsonb_build_object('folio', NEW.folio, 'resultado_id', NEW.resultado_id));
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_aml_propuestas_bitacora AFTER INSERT OR UPDATE ON aml.propuestas_cambio
    FOR EACH ROW EXECUTE FUNCTION aml.fn_bitacora_propuesta();

CREATE FUNCTION aml.fn_bitacora_regla() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM aml.registrar_bitacora('REGLA_VERSION_NUEVA', 'REGLA', NEW.id::text, NULL, NULL, NULL, NULL, NULL, NULL,
            NULL, NULL, to_jsonb(NEW));
    ELSIF OLD.activa AND NOT NEW.activa THEN
        PERFORM aml.registrar_bitacora('REGLA_DESACTIVADA', 'REGLA', NEW.id::text, NULL, NULL, NULL, NULL, NULL, 'ACTIVA',
            'INACTIVA', NULL, jsonb_build_object('codigo', NEW.codigo, 'nivel_cliente', NEW.nivel_cliente,
                                                 'version', NEW.version));
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_aml_reglas_bitacora AFTER INSERT OR UPDATE ON aml.reglas_riesgo
    FOR EACH ROW EXECUTE FUNCTION aml.fn_bitacora_regla();

-- La versión nueva de los parámetros con la diferencia contra la anterior.
CREATE FUNCTION aml.fn_bitacora_parametros() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_anterior JSONB := (SELECT to_jsonb(p) FROM aml.parametros p WHERE p.id < NEW.id ORDER BY p.id DESC LIMIT 1);
    v_nuevo    JSONB := to_jsonb(NEW);
BEGIN
    PERFORM aml.registrar_bitacora('PARAMETROS_VERSION_NUEVA', 'PARAMETROS', NEW.id::text, NULL, NULL, NULL, NULL, NULL,
        NULL, NULL, NULL,
        jsonb_build_object('parametros', v_nuevo, 'diferencia', coalesce((
            SELECT jsonb_object_agg(k, jsonb_build_object('anterior', v_anterior -> k, 'nuevo', v_nuevo -> k))
              FROM jsonb_object_keys(v_nuevo) k
             WHERE k NOT IN ('id', 'creado_en', 'propuesta_id') AND (v_anterior -> k) IS DISTINCT FROM (v_nuevo -> k)), '{}')));
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_aml_parametros_bitacora AFTER INSERT ON aml.parametros
    FOR EACH ROW EXECUTE FUNCTION aml.fn_bitacora_parametros();

-- Las semillas de la sección 4 se insertaron antes de estos triggers: su
-- versión 1 queda en la bitácora igual que cualquier versión nueva.
DO $$
DECLARE
    r aml.reglas_riesgo;
    p aml.parametros;
BEGIN
    PERFORM aml.fijar_actor(NULL);
    FOR p IN SELECT * FROM aml.parametros ORDER BY id LOOP
        PERFORM aml.registrar_bitacora('PARAMETROS_VERSION_NUEVA', 'PARAMETROS', p.id::text, NULL, NULL, NULL, NULL, NULL,
            NULL, NULL, NULL, jsonb_build_object('parametros', to_jsonb(p), 'diferencia', '{}'::jsonb));
    END LOOP;
    FOR r IN SELECT * FROM aml.reglas_riesgo ORDER BY codigo, nivel_cliente, version LOOP
        PERFORM aml.registrar_bitacora('REGLA_VERSION_NUEVA', 'REGLA', r.id::text, NULL, NULL, NULL, NULL, NULL, NULL,
            NULL, NULL, to_jsonb(r));
    END LOOP;
END
$$;

-- -----------------------------------------------------------------------------
-- 8. Triggers sobre las tablas del dinero y de KYC
-- -----------------------------------------------------------------------------
-- Triggers y no RLS: error.rs traduce las negativas de RLS a KYC_NO_VERIFICADO,
-- y el dueño del esquema esquiva RLS pero no los triggers.

-- 8.1 Guardia de débitos IFPE (BEFORE INSERT; su nombre la ordena antes de
-- trg_ifpe_tx_encadenar, así que rechaza antes del candado de la cabeza).
CREATE FUNCTION aml.fn_guardia_tx_ifpe() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_estado  TEXT;
    v_cliente UUID;
BEGIN
    IF NEW.tipo NOT IN ('RETIRO', 'TRANSFERENCIA', 'TRASPASO_A_SOFOM') THEN
        RETURN NEW;
    END IF;
    SELECT estado, cliente_id INTO v_estado, v_cliente FROM ifpe.cuentas WHERE id = NEW.cuenta_origen_id;
    IF NEW.tipo = 'RETIRO' AND starts_with(NEW.clave_idempotencia, 'spei-correccion:') THEN
        -- Las correcciones (y anulaciones) de un abono erróneo siguen
        -- permitidas; sobre un cliente congelado quedan en la bitácora y se
        -- notifican. Nunca alcanzan fondos retenidos: cargan saldo_disponible y
        -- trg_ifpe_cuentas_zz_aml_retenido protege el resto. La clave sola no
        -- basta: trg_ifpe_tx_zz_aml_correccion exige al COMMIT su registro en
        -- ifpe.spei_correcciones (sin él, ni el dueño debita a un congelado).
        IF v_estado = 'CONGELADO_PREVENTIVO' OR aml.cliente_congelado(v_cliente) THEN
            PERFORM aml.fijar_actor(NULL);
            PERFORM aml.registrar_bitacora('CARGO_EN_CUENTA_CONGELADA', 'OPERACION', NEW.id::text, v_cliente,
                NULL, (SELECT id FROM aml.congelamientos WHERE cliente_id = v_cliente AND estado = 'ACTIVO'), NULL, NEW.id,
                NULL, NULL, NULL, jsonb_build_object('monto', NEW.monto::text, 'clave', NEW.clave_idempotencia));
            PERFORM aml.notificar('CARGO_EN_CUENTA_CONGELADA', 'ALTA', v_cliente, NULL,
                (SELECT id FROM aml.congelamientos WHERE cliente_id = v_cliente AND estado = 'ACTIVO'),
                NULL, NULL, format('Corrección SPEI de %s MXN sobre cliente congelado', NEW.monto));
        END IF;
        RETURN NEW;
    END IF;
    IF v_estado = 'CONGELADA' THEN -- cierra el hueco: CONGELADA solo se exigía en Rust
        RAISE EXCEPTION 'SC_CUENTA_CONGELADA: la cuenta IFPE está congelada' USING ERRCODE = 'check_violation';
    END IF;
    IF v_estado = 'CONGELADO_PREVENTIVO' OR aml.cliente_congelado(v_cliente) THEN
        RAISE EXCEPTION 'SC_OPERACION_NO_DISPONIBLE: la operación no está disponible en este momento'
            USING ERRCODE = 'check_violation', DETAIL = format('cuenta %s', NEW.cuenta_origen_id);
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_ifpe_tx_aml_guardia BEFORE INSERT ON ifpe.transacciones
    FOR EACH ROW EXECUTE FUNCTION aml.fn_guardia_tx_ifpe();

-- Una corrección se reconoce por su registro, no por su clave: un RETIRO
-- 'spei-correccion:<id>' sin la fila de ifpe.spei_correcciones que lo liga
-- (tx_id y clave) no es una corrección. Sobre un cliente congelado es un
-- débito prohibido (SC_OPERACION_NO_DISPONIBLE); sobre cualquier otro, un uso
-- de una clave reservada (se saltaría la cobertura y la velocidad PLD).
CREATE FUNCTION aml.fn_correccion_registrada() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_estado  TEXT;
    v_cliente UUID;
BEGIN
    IF EXISTS (SELECT 1 FROM ifpe.spei_correcciones c
                WHERE c.tx_id = NEW.id AND NEW.clave_idempotencia = 'spei-correccion:' || c.id::text) THEN
        RETURN NULL;
    END IF;
    SELECT estado, cliente_id INTO v_estado, v_cliente FROM ifpe.cuentas WHERE id = NEW.cuenta_origen_id;
    IF v_estado = 'CONGELADO_PREVENTIVO' OR aml.cliente_congelado(v_cliente) THEN
        RAISE EXCEPTION 'SC_OPERACION_NO_DISPONIBLE: la operación no está disponible en este momento'
            USING ERRCODE = 'check_violation', DETAIL = format('cuenta %s', NEW.cuenta_origen_id);
    END IF;
    RAISE EXCEPTION 'SC_AML_CLAVE_RESERVADA: la clave % está reservada para correcciones registradas', NEW.clave_idempotencia;
END
$$;
CREATE CONSTRAINT TRIGGER trg_ifpe_tx_zz_aml_correccion
    AFTER INSERT ON ifpe.transacciones
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN (NEW.tipo = 'RETIRO' AND NEW.clave_idempotencia LIKE 'spei-correccion:%')
    EXECUTE FUNCTION aml.fn_correccion_registrada();

-- Una orden SPEI nunca usa una clave reservada del ledger (abonos, reversos,
-- correcciones y retenciones): su RETIRO se haría pasar por otro movimiento.
CREATE FUNCTION aml.fn_orden_clave_reservada() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF NEW.clave_idempotencia ~ '^(spei|spei-reverso|spei-correccion|ret|lib):' THEN
        RAISE EXCEPTION 'SC_AML_CLAVE_RESERVADA: la clave % está reservada y no identifica una orden SPEI',
            NEW.clave_idempotencia;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_spei_outbound_aml_clave BEFORE INSERT ON ifpe.spei_outbound
    FOR EACH ROW EXECUTE FUNCTION aml.fn_orden_clave_reservada();

-- 8.2 Cobertura: cada perspectiva de un movimiento de dinero de un cliente
-- tiene su evaluación EN_LINEA coherente (mismo cliente, monto, canal y
-- transacción) o recibe una marca DIFERIDA que el monitoreo califica. Nunca
-- falla por una evaluación ausente.
CREATE FUNCTION aml.fn_cubrir(p_entidad TEXT, p_tx UUID, p_xmin xid, p_sentido TEXT, p_cliente UUID, p_monto NUMERIC,
                              p_canal TEXT, p_canales TEXT[], p_clave TEXT) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    e      aml.evaluaciones;
    e_xmin xid;
    v_id   UUID;
BEGIN
    SELECT * INTO e FROM aml.evaluaciones
     WHERE entidad = p_entidad AND transaccion_id = p_tx AND sentido = p_sentido AND origen = 'EN_LINEA';
    IF e.id IS NOT NULL THEN
        SELECT x.xmin INTO e_xmin FROM aml.evaluaciones x WHERE x.id = e.id;
        IF e.cliente_id IS DISTINCT FROM p_cliente OR e.monto <> p_monto OR NOT (e.canal = ANY (p_canales))
           OR e.resultado NOT IN ('PERMITIDA', 'ALERTADA', 'RETENIDA', 'PENDIENTE_REEVALUACION')
           OR e_xmin IS DISTINCT FROM p_xmin THEN
            RAISE EXCEPTION 'SC_AML_EVALUACION_INCOHERENTE: la evaluación % no corresponde al movimiento % (%)',
                e.id, p_tx, p_sentido;
        END IF;
        RETURN;
    END IF;
    IF p_cliente IS NULL THEN
        RETURN; -- cuenta sin dueño: imposible por las llaves foráneas
    END IF;
    PERFORM aml.fijar_actor(NULL);
    INSERT INTO aml.evaluaciones (cliente_id, entidad, sentido, canal, monto, transaccion_id, clave_operacion, resultado,
                                  origen, parametros_id)
    VALUES (p_cliente, p_entidad, p_sentido, p_canal, p_monto, p_tx, p_clave, 'PENDIENTE_REEVALUACION', 'DIFERIDA',
            (SELECT id FROM aml.parametros_vigentes()))
    ON CONFLICT DO NOTHING
    RETURNING id INTO v_id;
    IF v_id IS NOT NULL THEN
        PERFORM aml.registrar_bitacora('EVALUACION_DIFERIDA', 'EVALUACION', v_id::text, p_cliente, NULL, NULL, NULL, p_tx,
            NULL, 'PENDIENTE_REEVALUACION', NULL,
            jsonb_build_object('entidad', p_entidad, 'sentido', p_sentido, 'canal', p_canal, 'monto', p_monto::text));
        PERFORM aml.notificar('COBERTURA', 'ALTA', p_cliente, NULL, NULL, NULL, NULL,
            format('%s · %s · movimiento sin evaluación en línea', p_canal, p_sentido));
    END IF;
END
$$;

CREATE FUNCTION aml.fn_cobertura_ifpe() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_xmin    xid;
    v_origen  UUID := (SELECT cliente_id FROM ifpe.cuentas WHERE id = NEW.cuenta_origen_id);
    v_destino UUID := (SELECT cliente_id FROM ifpe.cuentas WHERE id = NEW.cuenta_destino_id);
    v_canal   TEXT;
BEGIN
    SELECT x.xmin INTO v_xmin FROM ifpe.transacciones x WHERE x.id = NEW.id;
    CASE NEW.tipo
        WHEN 'DEPOSITO' THEN
            -- Errata E1: un DEPOSITO 'spei:' sin registro en spei_inbound (solo
            -- posible con trg_ifpe_tx_spei_registrado desactivado) es SPEI_WEBHOOK.
            SELECT CASE s.origen WHEN 'OPERADOR' THEN 'SPEI_OPERADOR' WHEN 'DEMO' THEN 'SPEI_DEMO' ELSE 'SPEI_WEBHOOK' END
              INTO v_canal FROM ifpe.spei_inbound s WHERE s.transaccion_id = NEW.id;
            PERFORM aml.fn_cubrir('IFPE', NEW.id, v_xmin, 'ENTRADA', v_destino, NEW.monto, coalesce(v_canal, 'SPEI_WEBHOOK'),
                                  ARRAY['SPEI_WEBHOOK', 'SPEI_OPERADOR', 'SPEI_DEMO'], NEW.clave_idempotencia);
        WHEN 'RETIRO' THEN
            PERFORM aml.fn_cubrir('IFPE', NEW.id, v_xmin, 'SALIDA', v_origen, NEW.monto, 'SPEI_ENVIADO',
                                  ARRAY['SPEI_ENVIADO'], NEW.clave_idempotencia);
        WHEN 'TRASPASO_A_SOFOM' THEN
            PERFORM aml.fn_cubrir('IFPE', NEW.id, v_xmin, 'SALIDA', v_origen, NEW.monto, 'INVERSION_MUTUO',
                                  ARRAY['INVERSION_MUTUO'], NEW.clave_idempotencia);
        WHEN 'TRANSFERENCIA' THEN
            PERFORM aml.fn_cubrir('IFPE', NEW.id, v_xmin, 'SALIDA', v_origen, NEW.monto, 'TRANSFERENCIA_ENVIADA',
                                  ARRAY['TRANSFERENCIA_ENVIADA'], NEW.clave_idempotencia);
            PERFORM aml.fn_cubrir('IFPE', NEW.id, v_xmin, 'ENTRADA', v_destino, NEW.monto, 'TRANSFERENCIA_RECIBIDA',
                                  ARRAY['TRANSFERENCIA_RECIBIDA'], NEW.clave_idempotencia);
        ELSE
            NULL;
    END CASE;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_ifpe_tx_zz_aml_cobertura
    AFTER INSERT ON ifpe.transacciones
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ((NEW.tipo = 'DEPOSITO' AND NEW.clave_idempotencia LIKE 'spei:%')
          OR NEW.tipo IN ('TRANSFERENCIA', 'TRASPASO_A_SOFOM')
          OR (NEW.tipo = 'RETIRO' AND NEW.clave_idempotencia NOT LIKE 'spei-correccion:%'))
    EXECUTE FUNCTION aml.fn_cobertura_ifpe();

-- 8.3 Todo abono a un cliente congelado (o calificado para retenerse) se
-- retiene en la misma transacción, por al menos su monto.
CREATE FUNCTION aml.fn_retencion_exigida() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_cliente UUID := (SELECT cliente_id FROM ifpe.cuentas WHERE id = NEW.cuenta_destino_id);
    v_xmin    xid;
BEGIN
    IF aml.cliente_congelado(v_cliente)
       OR EXISTS (SELECT 1 FROM aml.evaluaciones e
                   WHERE e.entidad = 'IFPE' AND e.transaccion_id = NEW.id AND e.sentido = 'ENTRADA'
                     AND e.resultado = 'RETENIDA') THEN
        SELECT x.xmin INTO v_xmin FROM ifpe.transacciones x WHERE x.id = NEW.id;
        IF NOT EXISTS (SELECT 1 FROM aml.retenciones r
                        WHERE r.transaccion_origen_id = NEW.id AND r.monto >= NEW.monto AND r.xmin = v_xmin) THEN
            RAISE EXCEPTION 'SC_AML_RETENCION_FALTANTE: el abono a un cliente congelado debe retenerse en la misma transacción';
        END IF;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_ifpe_tx_zz_aml_retencion
    AFTER INSERT ON ifpe.transacciones
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ((NEW.tipo = 'DEPOSITO' AND NEW.clave_idempotencia LIKE 'spei:%') OR NEW.tipo = 'TRANSFERENCIA')
    EXECUTE FUNCTION aml.fn_retencion_exigida();

-- 8.4 Claves reservadas 'ret:' y 'lib:': solo retenciones registradas en esta
-- misma transacción, de PLD (aml.retenciones) o del operador
-- (ifpe.retenciones_operador), siempre con el concepto neutro. Las dos tienen
-- la misma forma en el ledger del cliente.
CREATE FUNCTION aml.fn_clave_reservada() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_xmin xid;
    v_id   UUID;
BEGIN
    SELECT x.xmin INTO v_xmin FROM ifpe.transacciones x WHERE x.id = NEW.id;
    IF substr(NEW.clave_idempotencia, 5) ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' THEN
        v_id := substr(NEW.clave_idempotencia, 5)::uuid;
    END IF;
    IF starts_with(NEW.clave_idempotencia, 'ret:') THEN
        IF v_id IS NOT NULL AND NEW.tipo = 'RETENCION' AND NEW.concepto IS NOT DISTINCT FROM 'Retención temporal'
           AND (EXISTS (SELECT 1 FROM aml.retenciones r
                         WHERE r.id = v_id AND r.tx_retencion_id = NEW.id AND r.cuenta_id = NEW.cuenta_origen_id
                           AND r.monto = NEW.monto AND r.xmin = v_xmin)
                OR EXISTS (SELECT 1 FROM ifpe.retenciones_operador o
                            WHERE o.id = v_id AND o.transaccion_id = NEW.id AND o.tipo = 'RETENCION'
                              AND o.cuenta_id = NEW.cuenta_origen_id AND NEW.cuenta_destino_id = NEW.cuenta_origen_id
                              AND o.monto = NEW.monto AND o.xmin = v_xmin)) THEN
            RETURN NULL;
        END IF;
    ELSIF v_id IS NOT NULL AND NEW.tipo = 'LIBERACION_RETENCION'
          AND NEW.concepto IS NOT DISTINCT FROM 'Liberación de retención'
          AND (EXISTS (SELECT 1 FROM aml.retenciones r
                        WHERE r.id = v_id AND r.tx_liberacion_id = NEW.id AND r.cuenta_id = NEW.cuenta_origen_id
                          AND r.monto = NEW.monto AND r.estado = 'LIBERADA' AND r.xmin = v_xmin)
               OR EXISTS (SELECT 1 FROM ifpe.retenciones_operador o
                           WHERE o.id = v_id AND o.transaccion_id = NEW.id AND o.tipo = 'LIBERACION_RETENCION'
                             AND o.cuenta_id = NEW.cuenta_origen_id AND NEW.cuenta_destino_id = NEW.cuenta_origen_id
                             AND o.monto = NEW.monto AND o.xmin = v_xmin)) THEN
        RETURN NULL;
    END IF;
    RAISE EXCEPTION 'SC_AML_CLAVE_RESERVADA: la clave % está reservada para retenciones registradas', NEW.clave_idempotencia;
END
$$;
CREATE CONSTRAINT TRIGGER trg_ifpe_tx_zz_aml_clave
    AFTER INSERT ON ifpe.transacciones
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN (NEW.clave_idempotencia LIKE 'ret:%' OR NEW.clave_idempotencia LIKE 'lib:%')
    EXECUTE FUNCTION aml.fn_clave_reservada();

-- 8.5 El saldo bloqueado de una cuenta nunca baja de sus retenciones PLD
-- vigentes (una liberación genérica no las alcanza). Código neutro.
CREATE FUNCTION aml.fn_retenido_protegido() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_bloqueado NUMERIC;
BEGIN
    SELECT saldo_bloqueado INTO v_bloqueado FROM ifpe.cuentas WHERE id = NEW.id;
    IF v_bloqueado < (SELECT coalesce(sum(monto), 0) FROM aml.retenciones WHERE cuenta_id = NEW.id AND estado = 'VIGENTE') THEN
        RAISE EXCEPTION 'SC_RETENCION_PROTEGIDA: la retención no puede liberarse por esta vía';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_ifpe_cuentas_zz_aml_retenido
    AFTER UPDATE OF saldo_bloqueado ON ifpe.cuentas
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION aml.fn_retenido_protegido();

-- 8.6 Espejo: CONGELADO_PREVENTIVO <=> un congelamiento ACTIVO del cliente.
CREATE FUNCTION aml.fn_espejo() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_activo BOOLEAN := aml.cliente_congelado(NEW.cliente_id);
    v_estado TEXT := (SELECT estado FROM ifpe.cuentas WHERE cliente_id = NEW.cliente_id AND moneda = 'MXN');
BEGIN
    IF (v_estado = 'CONGELADO_PREVENTIVO' AND NOT v_activo)
       OR (v_activo AND v_estado IS NOT NULL AND v_estado NOT IN ('CONGELADO_PREVENTIVO', 'CONGELADA', 'CERRADA')) THEN
        RAISE EXCEPTION 'SC_AML_ESPEJO: el estado de la cuenta del cliente % (%) no refleja su congelamiento preventivo',
            NEW.cliente_id, v_estado;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_ifpe_cuentas_zz_aml_espejo
    AFTER UPDATE OF estado ON ifpe.cuentas
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION aml.fn_espejo();
CREATE CONSTRAINT TRIGGER trg_aml_congelamientos_zz_espejo
    AFTER INSERT OR UPDATE ON aml.congelamientos
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION aml.fn_espejo();

-- 8.7 Una evaluación en línea describe un renglón del ledger de esta misma
-- transacción (un intento bloqueado no tiene renglón), y los hechos que
-- declara salen de ese renglón: el (sentido, canal) que el tipo de renglón
-- admite, el cliente dueño de la cuenta (o el acreditado), el monto y la
-- clave. Ningún otro renglón admite una evaluación (RETENCION, LIBERACION,
-- RETORNO_DE_SOFOM, reversos 'spei-reverso:', correcciones 'spei-correccion:').
CREATE FUNCTION aml.fn_evaluacion_ligada() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    e_xmin    xid;
    t_xmin    xid;
    t         ifpe.transacciones;
    st        sofom.transacciones;
    v_cliente UUID;
    v_monto   NUMERIC;
    v_clave   TEXT;
    v_canal   TEXT;
BEGIN
    SELECT x.xmin INTO e_xmin FROM aml.evaluaciones x WHERE x.id = NEW.id;
    IF NEW.entidad = 'IFPE' THEN
        SELECT * INTO t FROM ifpe.transacciones WHERE id = NEW.transaccion_id;
        SELECT x.xmin INTO t_xmin FROM ifpe.transacciones x WHERE x.id = NEW.transaccion_id;
    ELSE
        SELECT * INTO st FROM sofom.transacciones WHERE id = NEW.transaccion_id;
        SELECT x.xmin INTO t_xmin FROM sofom.transacciones x WHERE x.id = NEW.transaccion_id;
    END IF;
    IF t_xmin IS NULL OR t_xmin IS DISTINCT FROM e_xmin THEN
        RAISE EXCEPTION 'SC_AML_EVALUACION_HUERFANA: la evaluación % no tiene su movimiento % en esta transacción',
            NEW.id, NEW.transaccion_id;
    END IF;
    IF NEW.entidad = 'IFPE' THEN
        v_monto := t.monto;
        v_clave := t.clave_idempotencia;
        IF t.tipo = 'DEPOSITO' AND starts_with(t.clave_idempotencia, 'spei:') AND NEW.sentido = 'ENTRADA' THEN
            -- Errata E1: sin registro en spei_inbound (lista blanca desactivada) es SPEI_WEBHOOK.
            v_canal := coalesce((SELECT CASE s.origen WHEN 'OPERADOR' THEN 'SPEI_OPERADOR' WHEN 'DEMO' THEN 'SPEI_DEMO'
                                                      ELSE 'SPEI_WEBHOOK' END
                                   FROM ifpe.spei_inbound s WHERE s.transaccion_id = t.id), 'SPEI_WEBHOOK');
            v_cliente := (SELECT cliente_id FROM ifpe.cuentas WHERE id = t.cuenta_destino_id);
        ELSIF t.tipo = 'RETIRO' AND NOT starts_with(t.clave_idempotencia, 'spei-correccion:') AND NEW.sentido = 'SALIDA' THEN
            v_canal := 'SPEI_ENVIADO';
            v_cliente := (SELECT cliente_id FROM ifpe.cuentas WHERE id = t.cuenta_origen_id);
        ELSIF t.tipo = 'TRASPASO_A_SOFOM' AND NEW.sentido = 'SALIDA' THEN
            v_canal := 'INVERSION_MUTUO';
            v_cliente := (SELECT cliente_id FROM ifpe.cuentas WHERE id = t.cuenta_origen_id);
        ELSIF t.tipo = 'TRANSFERENCIA' AND NEW.sentido = 'SALIDA' THEN
            v_canal := 'TRANSFERENCIA_ENVIADA';
            v_cliente := (SELECT cliente_id FROM ifpe.cuentas WHERE id = t.cuenta_origen_id);
        ELSIF t.tipo = 'TRANSFERENCIA' AND NEW.sentido = 'ENTRADA' THEN
            v_canal := 'TRANSFERENCIA_RECIBIDA';
            v_cliente := (SELECT cliente_id FROM ifpe.cuentas WHERE id = t.cuenta_destino_id);
        END IF;
    ELSE
        v_monto := st.monto;
        v_clave := st.clave_idempotencia;
        IF st.tipo = 'DESEMBOLSO_CREDITO' AND NEW.sentido = 'SALIDA' THEN
            v_canal := 'DESEMBOLSO_CREDITO';
        ELSIF st.tipo = 'COBRO_CREDITO' AND NEW.sentido = 'ENTRADA' THEN
            v_canal := 'COBRO_CREDITO';
        END IF;
        v_cliente := (SELECT acreditado_id FROM sofom.creditos WHERE id = st.credito_id);
    END IF;
    IF v_canal IS NULL OR NEW.canal IS DISTINCT FROM v_canal OR NEW.cliente_id IS DISTINCT FROM v_cliente
       OR NEW.monto IS DISTINCT FROM v_monto OR NEW.clave_operacion IS DISTINCT FROM v_clave THEN
        RAISE EXCEPTION 'SC_AML_EVALUACION_INCOHERENTE: la evaluación % no corresponde al movimiento % (%)',
            NEW.id, NEW.transaccion_id, NEW.sentido;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_aml_evaluaciones_zz_ligada
    AFTER INSERT ON aml.evaluaciones
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW WHEN (NEW.origen = 'EN_LINEA' AND NEW.resultado <> 'BLOQUEADA')
    EXECUTE FUNCTION aml.fn_evaluacion_ligada();

-- 8.8 Despacho SPEI: la orden de un cliente congelado que el proveedor
-- seguro no tiene (PENDIENTE, sin ninguna llamada a `registrar` que pudiera
-- haberla registrado: registro_incierto_desde NULL, y fuera de revisión
-- manual) no se reclama ni se registra (RETURN NULL: la fila no cambia).
-- Va más allá del §3.3c original (solo intentos_envio = 0): una orden que se
-- reclamó, cuya llamada probó no llegar al proveedor (NoRecibida), y cuyo
-- cliente se congeló después, también espera. Dos respaldos: el reclamo
-- (incrementa intentos_envio) y la renovación justo antes de `registrar` (fija
-- registro_incierto_desde), así que un congelamiento confirmado entre el
-- reclamo y la primera llamada también la detiene. Nunca falla: un cliente
-- congelado no aborta el lote. Su nombre la ordena antes de
-- trg_spei_outbound_intentos y _proteger.
CREATE FUNCTION aml.fn_despacho_retenido() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF aml.cliente_congelado(NEW.cliente_id) THEN
        RETURN NULL;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_spei_outbound_aml_retenida BEFORE UPDATE OF intentos_envio ON ifpe.spei_outbound
    FOR EACH ROW WHEN (OLD.estado = 'PENDIENTE' AND OLD.registro_incierto_desde IS NULL AND NOT OLD.revision_manual
                       AND NEW.intentos_envio > OLD.intentos_envio)
    EXECUTE FUNCTION aml.fn_despacho_retenido();
CREATE TRIGGER trg_spei_outbound_aml_registro BEFORE UPDATE OF registro_incierto_desde ON ifpe.spei_outbound
    FOR EACH ROW WHEN (OLD.estado = 'PENDIENTE' AND OLD.registro_incierto_desde IS NULL
                       AND NEW.registro_incierto_desde IS NOT NULL)
    EXECUTE FUNCTION aml.fn_despacho_retenido();

-- 8.9 SOFOM: sin desembolso ni activación de crédito para un acreditado congelado.
CREATE FUNCTION aml.fn_guardia_tx_sofom() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF aml.cliente_congelado((SELECT acreditado_id FROM sofom.creditos WHERE id = NEW.credito_id)) THEN
        RAISE EXCEPTION 'SC_OPERACION_NO_DISPONIBLE: la operación no está disponible en este momento'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_sofom_tx_aml_guardia BEFORE INSERT ON sofom.transacciones
    FOR EACH ROW WHEN (NEW.tipo = 'DESEMBOLSO_CREDITO')
    EXECUTE FUNCTION aml.fn_guardia_tx_sofom();

CREATE FUNCTION aml.fn_guardia_credito() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF aml.cliente_congelado(NEW.acreditado_id) THEN
        RAISE EXCEPTION 'SC_OPERACION_NO_DISPONIBLE: la operación no está disponible en este momento'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_creditos_aml_guardia BEFORE UPDATE OF estado ON sofom.creditos
    FOR EACH ROW WHEN (OLD.estado = 'EN_FONDEO' AND NEW.estado = 'VIGENTE')
    EXECUTE FUNCTION aml.fn_guardia_credito();

CREATE FUNCTION aml.fn_cobertura_sofom() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_xmin    xid;
    v_cliente UUID := (SELECT acreditado_id FROM sofom.creditos WHERE id = NEW.credito_id);
BEGIN
    SELECT x.xmin INTO v_xmin FROM sofom.transacciones x WHERE x.id = NEW.id;
    IF NEW.tipo = 'DESEMBOLSO_CREDITO' THEN
        PERFORM aml.fn_cubrir('SOFOM', NEW.id, v_xmin, 'SALIDA', v_cliente, NEW.monto, 'DESEMBOLSO_CREDITO',
                              ARRAY['DESEMBOLSO_CREDITO'], NEW.clave_idempotencia);
    ELSE
        PERFORM aml.fn_cubrir('SOFOM', NEW.id, v_xmin, 'ENTRADA', v_cliente, NEW.monto, 'COBRO_CREDITO',
                              ARRAY['COBRO_CREDITO'], NEW.clave_idempotencia);
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_sofom_tx_zz_aml_cobertura
    AFTER INSERT ON sofom.transacciones
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW WHEN (NEW.tipo IN ('DESEMBOLSO_CREDITO', 'COBRO_CREDITO'))
    EXECUTE FUNCTION aml.fn_cobertura_sofom();

-- 8.10 Lista de Personas Bloqueadas: alerta urgente (24 h) y congelamiento
-- inmediato. Ignora el modo observación (obligación legal).
CREATE FUNCTION aml.fn_lista_bloqueadas() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_alerta UUID;
BEGIN
    PERFORM aml.fijar_actor(NULL);
    PERFORM aml.bloquear_cliente(NEW.id);
    v_alerta := aml.registrar_alerta(NEW.id, 'IFPE', 'NINGUNO', NULL, NULL,
        (SELECT id FROM aml.reglas_riesgo WHERE codigo = 'R09_LISTA_BLOQUEADAS' AND activa ORDER BY version DESC LIMIT 1),
        'LISTA_BLOQUEADAS', 'REGISTRADA', 100, NULL, '{"motivo": "Lista de Personas Bloqueadas"}', TRUE,
        'R09:' || NEW.id, TRUE);
    PERFORM aml.congelar(NEW.id, 'LISTA_BLOQUEADAS', v_alerta, NULL, 100, 'Persona en la Lista de Personas Bloqueadas', NULL);
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_clientes_aml_lista_bloqueadas AFTER UPDATE OF en_lista_bloqueadas ON kyc.clientes
    FOR EACH ROW WHEN (NEW.en_lista_bloqueadas AND NOT OLD.en_lista_bloqueadas)
    EXECUTE FUNCTION aml.fn_lista_bloqueadas();

-- 8.x Nivel de riesgo PLD del cliente (kyc.clientes.nivel_riesgo_pld).
-- Es una entrada de la calificación (R03, bandas por nivel, M03) y CONFIRMAR lo
-- sube a ALTO. Fuera de las funciones de cumplimiento (rol sc_aml) y del dueño
-- del esquema, nadie lo baja mientras el cliente está bajo revisión de PLD: una
-- alerta abierta o confirmada, o un congelamiento activo. El intento no falla
-- (un error le diría al operador que hay un caso de cumplimiento): el nivel se
-- conserva y la bitácora registra RIESGO_CLIENTE_REDUCCION_DENEGADA. Todo otro
-- cambio de nivel fuera de cumplimiento queda en la bitácora
-- (RIESGO_CLIENTE_ELEVADO / RIESGO_CLIENTE_REDUCIDO, con el operador del
-- dictamen como actor; CONFIRMAR deja su propio RIESGO_CLIENTE_ELEVADO).
CREATE FUNCTION aml.rango_nivel_cliente(p TEXT) RETURNS INTEGER
LANGUAGE sql IMMUTABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$ SELECT CASE p WHEN 'BAJO' THEN 1 WHEN 'MEDIO' THEN 2 WHEN 'ALTO' THEN 3 END $$;

CREATE FUNCTION aml.cliente_bajo_revision(p_cliente UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT EXISTS (SELECT 1 FROM aml.alertas
                    WHERE cliente_id = p_cliente AND estado IN ('PENDIENTE', 'EN_REVISION', 'CONFIRMADO'))
        OR aml.cliente_congelado(p_cliente)
$$;

-- Actor de un cambio hecho por el dictamen KYC: el operador que la
-- transacción declaró (sc.operador_id), solo si de verdad es un OPERADOR.
-- Fuera de sc_kyc_verificador, NULL (sistema; el renglón conserva el rol y el
-- usuario de la base de datos).
CREATE FUNCTION aml.operador_de_sesion() RETURNS UUID
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_texto TEXT := current_setting('sc.operador_id', true);
    v_id    UUID;
BEGIN
    IF current_setting('role') <> 'sc_kyc_verificador'
       OR v_texto IS NULL OR v_texto !~ '^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$' THEN
        RETURN NULL;
    END IF;
    v_id := v_texto::uuid;
    IF EXISTS (SELECT 1 FROM kyc.credenciales WHERE cliente_id = v_id AND rol = 'OPERADOR') THEN
        RETURN v_id;
    END IF;
    RETURN NULL;
END
$$;

-- Renglón de bitácora del cambio de nivel con el operador de la sesión como
-- actor, sin pisar el actor de la transacción (un trigger no es punto de
-- entrada: si corre dentro de una función de cumplimiento, el actor sigue).
CREATE FUNCTION aml.registrar_cambio_nivel(p_evento TEXT, p_cliente UUID, p_anterior TEXT, p_nuevo TEXT)
RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_actor TEXT := current_setting('aml.actor', true);
BEGIN
    PERFORM set_config('aml.actor', coalesce(aml.operador_de_sesion()::text, ''), true);
    PERFORM aml.registrar_bitacora(p_evento, 'CLIENTE', p_cliente::text, p_cliente, NULL, NULL, NULL, NULL, p_anterior,
        p_nuevo, NULL, jsonb_build_object('rol_bd', current_setting('role')));
    PERFORM set_config('aml.actor', coalesce(v_actor, ''), true);
END
$$;

-- BEFORE UPDATE: protege el nivel y, si lo único que cambia es el nivel (o su
-- fecha de evaluación), conserva actualizado_en. ClienteVista (/yo, /tablero,
-- /auth/registro) sirve actualizado_en: si CONFIRMAR lo moviera, el cliente
-- vería la hora exacta de la decisión de cumplimiento. El nombre ordena
-- después de trg_clientes_actualizado_en (0001), que ya lo fijó en now().
CREATE FUNCTION aml.fn_cliente_riesgo() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF NEW.nivel_riesgo_pld IS DISTINCT FROM OLD.nivel_riesgo_pld
       AND current_setting('role') NOT IN ('none', 'sc_aml')
       AND aml.rango_nivel_cliente(NEW.nivel_riesgo_pld) < aml.rango_nivel_cliente(OLD.nivel_riesgo_pld)
       AND aml.cliente_bajo_revision(NEW.id) THEN
        PERFORM aml.registrar_cambio_nivel('RIESGO_CLIENTE_REDUCCION_DENEGADA', NEW.id, OLD.nivel_riesgo_pld,
            NEW.nivel_riesgo_pld);
        NEW.nivel_riesgo_pld := OLD.nivel_riesgo_pld;
        NEW.riesgo_evaluado_en := OLD.riesgo_evaluado_en;
    END IF;
    IF (to_jsonb(NEW) - ARRAY['nivel_riesgo_pld', 'riesgo_evaluado_en', 'actualizado_en'])
       = (to_jsonb(OLD) - ARRAY['nivel_riesgo_pld', 'riesgo_evaluado_en', 'actualizado_en']) THEN
        NEW.actualizado_en := OLD.actualizado_en;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_clientes_zz_aml_riesgo BEFORE UPDATE ON kyc.clientes
    FOR EACH ROW EXECUTE FUNCTION aml.fn_cliente_riesgo();

-- AFTER UPDATE: bitácora de los cambios de nivel hechos fuera de cumplimiento
-- (rol de la base de datos y usuario de la sesión quedan en el renglón) y
-- marca de cambio de las alertas del cliente: aml.alerta_json incrusta su
-- nivel, PEP y nombre, así que el sondeo de la consola debe volver a enviarlas.
CREATE FUNCTION aml.fn_cliente_cambiado() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF NEW.nivel_riesgo_pld IS DISTINCT FROM OLD.nivel_riesgo_pld AND current_setting('role') <> 'sc_aml' THEN
        PERFORM aml.registrar_cambio_nivel(
            CASE WHEN aml.rango_nivel_cliente(NEW.nivel_riesgo_pld) > aml.rango_nivel_cliente(OLD.nivel_riesgo_pld)
                 THEN 'RIESGO_CLIENTE_ELEVADO' ELSE 'RIESGO_CLIENTE_REDUCIDO' END,
            NEW.id, OLD.nivel_riesgo_pld, NEW.nivel_riesgo_pld);
    END IF;
    UPDATE aml.alertas SET actualizado_en = clock_timestamp() WHERE cliente_id = NEW.id;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_clientes_zz_aml_cambio AFTER UPDATE ON kyc.clientes
    FOR EACH ROW WHEN (OLD.nivel_riesgo_pld IS DISTINCT FROM NEW.nivel_riesgo_pld OR OLD.es_pep IS DISTINCT FROM NEW.es_pep
                       OR (OLD.nombre, OLD.apellido_paterno, OLD.apellido_materno)
                          IS DISTINCT FROM (NEW.nombre, NEW.apellido_paterno, NEW.apellido_materno))
    EXECUTE FUNCTION aml.fn_cliente_cambiado();

-- 8.y El congelamiento preventivo no deja huella en la proyección del
-- cliente: /ifpe/cuenta y /tablero sirven ifpe.cuentas.actualizado_en, y un
-- cambio de estado sin movimiento del ledger lo movía a la hora exacta del
-- congelamiento o del levantamiento. Si lo único que cambia es el estado entre
-- ACTIVA y CONGELADO_PREVENTIVO, se conserva. El nombre ordena después de
-- trg_ifpe_cuentas_actualizado_en (0001).
CREATE FUNCTION aml.fn_cuenta_estado_discreto() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF OLD.estado IN ('ACTIVA', 'CONGELADO_PREVENTIVO') AND NEW.estado IN ('ACTIVA', 'CONGELADO_PREVENTIVO')
       AND (to_jsonb(NEW) - ARRAY['estado', 'actualizado_en']) = (to_jsonb(OLD) - ARRAY['estado', 'actualizado_en']) THEN
        NEW.actualizado_en := OLD.actualizado_en;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_ifpe_cuentas_zz_aml_actualizado BEFORE UPDATE OF estado ON ifpe.cuentas
    FOR EACH ROW WHEN (OLD.estado IS DISTINCT FROM NEW.estado)
    EXECUTE FUNCTION aml.fn_cuenta_estado_discreto();

-- -----------------------------------------------------------------------------
-- 9. Envolturas en los esquemas transaccionales (hechos de la operación, nunca
--    un veredicto; ninguna regresa datos IFPE a sc_sofom)
-- -----------------------------------------------------------------------------
-- Cada rol transaccional evalúa solo los canales de sus propios caminos del
-- dinero (sc_spei: abonos SPEI; sc_ifpe: transferencias y SPEI enviados;
-- sc_puente: la inversión en mutuo). Un intento bloqueado no tiene renglón del
-- ledger, así que esta es la única barrera para que un rol fabrique un
-- bloqueo (y un congelamiento) en un canal que no opera.
CREATE FUNCTION ifpe.aml_evaluar(p_cliente UUID, p_sentido TEXT, p_canal TEXT, p_monto NUMERIC, p_transaccion_id UUID,
                                 p_clave TEXT, p_contraparte JSONB)
RETURNS TABLE (evaluacion_id UUID, decision TEXT, puntaje NUMERIC, nivel TEXT, monto_retener NUMERIC)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_rol TEXT := current_setting('role');
BEGIN
    IF NOT (v_rol = 'none'
            OR (v_rol = 'sc_spei' AND p_sentido = 'ENTRADA' AND p_canal IN ('SPEI_WEBHOOK', 'SPEI_OPERADOR', 'SPEI_DEMO'))
            OR (v_rol = 'sc_ifpe' AND ((p_sentido = 'SALIDA' AND p_canal IN ('TRANSFERENCIA_ENVIADA', 'SPEI_ENVIADO'))
                                       OR (p_sentido = 'ENTRADA' AND p_canal = 'TRANSFERENCIA_RECIBIDA')))
            OR (v_rol = 'sc_puente' AND p_sentido = 'SALIDA' AND p_canal = 'INVERSION_MUTUO')) THEN
        RAISE EXCEPTION 'SC_AML_PARAMETRO_INVALIDO: el rol % no evalúa operaciones (%, %)', v_rol, p_sentido, p_canal;
    END IF;
    RETURN QUERY SELECT * FROM aml.evaluar_operacion('IFPE', p_cliente, p_sentido, p_canal, p_monto, p_transaccion_id,
                                                     p_clave, p_contraparte);
END
$$;

CREATE FUNCTION ifpe.aml_retener(p_evaluacion UUID) RETURNS UUID
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    RETURN aml.aplicar_retencion(p_evaluacion);
END
$$;

CREATE FUNCTION ifpe.aml_cliente_congelado(p_cliente UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$ SELECT aml.cliente_congelado(p_cliente) $$;

CREATE FUNCTION sofom.aml_bloquear_cliente(p_cliente UUID) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM aml.bloquear_cliente(p_cliente);
END
$$;

CREATE FUNCTION sofom.aml_evaluar(p_cliente UUID, p_sentido TEXT, p_canal TEXT, p_monto NUMERIC, p_transaccion_id UUID,
                                  p_clave TEXT, p_contraparte JSONB)
RETURNS TABLE (evaluacion_id UUID, decision TEXT, puntaje NUMERIC, nivel TEXT, monto_retener NUMERIC)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    RETURN QUERY SELECT * FROM aml.evaluar_operacion('SOFOM', p_cliente, p_sentido, p_canal, p_monto, p_transaccion_id,
                                                     p_clave, p_contraparte);
END
$$;

-- -----------------------------------------------------------------------------
-- 10. Funciones de los oficiales de cumplimiento (EXECUTE: solo sc_aml)
-- -----------------------------------------------------------------------------
-- Contrato común de las funciones de escritura: (1) fijar el actor y exigir un
-- oficial; (2) idempotencia por clave (candado consultivo, huella SHA-256 de
-- todos los parámetros): la misma clave con la misma acción, objeto y huella
-- devuelve el resultado guardado con "idempotente": true, y con otra cosa
-- falla; (3) orden de candados: kyc.clientes FOR NO KEY UPDATE, luego el
-- candado del cliente (aml.bloquear_cliente), luego el renglón objetivo; (4)
-- validación en SQL (textos narrativos, autorresolución); (5) acción,
-- resultado con los constructores JSON y registro en aml.acciones.
CREATE FUNCTION aml.accion_previa(p_clave TEXT, p_accion TEXT, p_objeto UUID, p_huella TEXT) RETURNS JSONB
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    a aml.acciones;
BEGIN
    IF p_clave IS NULL OR length(p_clave) NOT BETWEEN 1 AND 200 THEN
        RAISE EXCEPTION 'SC_AML_PARAMETRO_INVALIDO: falta la clave de idempotencia';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended('aml:accion:' || p_clave, 0));
    SELECT * INTO a FROM aml.acciones WHERE clave_idempotencia = p_clave;
    IF a.id IS NULL THEN
        RETURN NULL;
    END IF;
    IF a.accion = p_accion AND a.objeto_id = p_objeto AND a.huella = p_huella THEN
        RETURN a.resultado || '{"idempotente": true}'::jsonb;
    END IF;
    RAISE EXCEPTION 'SC_CLAVE_IDEMPOTENCIA_REUTILIZADA: la clave de idempotencia ya se usó para otra acción';
END
$$;

CREATE FUNCTION aml.guardar_accion(p_clave TEXT, p_accion TEXT, p_actor UUID, p_objeto UUID, p_huella TEXT,
                                   p_resultado JSONB) RETURNS JSONB
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    INSERT INTO aml.acciones (clave_idempotencia, accion, actor_id, objeto_id, huella, resultado)
    VALUES (p_clave, p_accion, p_actor, p_objeto, p_huella, p_resultado);
    RETURN p_resultado || '{"idempotente": false}'::jsonb;
END
$$;

CREATE FUNCTION aml.resolver_alerta(p_actor UUID, p_alerta UUID, p_accion TEXT, p_dictamen TEXT, p_tipo_reporte TEXT,
                                    p_urgente BOOLEAN, p_congelar BOOLEAN, p_tipo_levantamiento TEXT, p_clave TEXT)
RETURNS JSONB
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    par        aml.parametros := aml.parametros_vigentes();
    v_accion   TEXT := 'RESOLVER_ALERTA:' || coalesce(p_accion, '');
    v_huella   TEXT;
    v_previo   JSONB;
    v_cliente  UUID;
    a          aml.alertas;
    v_dictamen TEXT;
    v_nivel    TEXT;
    v_cong     UUID;
    v_sol      UUID;
BEGIN
    PERFORM aml.fijar_actor(p_actor);
    PERFORM aml.exigir_oficial(p_actor);
    v_huella := nucleo.sha256_hex(jsonb_build_object(
        'alerta', p_alerta, 'accion', p_accion, 'dictamen', p_dictamen, 'tipo_reporte', p_tipo_reporte,
        'urgente', p_urgente, 'congelar', p_congelar, 'tipo_levantamiento', p_tipo_levantamiento)::text);
    v_previo := aml.accion_previa(p_clave, v_accion, p_alerta, v_huella);
    IF v_previo IS NOT NULL THEN
        RETURN v_previo;
    END IF;
    IF p_accion IS NULL OR p_accion NOT IN ('TOMAR', 'LIBERAR', 'ESCALAR', 'CONFIRMAR', 'FALSO_POSITIVO') THEN
        RAISE EXCEPTION 'SC_AML_TRANSICION_INVALIDA: acción % desconocida', p_accion;
    END IF;

    SELECT cliente_id INTO v_cliente FROM aml.alertas WHERE id = p_alerta;
    IF v_cliente IS NULL THEN
        RAISE EXCEPTION 'SC_AML_NO_ENCONTRADO: la alerta % no existe', p_alerta;
    END IF;
    PERFORM 1 FROM kyc.clientes WHERE id = v_cliente FOR NO KEY UPDATE;
    PERFORM aml.bloquear_cliente(v_cliente);
    SELECT * INTO a FROM aml.alertas WHERE id = p_alerta FOR UPDATE;

    IF p_actor = a.cliente_id THEN
        RAISE EXCEPTION 'SC_AML_AUTORESOLUCION: un oficial no resuelve alertas sobre sí mismo';
    END IF;
    IF a.estado IN ('CONFIRMADO', 'FALSO_POSITIVO') THEN
        RAISE EXCEPTION 'SC_AML_ALERTA_CERRADA: la alerta % ya está cerrada (%)', a.folio, a.estado;
    END IF;
    IF p_accion <> 'TOMAR' THEN
        IF a.estado = 'PENDIENTE' THEN
            RAISE EXCEPTION 'SC_AML_TRANSICION_INVALIDA: la alerta % debe tomarse antes de %', a.folio, p_accion;
        END IF;
        IF a.asignada_a IS DISTINCT FROM p_actor THEN
            RAISE EXCEPTION 'SC_AML_ASIGNADA_A_OTRO: la alerta % está asignada a otro oficial', a.folio;
        END IF;
    END IF;
    IF p_accion IN ('ESCALAR', 'CONFIRMAR', 'FALSO_POSITIVO') THEN
        v_dictamen := aml.texto_narrativo(p_dictamen, 30, 'el dictamen');
    END IF;

    CASE p_accion
    WHEN 'TOMAR' THEN
        IF a.estado = 'PENDIENTE' THEN
            UPDATE aml.alertas SET estado = 'EN_REVISION', asignada_a = p_actor, tomada_en = clock_timestamp()
             WHERE id = a.id;
        ELSIF a.asignada_a IS NULL THEN
            -- Alerta heredada EN_REVISION sin oficial asignado.
            UPDATE aml.alertas SET asignada_a = p_actor, tomada_en = clock_timestamp() WHERE id = a.id;
        ELSIF a.asignada_a <> p_actor THEN
            RAISE EXCEPTION 'SC_AML_ASIGNADA_A_OTRO: la alerta % está asignada a otro oficial', a.folio;
        END IF;

    WHEN 'LIBERAR' THEN
        IF a.resolucion_propuesta IS NOT NULL THEN
            RAISE EXCEPTION 'SC_AML_TRANSICION_INVALIDA: la alerta % tiene una propuesta de falso positivo pendiente', a.folio;
        END IF;
        UPDATE aml.alertas SET estado = 'PENDIENTE', asignada_a = NULL, tomada_en = NULL WHERE id = a.id;

    WHEN 'ESCALAR' THEN
        PERFORM aml.fijar_motivo(v_dictamen);
        UPDATE aml.alertas SET escalada = TRUE, nivel_riesgo = 'CRITICO' WHERE id = a.id;
        PERFORM aml.fijar_motivo(NULL);
        IF coalesce(p_congelar, FALSE) AND NOT aml.cliente_congelado(v_cliente) THEN
            PERFORM aml.congelar(v_cliente, 'MANUAL', a.id, NULL, a.puntaje, v_dictamen, p_actor);
        END IF;

    WHEN 'CONFIRMAR' THEN
        IF p_tipo_reporte IS NOT NULL AND p_tipo_reporte NOT IN ('INUSUAL', 'INTERNA_PREOCUPANTE') THEN
            RAISE EXCEPTION 'SC_AML_TRANSICION_INVALIDA: tipo de reporte % inválido', p_tipo_reporte;
        END IF;
        -- Errata E2: las propuestas de falso positivo del cliente se retiran
        -- antes de confirmar (ck_aml_propuesta solo las admite EN_REVISION).
        UPDATE aml.alertas SET resolucion_propuesta = NULL, propuesta_por = NULL, dictamen_propuesto = NULL,
                               propuesta_en = NULL, solicitud_propuesta_id = NULL
         WHERE cliente_id = v_cliente AND resolucion_propuesta IS NOT NULL;
        UPDATE aml.alertas
           SET estado = 'CONFIRMADO', dictamen = v_dictamen, resuelta_por = p_actor, resuelta_en = clock_timestamp(),
               tipo_reporte = coalesce(p_tipo_reporte, tipo_reporte),
               urgente = urgente OR coalesce(p_urgente, FALSE),
               fecha_limite_reporte = CASE WHEN coalesce(p_urgente, FALSE)
                                           THEN LEAST(fecha_limite_reporte,
                                                      clock_timestamp() + par.horas_reporte_urgente * interval '1 hour')
                                           ELSE fecha_limite_reporte END
         WHERE id = a.id;
        -- El cliente pasa a riesgo alto (diligencia reforzada).
        SELECT nivel_riesgo_pld INTO v_nivel FROM kyc.clientes WHERE id = v_cliente;
        IF v_nivel <> 'ALTO' THEN
            UPDATE kyc.clientes SET nivel_riesgo_pld = 'ALTO', riesgo_evaluado_en = now() WHERE id = v_cliente;
            PERFORM aml.registrar_bitacora('RIESGO_CLIENTE_ELEVADO', 'CLIENTE', v_cliente::text, v_cliente, a.id, NULL, NULL,
                NULL, v_nivel, 'ALTO', NULL, jsonb_build_object('alerta_folio', a.folio));
        END IF;
        SELECT id INTO v_cong FROM aml.congelamientos WHERE cliente_id = v_cliente AND estado = 'ACTIVO' FOR UPDATE;
        IF v_cong IS NOT NULL THEN
            UPDATE aml.congelamientos SET confirmado = TRUE WHERE id = v_cong AND NOT confirmado;
            SELECT id INTO v_sol FROM aml.solicitudes_levantamiento
             WHERE congelamiento_id = v_cong AND estado = 'PENDIENTE' FOR UPDATE;
            IF v_sol IS NOT NULL THEN
                UPDATE aml.solicitudes_levantamiento
                   SET estado = 'ANULADA', decidida_en = clock_timestamp(),
                       comentario = format('Anulada por confirmación de la alerta %s', a.folio)
                 WHERE id = v_sol;
            END IF;
        ELSIF coalesce(p_congelar, FALSE) THEN
            v_cong := aml.congelar(v_cliente, 'MANUAL', a.id, NULL, a.puntaje, v_dictamen, p_actor);
        END IF;

    WHEN 'FALSO_POSITIVO' THEN
        IF p_tipo_levantamiento IS NOT NULL
           AND p_tipo_levantamiento NOT IN ('LEVANTAR_Y_LIBERAR', 'LEVANTAR_CONSERVANDO_RETENCIONES') THEN
            RAISE EXCEPTION 'SC_AML_TRANSICION_INVALIDA: tipo de levantamiento % inválido', p_tipo_levantamiento;
        END IF;
        SELECT id INTO v_cong FROM aml.congelamientos WHERE cliente_id = v_cliente AND estado = 'ACTIVO' FOR UPDATE;
        IF v_cong IS NULL THEN
            UPDATE aml.alertas
               SET estado = 'FALSO_POSITIVO', dictamen = v_dictamen, resuelta_por = p_actor, resuelta_en = clock_timestamp()
             WHERE id = a.id;
        ELSE
            -- Con el cliente congelado, el falso positivo (y su marca de
            -- supresión) solo surte efecto con la aprobación de un segundo
            -- oficial: la propuesta queda ligada a la solicitud de levantamiento
            -- (la que hay o una nueva) y fechada; solo esa solicitud la resuelve.
            SELECT id INTO v_sol FROM aml.solicitudes_levantamiento WHERE congelamiento_id = v_cong AND estado = 'PENDIENTE';
            IF v_sol IS NULL THEN
                INSERT INTO aml.solicitudes_levantamiento (congelamiento_id, tipo, justificacion, solicitada_por)
                VALUES (v_cong, coalesce(p_tipo_levantamiento, 'LEVANTAR_Y_LIBERAR'), v_dictamen, p_actor)
                RETURNING id INTO v_sol;
            END IF;
            UPDATE aml.alertas
               SET resolucion_propuesta = 'FALSO_POSITIVO', propuesta_por = p_actor, dictamen_propuesto = v_dictamen,
                   propuesta_en = clock_timestamp(), solicitud_propuesta_id = v_sol
             WHERE id = a.id;
        END IF;
    END CASE;

    v_cong := coalesce((SELECT id FROM aml.congelamientos WHERE cliente_id = v_cliente AND estado = 'ACTIVO'),
                       (SELECT congelamiento_id FROM aml.alertas WHERE id = a.id), v_cong);
    v_sol := coalesce(v_sol, (SELECT id FROM aml.solicitudes_levantamiento
                               WHERE congelamiento_id = v_cong AND estado = 'PENDIENTE'));
    RETURN aml.guardar_accion(p_clave, v_accion, p_actor, p_alerta, v_huella, jsonb_build_object(
        'alerta', aml.alerta_json(a.id), 'congelamiento', aml.congelamiento_json(v_cong),
        'solicitud', aml.solicitud_json(v_sol)));
END
$$;

CREATE FUNCTION aml.registrar_reporte_uif(p_actor UUID, p_alerta UUID, p_folio TEXT, p_reportada_en DATE, p_clave TEXT)
RETURNS JSONB
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_huella TEXT;
    v_previo JSONB;
    a        aml.alertas;
BEGIN
    PERFORM aml.fijar_actor(p_actor);
    PERFORM aml.exigir_oficial(p_actor);
    v_huella := nucleo.sha256_hex(jsonb_build_object('alerta', p_alerta, 'folio_uif', p_folio,
                                                     'reportada_en', p_reportada_en)::text);
    v_previo := aml.accion_previa(p_clave, 'REPORTE_UIF', p_alerta, v_huella);
    IF v_previo IS NOT NULL THEN
        RETURN v_previo;
    END IF;
    SELECT * INTO a FROM aml.alertas WHERE id = p_alerta FOR UPDATE;
    IF a.id IS NULL THEN
        RAISE EXCEPTION 'SC_AML_NO_ENCONTRADO: la alerta % no existe', p_alerta;
    END IF;
    IF p_actor = a.cliente_id THEN
        RAISE EXCEPTION 'SC_AML_AUTORESOLUCION: un oficial no reporta alertas sobre sí mismo';
    END IF;
    IF a.estado <> 'CONFIRMADO' THEN
        RAISE EXCEPTION 'SC_AML_TRANSICION_INVALIDA: solo se reporta a la UIF una alerta CONFIRMADO (la % está %)',
            a.folio, a.estado;
    END IF;
    IF a.folio_uif IS NOT NULL THEN
        RAISE EXCEPTION 'SC_AML_FOLIO_REGISTRADO: la alerta % ya tiene folio UIF', a.folio;
    END IF;
    IF p_folio IS NULL OR p_folio !~ '^[A-Z0-9-]{4,40}$' THEN
        RAISE EXCEPTION 'SC_AML_DICTAMEN_INVALIDO: el folio UIF no tiene el formato esperado';
    END IF;
    IF p_reportada_en IS NULL
       OR p_reportada_en < (a."timestamp" AT TIME ZONE 'America/Mexico_City')::date
       OR p_reportada_en > (clock_timestamp() AT TIME ZONE 'America/Mexico_City')::date THEN
        RAISE EXCEPTION 'SC_AML_DICTAMEN_INVALIDO: la fecha del reporte debe estar entre la fecha de la alerta y hoy';
    END IF;
    UPDATE aml.alertas SET folio_uif = p_folio, reportada_uif_en = p_reportada_en WHERE id = a.id;
    RETURN aml.guardar_accion(p_clave, 'REPORTE_UIF', p_actor, p_alerta, v_huella,
                              jsonb_build_object('alerta', aml.alerta_json(a.id)));
END
$$;

-- Congelamiento manual (un solo oficial puede endurecer). Errata E3: el
-- resultado lleva "ya_congelado".
CREATE FUNCTION aml.congelar_manual(p_actor UUID, p_cliente UUID, p_motivo TEXT, p_alerta UUID, p_clave TEXT)
RETURNS JSONB
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_huella TEXT;
    v_previo JSONB;
    v_motivo TEXT;
    v_cong   UUID;
    v_alerta UUID;
    a        aml.alertas;
BEGIN
    PERFORM aml.fijar_actor(p_actor);
    PERFORM aml.exigir_oficial(p_actor);
    v_huella := nucleo.sha256_hex(jsonb_build_object('cliente', p_cliente, 'motivo', p_motivo, 'alerta', p_alerta)::text);
    v_previo := aml.accion_previa(p_clave, 'CONGELAR_MANUAL', p_cliente, v_huella);
    IF v_previo IS NOT NULL THEN
        RETURN v_previo;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM kyc.clientes WHERE id = p_cliente) THEN
        RAISE EXCEPTION 'SC_AML_NO_ENCONTRADO: el cliente % no existe', p_cliente;
    END IF;
    IF p_actor = p_cliente THEN
        RAISE EXCEPTION 'SC_AML_AUTORESOLUCION: un oficial no se congela a sí mismo';
    END IF;
    v_motivo := aml.texto_narrativo(p_motivo, 30, 'el motivo');
    PERFORM 1 FROM kyc.clientes WHERE id = p_cliente FOR NO KEY UPDATE;
    PERFORM aml.bloquear_cliente(p_cliente);

    SELECT id INTO v_cong FROM aml.congelamientos WHERE cliente_id = p_cliente AND estado = 'ACTIVO';
    IF v_cong IS NOT NULL THEN
        RETURN aml.guardar_accion(p_clave, 'CONGELAR_MANUAL', p_actor, p_cliente, v_huella, jsonb_build_object(
            'congelamiento', aml.congelamiento_json(v_cong),
            'alerta', aml.alerta_json((SELECT alerta_id FROM aml.congelamientos WHERE id = v_cong)),
            'ya_congelado', TRUE));
    END IF;

    IF p_alerta IS NOT NULL THEN
        SELECT * INTO a FROM aml.alertas WHERE id = p_alerta FOR UPDATE;
        IF a.id IS NULL OR a.cliente_id <> p_cliente OR a.estado NOT IN ('PENDIENTE', 'EN_REVISION') THEN
            RAISE EXCEPTION 'SC_AML_TRANSICION_INVALIDA: la alerta % no es una alerta abierta del cliente', p_alerta;
        END IF;
        v_alerta := a.id;
    ELSE
        v_alerta := aml.registrar_alerta(p_cliente, 'IFPE', 'NINGUNO', NULL, NULL,
            (SELECT id FROM aml.reglas_riesgo WHERE codigo = 'R30_CONGELAMIENTO_MANUAL' AND activa
              ORDER BY version DESC LIMIT 1),
            'MANUAL', 'REGISTRADA', 100, NULL, jsonb_build_object('motivo', v_motivo), FALSE,
            'R30:' || gen_random_uuid(), FALSE);
    END IF;
    v_cong := aml.congelar(p_cliente, 'MANUAL', v_alerta, NULL, 100, v_motivo, p_actor);
    RETURN aml.guardar_accion(p_clave, 'CONGELAR_MANUAL', p_actor, p_cliente, v_huella, jsonb_build_object(
        'congelamiento', aml.congelamiento_json(v_cong), 'alerta', aml.alerta_json(v_alerta), 'ya_congelado', FALSE));
END
$$;

CREATE FUNCTION aml.solicitar_levantamiento(p_actor UUID, p_congelamiento UUID, p_tipo TEXT, p_justificacion TEXT,
                                            p_clave TEXT) RETURNS JSONB
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_huella  TEXT;
    v_previo  JSONB;
    v_cliente UUID;
    v_just    TEXT;
    f         aml.congelamientos;
    v_sol     UUID;
BEGIN
    PERFORM aml.fijar_actor(p_actor);
    PERFORM aml.exigir_oficial(p_actor);
    v_huella := nucleo.sha256_hex(jsonb_build_object('congelamiento', p_congelamiento, 'tipo', p_tipo,
                                                     'justificacion', p_justificacion)::text);
    v_previo := aml.accion_previa(p_clave, 'SOLICITAR_LEVANTAMIENTO', p_congelamiento, v_huella);
    IF v_previo IS NOT NULL THEN
        RETURN v_previo;
    END IF;
    SELECT cliente_id INTO v_cliente FROM aml.congelamientos WHERE id = p_congelamiento;
    IF v_cliente IS NULL THEN
        RAISE EXCEPTION 'SC_AML_NO_ENCONTRADO: el congelamiento % no existe', p_congelamiento;
    END IF;
    IF p_actor = v_cliente THEN
        RAISE EXCEPTION 'SC_AML_AUTORESOLUCION: un oficial no solicita levantar su propio congelamiento';
    END IF;
    v_just := aml.texto_narrativo(p_justificacion, 30, 'la justificación');
    IF p_tipo IS NULL OR p_tipo NOT IN ('LEVANTAR_Y_LIBERAR', 'LEVANTAR_CONSERVANDO_RETENCIONES', 'LIBERAR_RETENCIONES') THEN
        RAISE EXCEPTION 'SC_AML_TRANSICION_INVALIDA: tipo de solicitud % inválido', p_tipo;
    END IF;
    PERFORM aml.bloquear_cliente(v_cliente);
    SELECT * INTO f FROM aml.congelamientos WHERE id = p_congelamiento FOR UPDATE;
    IF EXISTS (SELECT 1 FROM aml.solicitudes_levantamiento WHERE congelamiento_id = f.id AND estado = 'PENDIENTE') THEN
        RAISE EXCEPTION 'SC_AML_SOLICITUD_PENDIENTE: el congelamiento % ya tiene una solicitud pendiente', f.folio;
    END IF;
    IF p_tipo <> 'LIBERAR_RETENCIONES' AND f.estado <> 'ACTIVO' THEN
        RAISE EXCEPTION 'SC_AML_CONGELAMIENTO_NO_ACTIVO: el congelamiento % ya fue levantado', f.folio;
    END IF;
    IF p_tipo = 'LIBERAR_RETENCIONES' AND (f.estado <> 'LEVANTADO' OR NOT EXISTS (
           SELECT 1 FROM aml.retenciones WHERE congelamiento_id = f.id AND estado = 'VIGENTE')) THEN
        RAISE EXCEPTION 'SC_AML_SIN_RETENCIONES: el congelamiento % no está levantado con retenciones vigentes', f.folio;
    END IF;
    INSERT INTO aml.solicitudes_levantamiento (congelamiento_id, tipo, justificacion, solicitada_por)
    VALUES (f.id, p_tipo, v_just, p_actor)
    RETURNING id INTO v_sol;
    RETURN aml.guardar_accion(p_clave, 'SOLICITAR_LEVANTAMIENTO', p_actor, p_congelamiento, v_huella, jsonb_build_object(
        'solicitud', aml.solicitud_json(v_sol), 'congelamiento', aml.congelamiento_json(f.id)));
END
$$;

-- Decisión de cuatro ojos sobre un levantamiento o una liberación.
CREATE FUNCTION aml.decidir_levantamiento(p_actor UUID, p_solicitud UUID, p_aprobar BOOLEAN, p_comentario TEXT,
                                          p_clave TEXT) RETURNS JSONB
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_huella      TEXT;
    v_previo      JSONB;
    v_cliente     UUID;
    v_comentario  TEXT;
    s             aml.solicitudes_levantamiento;
    f             aml.congelamientos;
    r             aml.retenciones;
    v_tx          UUID;
    v_resueltas   UUID[] := '{}';
    v_liberaciones JSONB := '[]';
BEGIN
    PERFORM aml.fijar_actor(p_actor);
    PERFORM aml.exigir_oficial(p_actor);
    v_huella := nucleo.sha256_hex(jsonb_build_object('solicitud', p_solicitud, 'aprobar', p_aprobar,
                                                     'comentario', p_comentario)::text);
    v_previo := aml.accion_previa(p_clave, 'DECIDIR_LEVANTAMIENTO', p_solicitud, v_huella);
    IF v_previo IS NOT NULL THEN
        RETURN v_previo;
    END IF;
    SELECT c.cliente_id INTO v_cliente
      FROM aml.solicitudes_levantamiento x JOIN aml.congelamientos c ON c.id = x.congelamiento_id
     WHERE x.id = p_solicitud;
    IF v_cliente IS NULL THEN
        RAISE EXCEPTION 'SC_AML_NO_ENCONTRADO: la solicitud % no existe', p_solicitud;
    END IF;
    PERFORM 1 FROM kyc.clientes WHERE id = v_cliente FOR NO KEY UPDATE;
    PERFORM aml.bloquear_cliente(v_cliente);
    SELECT * INTO s FROM aml.solicitudes_levantamiento WHERE id = p_solicitud FOR UPDATE;
    SELECT * INTO f FROM aml.congelamientos WHERE id = s.congelamiento_id FOR UPDATE;

    IF s.estado <> 'PENDIENTE' THEN
        RAISE EXCEPTION 'SC_AML_SOLICITUD_NO_PENDIENTE: la solicitud % ya fue decidida (%)', s.folio, s.estado;
    END IF;
    IF p_actor = v_cliente THEN
        RAISE EXCEPTION 'SC_AML_AUTORESOLUCION: un oficial no decide sobre su propio congelamiento';
    END IF;
    IF p_actor = s.solicitada_por OR EXISTS (
           SELECT 1 FROM aml.alertas
            WHERE cliente_id = v_cliente AND resolucion_propuesta IS NOT NULL AND propuesta_por = p_actor) THEN
        RAISE EXCEPTION 'SC_AML_CUATRO_OJOS: la decisión debe tomarla un oficial distinto del que solicitó o propuso';
    END IF;
    v_comentario := aml.texto_narrativo(p_comentario, 10, 'el comentario');
    IF p_aprobar IS NULL THEN
        RAISE EXCEPTION 'SC_AML_TRANSICION_INVALIDA: falta la decisión';
    END IF;

    IF NOT p_aprobar THEN
        UPDATE aml.solicitudes_levantamiento
           SET estado = 'RECHAZADA', decidida_por = p_actor, decidida_en = clock_timestamp(), comentario = v_comentario
         WHERE id = s.id;
        -- Solo se retiran las propuestas ligadas a esta solicitud.
        UPDATE aml.alertas SET resolucion_propuesta = NULL, propuesta_por = NULL, dictamen_propuesto = NULL,
                               propuesta_en = NULL, solicitud_propuesta_id = NULL
         WHERE solicitud_propuesta_id = s.id;
    ELSE
        IF s.tipo IN ('LEVANTAR_Y_LIBERAR', 'LEVANTAR_CONSERVANDO_RETENCIONES') THEN
            IF f.estado <> 'ACTIVO' THEN
                RAISE EXCEPTION 'SC_AML_CONGELAMIENTO_NO_ACTIVO: el congelamiento % ya fue levantado', f.folio;
            END IF;
            IF EXISTS (SELECT 1 FROM aml.alertas
                        WHERE cliente_id = v_cliente AND estado IN ('PENDIENTE', 'EN_REVISION')
                          AND nivel_riesgo IN ('ALTO', 'CRITICO') AND solicitud_propuesta_id IS DISTINCT FROM s.id) THEN
                RAISE EXCEPTION 'SC_AML_ALERTAS_ABIERTAS: el cliente tiene alertas ALTO o CRITICO abiertas sin propuesta de resolución';
            END IF;
            -- Ninguna propuesta se aprueba sobre evidencia que llegó después de proponerla.
            IF EXISTS (SELECT 1 FROM aml.alertas
                        WHERE solicitud_propuesta_id = s.id AND ultima_ocurrencia_en > propuesta_en) THEN
                RAISE EXCEPTION 'SC_AML_ALERTAS_ABIERTAS: una alerta propuesta recibió evidencia nueva después de la propuesta';
            END IF;
        ELSIF f.estado <> 'LEVANTADO'
              OR NOT EXISTS (SELECT 1 FROM aml.retenciones WHERE congelamiento_id = f.id AND estado = 'VIGENTE') THEN
            RAISE EXCEPTION 'SC_AML_SIN_RETENCIONES: el congelamiento % no tiene retenciones vigentes que liberar', f.folio;
        END IF;

        -- 1. La solicitud queda aprobada (los triggers de protección exigen
        --    esta aprobación en la misma transacción).
        UPDATE aml.solicitudes_levantamiento
           SET estado = 'APROBADA', decidida_por = p_actor, decidida_en = clock_timestamp(), comentario = v_comentario
         WHERE id = s.id;
        -- 2. Los falsos positivos propuestos bajo esta solicitud de
        --    levantamiento surten efecto (marca de supresión). Una liberación de
        --    retenciones de otro congelamiento no resuelve ninguna propuesta.
        IF s.tipo IN ('LEVANTAR_Y_LIBERAR', 'LEVANTAR_CONSERVANDO_RETENCIONES') THEN
            WITH u AS (
                UPDATE aml.alertas
                   SET estado = 'FALSO_POSITIVO', dictamen = dictamen_propuesto, resuelta_por = propuesta_por,
                       resuelta_en = clock_timestamp(), aprobada_por = p_actor,
                       resolucion_propuesta = NULL, propuesta_por = NULL, dictamen_propuesto = NULL,
                       propuesta_en = NULL, solicitud_propuesta_id = NULL
                 WHERE solicitud_propuesta_id = s.id AND resolucion_propuesta = 'FALSO_POSITIVO'
                RETURNING id, folio)
            SELECT coalesce(array_agg(id ORDER BY folio), '{}') INTO v_resueltas FROM u;
        END IF;
        -- 3. Levantamiento: el congelamiento y el estado de la cuenta.
        IF s.tipo IN ('LEVANTAR_Y_LIBERAR', 'LEVANTAR_CONSERVANDO_RETENCIONES') THEN
            UPDATE aml.congelamientos
               SET estado = 'LEVANTADO', levantado_por = p_actor, levantado_en = clock_timestamp(), solicitud_id = s.id
             WHERE id = f.id;
            UPDATE ifpe.cuentas SET estado = 'ACTIVA' WHERE cliente_id = v_cliente AND estado = 'CONGELADO_PREVENTIVO';
        END IF;
        -- 4. Liberación de las retenciones vigentes del congelamiento.
        IF s.tipo IN ('LEVANTAR_Y_LIBERAR', 'LIBERAR_RETENCIONES') THEN
            FOR r IN SELECT * FROM aml.retenciones WHERE congelamiento_id = f.id AND estado = 'VIGENTE'
                      ORDER BY creado_en, id LOOP
                v_tx := aml.liberar_retencion(r.id, s.id);
                v_liberaciones := v_liberaciones || jsonb_build_array(jsonb_build_object(
                    'retencion_id', r.id, 'transaccion_id', v_tx, 'monto', r.monto::text));
            END LOOP;
        END IF;
    END IF;

    RETURN aml.guardar_accion(p_clave, 'DECIDIR_LEVANTAMIENTO', p_actor, p_solicitud, v_huella, jsonb_build_object(
        'solicitud', aml.solicitud_json(s.id), 'congelamiento', aml.congelamiento_json(f.id),
        'alertas_resueltas', (SELECT coalesce(jsonb_agg(aml.alerta_json(x.id) ORDER BY x.n), '[]')
                                FROM unnest(v_resueltas) WITH ORDINALITY x (id, n)),
        'liberaciones', v_liberaciones));
END
$$;

-- 10.1 Propuestas de cambio a reglas y parámetros (cuatro ojos). Las claves y
-- los tipos de `cambios` se validan aquí; el resultado se valida antes de
-- insertarlo (un CHECK fallido llegaría como RESTRICCION_VIOLADA).
CREATE FUNCTION aml.validar_cambios(p_tipo TEXT, p_cambios JSONB) RETURNS JSONB
LANGUAGE plpgsql IMMUTABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_numericos TEXT[];
    v_enteros   TEXT[];
    v_booleanos TEXT[];
    v_arreglos  TEXT[];
    k           TEXT;
    v           JSONB;
    v_norm      JSONB := '{}';
    v_elementos JSONB;
BEGIN
    IF p_cambios IS NULL OR jsonb_typeof(p_cambios) <> 'object' OR p_cambios = '{}' THEN
        RAISE EXCEPTION 'SC_AML_CAMBIO_INVALIDO: la propuesta no trae cambios';
    END IF;
    IF p_tipo = 'REGLA' THEN
        v_numericos := ARRAY['puntos', 'umbral_monto', 'banda_inferior', 'volumen_max', 'multiplo_ingreso', 'porcentaje_min'];
        v_enteros := ARRAY['frecuencia_min', 'ventana_horas', 'operaciones_max', 'contrapartes_min', 'dias_antiguedad'];
        v_booleanos := ARRAY['activa', 'permite_congelar'];
        v_arreglos := ARRAY['paises_alto_riesgo', 'entidades_alto_riesgo', 'estados_alto_riesgo'];
    ELSIF p_tipo = 'PARAMETROS' THEN
        v_numericos := ARRAY['umbral_medio', 'umbral_alto', 'umbral_critico'];
        v_enteros := ARRAY['sla_critica_horas', 'sla_congelamiento_horas', 'dias_reporte_inusual', 'horas_reporte_urgente',
                           'dias_aviso_plazo'];
        v_booleanos := ARRAY['modo_observacion'];
        v_arreglos := '{}';
    ELSE
        RAISE EXCEPTION 'SC_AML_CAMBIO_INVALIDO: tipo de propuesta % inválido', p_tipo;
    END IF;
    FOR k, v IN SELECT * FROM jsonb_each(p_cambios) LOOP
        IF k = ANY (v_numericos) THEN
            IF jsonb_typeof(v) <> 'string' OR (v #>> '{}') !~ '^[0-9]{1,10}(\.[0-9]{1,8})?$' THEN
                RAISE EXCEPTION 'SC_AML_CAMBIO_INVALIDO: % debe ser un decimal no negativo como texto', k;
            END IF;
            v_norm := v_norm || jsonb_build_object(k, v);
        ELSIF k = ANY (v_enteros) THEN
            IF jsonb_typeof(v) <> 'number' OR v::text !~ '^[0-9]{1,9}$' THEN
                RAISE EXCEPTION 'SC_AML_CAMBIO_INVALIDO: % debe ser un entero no negativo', k;
            END IF;
            v_norm := v_norm || jsonb_build_object(k, v);
        ELSIF k = ANY (v_booleanos) THEN
            IF jsonb_typeof(v) <> 'boolean' THEN
                RAISE EXCEPTION 'SC_AML_CAMBIO_INVALIDO: % debe ser booleano', k;
            END IF;
            v_norm := v_norm || jsonb_build_object(k, v);
        ELSIF k = ANY (v_arreglos) THEN
            IF jsonb_typeof(v) <> 'array'
               OR EXISTS (SELECT 1 FROM jsonb_array_elements(v) x WHERE jsonb_typeof(x) <> 'string') THEN
                RAISE EXCEPTION 'SC_AML_CAMBIO_INVALIDO: % debe ser un arreglo de textos', k;
            END IF;
            IF k = 'estados_alto_riesgo' THEN
                IF EXISTS (SELECT 1 FROM jsonb_array_elements_text(v) x WHERE length(btrim(x)) NOT BETWEEN 1 AND 80) THEN
                    RAISE EXCEPTION 'SC_AML_CAMBIO_INVALIDO: % trae un nombre de estado inválido', k;
                END IF;
                SELECT coalesce(jsonb_agg(DISTINCT aml.normalizar_texto(x)), '[]') INTO v_elementos
                  FROM jsonb_array_elements_text(v) x;
            ELSE
                IF EXISTS (SELECT 1 FROM jsonb_array_elements_text(v) x WHERE x !~ '^[A-Z]{2}$') THEN
                    RAISE EXCEPTION 'SC_AML_CAMBIO_INVALIDO: % debe traer claves de dos letras mayúsculas', k;
                END IF;
                SELECT coalesce(jsonb_agg(DISTINCT x), '[]') INTO v_elementos FROM jsonb_array_elements_text(v) x;
            END IF;
            v_norm := v_norm || jsonb_build_object(k, v_elementos);
        ELSE
            RAISE EXCEPTION 'SC_AML_CAMBIO_INVALIDO: la clave % no es modificable en una propuesta %', k, p_tipo;
        END IF;
    END LOOP;
    RETURN v_norm;
END
$$;

-- Los CHECK de aml.reglas_riesgo, repetidos para responder con un código propio.
CREATE FUNCTION aml.validar_regla(r aml.reglas_riesgo) RETURNS void
LANGUAGE plpgsql IMMUTABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF r.puntos IS NULL OR r.puntos < 0 OR r.puntos > 100
       OR r.umbral_monto <= 0 OR r.banda_inferior <= 0 OR r.banda_inferior >= 1 OR r.frecuencia_min < 2
       OR r.ventana_horas < 1 OR r.ventana_horas > 2160 OR (r.clase = 'PRIMARIA' AND r.ventana_horas > 24)
       OR r.volumen_max <= 0 OR r.operaciones_max <= 0 OR r.multiplo_ingreso <= 0 OR r.multiplo_ingreso >= 10000
       OR r.porcentaje_min <= 0 OR r.porcentaje_min > 1 OR r.contrapartes_min < 2 OR r.dias_antiguedad <= 0 THEN
        RAISE EXCEPTION 'SC_AML_CAMBIO_INVALIDO: la versión resultante de % queda fuera de los rangos permitidos', r.codigo;
    END IF;
END
$$;

CREATE FUNCTION aml.validar_parametros(p aml.parametros) RETURNS void
LANGUAGE plpgsql IMMUTABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF NOT (0 < p.umbral_medio AND p.umbral_medio < p.umbral_alto AND p.umbral_alto < p.umbral_critico
            AND p.umbral_critico <= 100)
       OR p.sla_critica_horas NOT BETWEEN 1 AND 720 OR p.sla_congelamiento_horas NOT BETWEEN 1 AND 720
       OR p.dias_reporte_inusual NOT BETWEEN 1 AND 365 OR p.horas_reporte_urgente NOT BETWEEN 1 AND 168
       OR p.dias_aviso_plazo NOT BETWEEN 1 AND 60 THEN
        RAISE EXCEPTION 'SC_AML_CAMBIO_INVALIDO: los parámetros resultantes quedan fuera de los rangos permitidos (umbral_medio < umbral_alto < umbral_critico <= 100)';
    END IF;
END
$$;

CREATE FUNCTION aml.proponer_cambio(p_actor UUID, p_tipo TEXT, p_regla_codigo TEXT, p_nivel_cliente TEXT, p_cambios JSONB,
                                    p_justificacion TEXT, p_clave TEXT) RETURNS JSONB
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_huella  TEXT;
    v_previo  JSONB;
    v_objeto  UUID := md5('aml:propuesta:' || coalesce(p_tipo, '') || ':' || coalesce(p_regla_codigo, '') || ':'
                          || coalesce(p_nivel_cliente, ''))::uuid;
    v_just    TEXT;
    v_cambios JSONB;
    r         aml.reglas_riesgo;
    v_id      UUID;
BEGIN
    PERFORM aml.fijar_actor(p_actor);
    PERFORM aml.exigir_oficial(p_actor);
    v_huella := nucleo.sha256_hex(jsonb_build_object('tipo', p_tipo, 'regla_codigo', p_regla_codigo,
        'nivel_cliente', p_nivel_cliente, 'cambios', p_cambios, 'justificacion', p_justificacion)::text);
    v_previo := aml.accion_previa(p_clave, 'PROPONER_CAMBIO', v_objeto, v_huella);
    IF v_previo IS NOT NULL THEN
        RETURN v_previo;
    END IF;
    v_just := aml.texto_narrativo(p_justificacion, 30, 'la justificación');
    v_cambios := aml.validar_cambios(p_tipo, p_cambios);
    IF p_tipo = 'REGLA' THEN
        IF p_regla_codigo IS NULL OR p_nivel_cliente IS NULL THEN
            RAISE EXCEPTION 'SC_AML_CAMBIO_INVALIDO: una propuesta de regla exige su código y nivel';
        END IF;
        IF p_regla_codigo IN ('R00_LEGADO', 'R09_LISTA_BLOQUEADAS', 'R30_CONGELAMIENTO_MANUAL') THEN
            RAISE EXCEPTION 'SC_AML_CAMBIO_INVALIDO: la regla % no se modifica (obligación legal, acción manual o legado)',
                p_regla_codigo;
        END IF;
        -- La versión activa o, si la regla se desactivó, la última versión: una
        -- propuesta puede reactivarla (el interruptor no es de un solo sentido).
        SELECT * INTO r FROM aml.reglas_riesgo WHERE codigo = p_regla_codigo AND nivel_cliente = p_nivel_cliente
         ORDER BY activa DESC, version DESC LIMIT 1;
        IF r.id IS NULL THEN
            RAISE EXCEPTION 'SC_AML_NO_ENCONTRADO: no hay una regla % para el nivel %', p_regla_codigo, p_nivel_cliente;
        END IF;
        PERFORM aml.validar_regla(jsonb_populate_record(r, v_cambios));
    ELSE
        IF p_regla_codigo IS NOT NULL OR p_nivel_cliente IS NOT NULL THEN
            RAISE EXCEPTION 'SC_AML_CAMBIO_INVALIDO: una propuesta de parámetros no lleva regla ni nivel';
        END IF;
        PERFORM aml.validar_parametros(jsonb_populate_record(aml.parametros_vigentes(), v_cambios));
    END IF;
    INSERT INTO aml.propuestas_cambio (tipo, regla_codigo, nivel_cliente, cambios, justificacion, propuesta_por)
    VALUES (p_tipo, p_regla_codigo, p_nivel_cliente, v_cambios, v_just, p_actor)
    RETURNING id INTO v_id;
    RETURN aml.guardar_accion(p_clave, 'PROPONER_CAMBIO', p_actor, v_objeto, v_huella,
                              jsonb_build_object('propuesta', aml.propuesta_json(v_id)));
END
$$;

CREATE FUNCTION aml.decidir_propuesta(p_actor UUID, p_propuesta UUID, p_aprobar BOOLEAN, p_comentario TEXT, p_clave TEXT)
RETURNS JSONB
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_huella     TEXT;
    v_previo     JSONB;
    v_comentario TEXT;
    p            aml.propuestas_cambio;
    r            aml.reglas_riesgo;
    n            aml.reglas_riesgo;
    actual       aml.parametros;
    nueva        aml.parametros;
    v_par_id     BIGINT;
BEGIN
    PERFORM aml.fijar_actor(p_actor);
    PERFORM aml.exigir_oficial(p_actor);
    v_huella := nucleo.sha256_hex(jsonb_build_object('propuesta', p_propuesta, 'aprobar', p_aprobar,
                                                     'comentario', p_comentario)::text);
    v_previo := aml.accion_previa(p_clave, 'DECIDIR_PROPUESTA', p_propuesta, v_huella);
    IF v_previo IS NOT NULL THEN
        RETURN v_previo;
    END IF;
    SELECT * INTO p FROM aml.propuestas_cambio WHERE id = p_propuesta FOR UPDATE;
    IF p.id IS NULL THEN
        RAISE EXCEPTION 'SC_AML_NO_ENCONTRADO: la propuesta % no existe', p_propuesta;
    END IF;
    IF p.estado <> 'PENDIENTE' THEN
        RAISE EXCEPTION 'SC_AML_PROPUESTA_NO_PENDIENTE: la propuesta % ya fue decidida (%)', p.folio, p.estado;
    END IF;
    IF p.propuesta_por = p_actor THEN
        RAISE EXCEPTION 'SC_AML_CUATRO_OJOS: una propuesta la decide un oficial distinto del que la hizo';
    END IF;
    v_comentario := aml.texto_narrativo(p_comentario, 10, 'el comentario');
    IF p_aprobar IS NULL THEN
        RAISE EXCEPTION 'SC_AML_TRANSICION_INVALIDA: falta la decisión';
    END IF;

    IF NOT p_aprobar THEN
        UPDATE aml.propuestas_cambio
           SET estado = 'RECHAZADA', decidida_por = p_actor, decidida_en = clock_timestamp(), comentario = v_comentario
         WHERE id = p.id;
        RETURN aml.guardar_accion(p_clave, 'DECIDIR_PROPUESTA', p_actor, p_propuesta, v_huella, jsonb_build_object(
            'propuesta', aml.propuesta_json(p.id), 'regla', NULL, 'parametros', NULL));
    END IF;

    IF p.tipo = 'REGLA' THEN
        SELECT * INTO r FROM aml.reglas_riesgo
         WHERE codigo = p.regla_codigo AND nivel_cliente = p.nivel_cliente
         ORDER BY activa DESC, version DESC LIMIT 1 FOR UPDATE;
        IF r.id IS NULL THEN
            RAISE EXCEPTION 'SC_AML_NO_ENCONTRADO: no hay una regla % para el nivel %', p.regla_codigo, p.nivel_cliente;
        END IF;
        IF r.codigo IN ('R00_LEGADO', 'R09_LISTA_BLOQUEADAS', 'R30_CONGELAMIENTO_MANUAL') THEN
            RAISE EXCEPTION 'SC_AML_CAMBIO_INVALIDO: la regla % no se modifica', r.codigo;
        END IF;
        n := jsonb_populate_record(r, aml.validar_cambios('REGLA', p.cambios));
        n.id := gen_random_uuid();
        n.version := (SELECT max(version) + 1 FROM aml.reglas_riesgo WHERE codigo = r.codigo AND nivel_cliente = r.nivel_cliente);
        n.propuesta_id := p.id;
        n.creado_en := clock_timestamp();
        n.desactivada_en := CASE WHEN n.activa THEN NULL ELSE clock_timestamp() END;
        PERFORM aml.validar_regla(n);
        UPDATE aml.propuestas_cambio
           SET estado = 'APROBADA', decidida_por = p_actor, decidida_en = clock_timestamp(), comentario = v_comentario,
               resultado_id = n.id::text
         WHERE id = p.id;
        IF r.activa THEN
            UPDATE aml.reglas_riesgo SET activa = FALSE, desactivada_en = clock_timestamp() WHERE id = r.id;
        END IF;
        INSERT INTO aml.reglas_riesgo SELECT (n).*;
        RETURN aml.guardar_accion(p_clave, 'DECIDIR_PROPUESTA', p_actor, p_propuesta, v_huella, jsonb_build_object(
            'propuesta', aml.propuesta_json(p.id), 'regla', aml.regla_json(n.id), 'parametros', NULL));
    END IF;

    actual := aml.parametros_vigentes();
    nueva := jsonb_populate_record(actual, aml.validar_cambios('PARAMETROS', p.cambios));
    PERFORM aml.validar_parametros(nueva);
    v_par_id := nextval(pg_get_serial_sequence('aml.parametros', 'id'));
    UPDATE aml.propuestas_cambio
       SET estado = 'APROBADA', decidida_por = p_actor, decidida_en = clock_timestamp(), comentario = v_comentario,
           resultado_id = v_par_id::text
     WHERE id = p.id;
    INSERT INTO aml.parametros (id, umbral_medio, umbral_alto, umbral_critico, modo_observacion, sla_critica_horas,
                                sla_congelamiento_horas, dias_reporte_inusual, horas_reporte_urgente, dias_aviso_plazo,
                                alertar_desde, propuesta_id)
    OVERRIDING SYSTEM VALUE
    VALUES (v_par_id, nueva.umbral_medio, nueva.umbral_alto, nueva.umbral_critico, nueva.modo_observacion,
            nueva.sla_critica_horas, nueva.sla_congelamiento_horas, nueva.dias_reporte_inusual,
            nueva.horas_reporte_urgente, nueva.dias_aviso_plazo, actual.alertar_desde, p.id);
    RETURN aml.guardar_accion(p_clave, 'DECIDIR_PROPUESTA', p_actor, p_propuesta, v_huella, jsonb_build_object(
        'propuesta', aml.propuesta_json(p.id), 'regla', NULL, 'parametros', aml.parametros_json()));
END
$$;

-- 10.2 Lectura del expediente (deja constancia en la bitácora).
CREATE FUNCTION aml.bitacora_json(b aml.bitacora) RETURNS JSONB
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT jsonb_build_object(
        'id', b.id, 'ocurrido_en', b.ocurrido_en, 'evento', b.evento, 'objeto', b.objeto, 'objeto_id', b.objeto_id,
        'actor', aml.persona_json(b.actor_id), 'actor_tipo', b.actor_tipo, 'estado_anterior', b.estado_anterior,
        'estado_nuevo', b.estado_nuevo, 'motivo', b.motivo, 'datos', b.datos)
$$;

-- Movimientos del cliente con la marca de retención (vista del personal).
CREATE FUNCTION aml.movimientos_json(p_cliente UUID, p_desde TIMESTAMPTZ, p_hasta TIMESTAMPTZ, p_ids UUID[],
                                     p_limite INTEGER, p_recientes_primero BOOLEAN) RETURNS JSONB
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT coalesce(jsonb_agg(jsonb_build_object(
               'entidad', m.entidad, 'transaccion_id', m.transaccion_id, 'sentido', m.sentido, 'canal', m.canal,
               'monto', m.monto::text, 'contraparte', m.contraparte, 'creado_en', m.creado_en,
               'retenido', EXISTS (SELECT 1 FROM aml.retenciones r
                                    WHERE r.transaccion_origen_id = m.transaccion_id AND r.estado = 'VIGENTE'))
             ORDER BY CASE WHEN p_recientes_primero THEN m.creado_en END DESC,
                      CASE WHEN NOT p_recientes_primero THEN m.creado_en END ASC), '[]')
      FROM (SELECT * FROM aml.movimientos_cliente(p_cliente, p_desde, p_hasta, NULL) x
             WHERE p_ids IS NULL OR x.transaccion_id = ANY (p_ids)
             ORDER BY CASE WHEN p_recientes_primero THEN x.creado_en END DESC,
                      CASE WHEN NOT p_recientes_primero THEN x.creado_en END ASC
             LIMIT p_limite) m
$$;

CREATE FUNCTION aml.consultar_alerta(p_actor UUID, p_alerta UUID) RETURNS JSONB
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    a      aml.alertas;
    v_cong UUID;
    v_ids  UUID[];
BEGIN
    PERFORM aml.fijar_actor(p_actor);
    PERFORM aml.exigir_lector(p_actor);
    SELECT * INTO a FROM aml.alertas WHERE id = p_alerta;
    -- Un oficial o auditor nunca ve un expediente del que es sujeto: la misma
    -- respuesta que una alerta inexistente (sin aviso al sujeto).
    IF a.id IS NULL OR a.cliente_id = p_actor THEN
        RAISE EXCEPTION 'SC_AML_NO_ENCONTRADO: la alerta % no existe', p_alerta;
    END IF;
    v_cong := coalesce((SELECT id FROM aml.congelamientos WHERE cliente_id = a.cliente_id AND estado = 'ACTIVO'),
                       a.congelamiento_id);
    v_ids := ARRAY(SELECT x::uuid FROM jsonb_array_elements_text(
                       CASE WHEN jsonb_typeof(a.detalles -> 'transacciones') = 'array'
                            THEN a.detalles -> 'transacciones' ELSE '[]' END) x
                    WHERE x ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$');
    PERFORM aml.registrar_bitacora('CONSULTA_EXPEDIENTE', 'ALERTA', a.id::text, a.cliente_id, NULL, NULL, NULL, NULL,
        NULL, NULL, NULL, jsonb_build_object('folio', a.folio));
    RETURN jsonb_build_object(
        'alerta', aml.alerta_json(a.id) || jsonb_build_object('detalles', a.detalles,
                                                              'dictamen_propuesto', a.dictamen_propuesto,
                                                              'legado_analista', a.legado_analista),
        'evaluaciones', (SELECT coalesce(jsonb_agg(aml.evaluacion_json(e.id) ORDER BY e.creado_en, e.id), '[]')
                           FROM (SELECT x.id, x.creado_en FROM aml.evaluaciones x
                                  WHERE x.id = a.evaluacion_id
                                     OR x.id IN (SELECT ae.evaluacion_id FROM aml.alerta_evaluaciones ae
                                                  WHERE ae.alerta_id = a.id)
                                  ORDER BY x.creado_en, x.id LIMIT 50) e),
        'movimientos', aml.movimientos_json(a.cliente_id, a."timestamp" - interval '30 days', clock_timestamp(), v_ids,
                                            50, FALSE),
        'cliente', aml.cliente_json(a.cliente_id),
        'cuentas', aml.cuentas_json(a.cliente_id),
        'congelamiento', aml.congelamiento_json(v_cong),
        'otras_alertas', (SELECT coalesce(jsonb_agg(aml.alerta_json(o.id) ORDER BY o.folio DESC), '[]')
                            FROM (SELECT x.id, x.folio FROM aml.alertas x
                                   WHERE x.cliente_id = a.cliente_id AND x.id <> a.id
                                   ORDER BY x.folio DESC LIMIT 20) o),
        'historial', (SELECT coalesce(jsonb_agg(aml.bitacora_json(b) ORDER BY b.id DESC), '[]')
                        FROM (SELECT * FROM aml.bitacora x
                               WHERE x.alerta_id = a.id OR (v_cong IS NOT NULL AND x.congelamiento_id = v_cong)
                               ORDER BY x.id DESC LIMIT 100) b));
END
$$;

CREATE FUNCTION aml.consultar_cliente(p_actor UUID, p_cliente UUID) RETURNS JSONB
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_ahora TIMESTAMPTZ := clock_timestamp();
BEGIN
    PERFORM aml.fijar_actor(p_actor);
    PERFORM aml.exigir_lector(p_actor);
    -- Nadie revisa su propio expediente (misma respuesta que un cliente inexistente).
    IF p_cliente = p_actor OR NOT EXISTS (SELECT 1 FROM kyc.clientes WHERE id = p_cliente) THEN
        RAISE EXCEPTION 'SC_AML_NO_ENCONTRADO: el cliente % no existe', p_cliente;
    END IF;
    PERFORM aml.registrar_bitacora('CONSULTA_EXPEDIENTE', 'CLIENTE', p_cliente::text, p_cliente, NULL, NULL, NULL, NULL,
        NULL, NULL, NULL, '{}');
    RETURN jsonb_build_object(
        'cliente', aml.cliente_json(p_cliente),
        'cuentas', aml.cuentas_json(p_cliente),
        'agregados', (SELECT jsonb_object_agg(v.clave, (
                          SELECT jsonb_build_object(
                                     'entradas_n', count(*) FILTER (WHERE m.sentido = 'ENTRADA'),
                                     'entradas_monto', coalesce(sum(m.monto) FILTER (WHERE m.sentido = 'ENTRADA'), 0)::numeric(20, 8)::text,
                                     'salidas_n', count(*) FILTER (WHERE m.sentido = 'SALIDA'),
                                     'salidas_monto', coalesce(sum(m.monto) FILTER (WHERE m.sentido = 'SALIDA'), 0)::numeric(20, 8)::text)
                            FROM aml.movimientos_cliente(p_cliente, v_ahora - v.ventana, v_ahora, NULL) m))
                        FROM (VALUES ('h24', interval '24 hours'), ('d7', interval '7 days'), ('d30', interval '30 days'))
                             v (clave, ventana)),
        'movimientos', aml.movimientos_json(p_cliente, v_ahora - interval '30 days', v_ahora, NULL, 100, TRUE),
        'ordenes_spei_pendientes', (
            SELECT coalesce(jsonb_agg(jsonb_build_object(
                       'id', o.id, 'clave_rastreo', o.clave_rastreo, 'monto', o.monto::text,
                       'beneficiario_clabe', o.beneficiario_clabe::text, 'beneficiario_nombre', o.beneficiario_nombre,
                       'estado', o.estado, 'intentos_envio', o.intentos_envio, 'creado_en', o.creado_en)
                     ORDER BY o.creado_en), '[]')
              FROM ifpe.spei_outbound o WHERE o.cliente_id = p_cliente AND o.estado IN ('PENDIENTE', 'ENVIADA')),
        'mutuos_vigentes', (
            SELECT coalesce(jsonb_agg(jsonb_build_object(
                       'id', m.id, 'folio', m.folio, 'principal', m.principal::text,
                       'fecha_vencimiento', to_char(m.fecha_vencimiento, 'YYYY-MM-DD')) ORDER BY m.folio), '[]')
              FROM sofom.contratos_mutuo m WHERE m.cliente_id = p_cliente AND m.estado = 'VIGENTE'),
        'creditos', (
            SELECT coalesce(jsonb_agg(jsonb_build_object(
                       'id', c.id, 'folio', c.folio, 'principal', c.principal::text,
                       'saldo_insoluto', c.saldo_insoluto::text, 'estado', c.estado,
                       'fecha_vencimiento', to_char(c.fecha_vencimiento, 'YYYY-MM-DD')) ORDER BY c.folio DESC), '[]')
              FROM sofom.creditos c WHERE c.acreditado_id = p_cliente),
        'alertas', (SELECT coalesce(jsonb_agg(aml.alerta_json(x.id) ORDER BY x.folio DESC), '[]')
                      FROM (SELECT id, folio FROM aml.alertas WHERE cliente_id = p_cliente ORDER BY folio DESC LIMIT 50) x),
        'congelamientos', (SELECT coalesce(jsonb_agg(aml.congelamiento_json(f.id) ORDER BY f.congelado_en DESC), '[]')
                             FROM aml.congelamientos f WHERE f.cliente_id = p_cliente),
        'relevantes_n', (SELECT count(*) FROM aml.operaciones_relevantes WHERE cliente_id = p_cliente));
END
$$;

-- Retenciones y liberaciones de PLD del ledger creadas en una transacción de
-- sc_aml, para que el backend verifique su tx_hash (sc_aml no tiene USAGE en
-- ifpe). Solo las ligadas a aml.retenciones (no las del operador).
CREATE FUNCTION aml.movimientos_ledger(p_ids UUID[]) RETURNS SETOF ifpe.transacciones
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT t.* FROM ifpe.transacciones t
     WHERE t.id = ANY (p_ids) AND t.tipo IN ('RETENCION', 'LIBERACION_RETENCION')
       AND (t.clave_idempotencia LIKE 'ret:%' OR t.clave_idempotencia LIKE 'lib:%')
       AND EXISTS (SELECT 1 FROM aml.retenciones r WHERE r.tx_retencion_id = t.id OR r.tx_liberacion_id = t.id)
     ORDER BY t.secuencia
$$;

-- -----------------------------------------------------------------------------
-- 11. Monitoreo por lotes (EXECUTE: sc_aml; verificar_bitacora también sc_auditor_pld)
-- -----------------------------------------------------------------------------
-- Turno de una sola instancia (arrendamiento con ficha) y marcas de agua del
-- ledger. Solo quien tiene la ficha vigente renueva, fija el rango del ciclo,
-- avanza la marca o libera el turno: un ciclo que se pasó de su arrendamiento
-- no toca el turno de la instancia que lo retomó. El arrendamiento dura a lo
-- sumo 600 s (TURNO_SEG del ciclo) y se renueva entre pasos.
CREATE FUNCTION aml.monitoreo_tomar_turno(p_segundos INTEGER)
RETURNS TABLE (turno UUID, ultima_secuencia_ifpe BIGINT, ultima_secuencia_sofom BIGINT)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_segundos IS NULL OR p_segundos NOT BETWEEN 1 AND 600 THEN
        RAISE EXCEPTION 'SC_AML_PARAMETRO_INVALIDO: el turno dura entre 1 y 600 segundos';
    END IF;
    RETURN QUERY
    WITH u AS (
        UPDATE aml.monitoreo_estado m
           SET en_curso_hasta = clock_timestamp() + p_segundos * interval '1 second', turno = gen_random_uuid(),
               rango_hasta_ifpe = NULL, rango_hasta_sofom = NULL
         WHERE m.id AND m.en_curso_hasta < clock_timestamp()
        RETURNING m.turno AS t, m.ultima_secuencia_ifpe AS i, m.ultima_secuencia_sofom AS s)
    SELECT u.t, u.i, u.s FROM u;
END
$$;

CREATE FUNCTION aml.monitoreo_renovar_turno(p_turno UUID, p_segundos INTEGER) RETURNS BOOLEAN
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF p_segundos IS NULL OR p_segundos NOT BETWEEN 1 AND 600 THEN
        RAISE EXCEPTION 'SC_AML_PARAMETRO_INVALIDO: el turno dura entre 1 y 600 segundos';
    END IF;
    UPDATE aml.monitoreo_estado SET en_curso_hasta = clock_timestamp() + p_segundos * interval '1 second'
     WHERE id AND turno = p_turno AND en_curso_hasta > clock_timestamp();
    RETURN FOUND;
END
$$;

CREATE FUNCTION aml.monitoreo_liberar_turno(p_turno UUID, p_resultado JSONB) RETURNS BOOLEAN
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    UPDATE aml.monitoreo_estado
       SET en_curso_hasta = '-infinity', turno = NULL, rango_hasta_ifpe = NULL, rango_hasta_sofom = NULL,
           ultimo_ciclo_en = clock_timestamp(), ultimo_resultado = p_resultado
     WHERE id AND turno = p_turno;
    RETURN FOUND;
END
$$;

-- El renglón de estado con el turno vigente de p_turno (o un error).
CREATE FUNCTION aml.monitoreo_exigir_turno(p_turno UUID) RETURNS aml.monitoreo_estado
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    est aml.monitoreo_estado;
BEGIN
    SELECT * INTO est FROM aml.monitoreo_estado WHERE id AND turno = p_turno AND en_curso_hasta > clock_timestamp();
    IF est.id IS NULL THEN
        RAISE EXCEPTION 'SC_AML_TURNO_PERDIDO: el turno del monitoreo venció o lo tomó otra instancia';
    END IF;
    RETURN est;
END
$$;

-- Hasta dónde revisar en este ciclo (queda registrado con el turno: la marca
-- solo avanza hasta aquí, nunca más allá de las cabezas del ledger ni hacia
-- atrás). Correcto porque las secuencias confirmadas no tienen huecos y
-- llegan en orden de COMMIT: el candado de la cabeza del ledger se mantiene
-- hasta el COMMIT. Si el rango empieza hace más de 24 h (ciclo rezagado), un
-- aviso COBERTURA (a lo más uno por hora): las reglas por cliente se anclan a
-- los movimientos, pero las redes R26/R27 miden ventanas desde ahora.
CREATE FUNCTION aml.monitoreo_rango(p_turno UUID, p_lote INTEGER) RETURNS TABLE (hasta_ifpe BIGINT, hasta_sofom BIGINT)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    est       aml.monitoreo_estado := aml.monitoreo_exigir_turno(p_turno);
    v_lote    INTEGER := LEAST(GREATEST(coalesce(p_lote, 1), 1), 100000);
    v_i       BIGINT;
    v_s       BIGINT;
    v_antiguo TIMESTAMPTZ;
BEGIN
    v_i := GREATEST(est.ultima_secuencia_ifpe,
                    LEAST(coalesce((SELECT max(secuencia) FROM ifpe.transacciones), est.ultima_secuencia_ifpe),
                          est.ultima_secuencia_ifpe + v_lote));
    v_s := GREATEST(est.ultima_secuencia_sofom,
                    LEAST(coalesce((SELECT max(secuencia) FROM sofom.transacciones), est.ultima_secuencia_sofom),
                          est.ultima_secuencia_sofom + v_lote));
    UPDATE aml.monitoreo_estado SET rango_hasta_ifpe = v_i, rango_hasta_sofom = v_s WHERE id AND turno = p_turno;
    v_antiguo := LEAST((SELECT creado_en FROM ifpe.transacciones
                         WHERE secuencia = est.ultima_secuencia_ifpe + 1 AND secuencia <= v_i),
                       (SELECT creado_en FROM sofom.transacciones
                         WHERE secuencia = est.ultima_secuencia_sofom + 1 AND secuencia <= v_s));
    IF v_antiguo < clock_timestamp() - interval '24 hours'
       AND (est.rezago_notificado_en IS NULL OR est.rezago_notificado_en < clock_timestamp() - interval '1 hour') THEN
        PERFORM aml.notificar('COBERTURA', 'ALTA', NULL, NULL, NULL, NULL, NULL,
                              'monitoreo por lotes rezagado: el rango revisado empieza hace más de 24 h');
        UPDATE aml.monitoreo_estado SET rezago_notificado_en = clock_timestamp() WHERE id;
    END IF;
    RETURN QUERY SELECT v_i, v_s;
END
$$;

-- Clientes con movimientos de dinero en el rango del ciclo, cada uno con sus
-- anclas: el instante del último movimiento de cada hora del rango (a lo
-- sumo 48, las más recientes). Las reglas por lotes se evalúan en esos
-- instantes, no en el reloj del ciclo: un ciclo rezagado ve lo mismo que uno
-- puntual.
CREATE FUNCTION aml.monitoreo_candidatos(p_turno UUID) RETURNS TABLE (cliente_id UUID, anclas TIMESTAMPTZ[])
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    est aml.monitoreo_estado := aml.monitoreo_exigir_turno(p_turno);
BEGIN
    IF est.rango_hasta_ifpe IS NULL THEN
        RAISE EXCEPTION 'SC_AML_TURNO_PERDIDO: el ciclo no fijó su rango (aml.monitoreo_rango)';
    END IF;
    RETURN QUERY
    WITH mov AS (
        SELECT c.cliente_id AS cliente, t.creado_en
          FROM ifpe.transacciones t
          JOIN ifpe.cuentas c ON c.id IN (t.cuenta_origen_id, t.cuenta_destino_id)
         WHERE t.secuencia > est.ultima_secuencia_ifpe AND t.secuencia <= est.rango_hasta_ifpe
           AND t.tipo IN ('DEPOSITO', 'TRANSFERENCIA', 'RETIRO', 'TRASPASO_A_SOFOM')
        UNION ALL
        SELECT cr.acreditado_id, t.creado_en
          FROM sofom.transacciones t
          JOIN sofom.creditos cr ON cr.id = t.credito_id
         WHERE t.secuencia > est.ultima_secuencia_sofom AND t.secuencia <= est.rango_hasta_sofom
           AND t.tipo IN ('DESEMBOLSO_CREDITO', 'COBRO_CREDITO')),
    horas AS (SELECT m.cliente, max(m.creado_en) AS ancla FROM mov m GROUP BY m.cliente, date_trunc('hour', m.creado_en))
    SELECT h.cliente, (array_agg(h.ancla ORDER BY h.ancla DESC))[1:48]
      FROM horas h GROUP BY h.cliente ORDER BY 1;
END
$$;

-- La marca avanza hasta el rango que registró este turno, nunca hacia atrás.
CREATE FUNCTION aml.monitoreo_avanzar_marca(p_turno UUID) RETURNS BOOLEAN
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    UPDATE aml.monitoreo_estado
       SET ultima_secuencia_ifpe = GREATEST(ultima_secuencia_ifpe, rango_hasta_ifpe),
           ultima_secuencia_sofom = GREATEST(ultima_secuencia_sofom, rango_hasta_sofom),
           rango_hasta_ifpe = NULL, rango_hasta_sofom = NULL
     WHERE id AND turno = p_turno AND en_curso_hasta > clock_timestamp() AND rango_hasta_ifpe IS NOT NULL;
    RETURN FOUND;
END
$$;

-- Un cliente cuyo barrido falló en este ciclo. Al tercer ciclo fallido queda
-- en cuarentena (aviso COBERTURA y bitácora): la marca de agua ya no lo
-- espera y el ciclo lo reintenta aparte. TRUE: el cliente está en cuarentena.
CREATE FUNCTION aml.monitoreo_registrar_fallo(p_turno UUID, p_cliente UUID, p_error TEXT) RETURNS BOOLEAN
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    f aml.monitoreo_fallos;
BEGIN
    PERFORM aml.monitoreo_exigir_turno(p_turno);
    PERFORM aml.fijar_actor(NULL);
    INSERT INTO aml.monitoreo_fallos AS x (cliente_id, ultimo_error)
    VALUES (p_cliente, left(coalesce(p_error, 'error desconocido'), 500))
    ON CONFLICT (cliente_id) DO UPDATE
       SET fallos = x.fallos + 1, ultimo_fallo_en = clock_timestamp(), ultimo_error = EXCLUDED.ultimo_error
    RETURNING * INTO f;
    IF f.fallos >= 3 AND f.cuarentena_desde IS NULL THEN
        UPDATE aml.monitoreo_fallos SET cuarentena_desde = clock_timestamp() WHERE cliente_id = p_cliente
        RETURNING * INTO f;
        PERFORM aml.registrar_bitacora('EVALUACION_FALLIDA', 'CLIENTE', p_cliente::text, p_cliente, NULL, NULL, NULL, NULL,
            NULL, 'CUARENTENA', f.ultimo_error, jsonb_build_object('fallos', f.fallos, 'monitoreo', 'lotes'));
        PERFORM aml.notificar('COBERTURA', 'ALTA', p_cliente, NULL, NULL, NULL, NULL,
                              format('monitoreo por lotes: cliente en cuarentena tras %s ciclos fallidos', f.fallos));
    END IF;
    RETURN f.cuarentena_desde IS NOT NULL;
END
$$;

-- Clientes en cuarentena (el ciclo los reintenta con el reloj del servidor).
CREATE FUNCTION aml.monitoreo_cuarentena() RETURNS SETOF UUID
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$ SELECT cliente_id FROM aml.monitoreo_fallos WHERE cuarentena_desde IS NOT NULL ORDER BY ultimo_fallo_en, cliente_id $$;

-- Reglas por lotes de un cliente (R20-R25) sobre 30 días de su ledger, con la
-- marca de supresión de cada regla (por sentido), evaluadas en cada ancla
-- (los instantes de sus movimientos del rango, aml.monitoreo_candidatos); sin
-- anclas, en aml.reloj(p_ahora). La lectura no toma candados: el candado del
-- cliente (fila IFPE y consultivo, sin esperar) solo cubre el alta de alertas
-- y el congelamiento, así que un barrido largo nunca detiene sus caminos del
-- dinero. -1: el cliente está ocupado; el ciclo lo reintenta.
-- Deduplicación por cliente, regla y semana ISO (Ciudad de México): con una
-- alerta ABIERTA de esa llave no se crea otra (la evidencia nueva sí retira
-- una propuesta de falso positivo pendiente); con una CERRADA, un patrón con
-- evidencia posterior al cierre abre una alerta nueva (llave con el folio de
-- la cerrada) y puede congelar. Volver a correr sin movimientos nuevos no crea nada.
CREATE FUNCTION aml.barrer_cliente(p_cliente UUID, p_ahora TIMESTAMPTZ, p_anclas TIMESTAMPTZ[] DEFAULT NULL)
RETURNS INTEGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    par        aml.parametros := aml.parametros_vigentes();
    v_reloj    TIMESTAMPTZ := clock_timestamp();
    v_anclas   TIMESTAMPTZ[];
    a          TIMESTAMPTZ;
    v_semana   TEXT;
    v_perfil   JSONB;
    v_tier     TEXT;
    v_ingreso  NUMERIC;
    v_mod      RECORD;
    v_m        aml.movimiento[];
    v_w        aml.movimiento[];
    r          aml.reglas_riesgo;
    v_wm       TIMESTAMPTZ;
    v_desde    TIMESTAMPTZ;
    v_n        BIGINT;
    v_dias     BIGINT;
    v_suma     NUMERIC;
    v_e        NUMERIC;
    v_o        NUMERIC;
    v_ids      JSONB;
    v_ultimo   TIMESTAMPTZ;
    v_disparo  BOOLEAN;
    v_evid     JSONB;
    v_disparos JSONB := '{}';
    d          JSONB;
    v_base     TEXT;
    v_clave    TEXT;
    v_prev     aml.alertas;
    v_puntaje  NUMERIC;
    v_detalles JSONB;
    v_alerta   UUID;
    v_creadas  INTEGER := 0;
BEGIN
    PERFORM aml.fijar_actor(NULL);
    -- 1. Anclas (nunca en el futuro del servidor).
    v_anclas := ARRAY(SELECT DISTINCT LEAST(x, v_reloj) FROM unnest(p_anclas) x WHERE x IS NOT NULL ORDER BY 1 DESC LIMIT 48);
    IF cardinality(v_anclas) = 0 THEN
        v_anclas := ARRAY[aml.reloj(p_ahora)];
    END IF;
    v_perfil := aml.perfil_cliente(p_cliente, v_anclas[1]);
    IF v_perfil IS NULL THEN
        RETURN 0;
    END IF;
    v_tier := v_perfil ->> 'nivel_riesgo_pld';
    v_ingreso := (v_perfil ->> 'ingreso_mensual_estimado')::numeric;
    SELECT * INTO v_mod FROM aml.modificadores(v_perfil);
    v_m := ARRAY(SELECT ROW(m.entidad, m.transaccion_id, m.sentido, m.canal, m.monto, m.contraparte,
                            m.creado_en)::aml.movimiento
                   FROM aml.movimientos_cliente(p_cliente,
                                                greatest(v_anclas[cardinality(v_anclas)] - interval '720 hours',
                                                         par.alertar_desde),
                                                v_anclas[1], NULL) m
                  ORDER BY m.creado_en, m.transaccion_id);

    -- 2. Lectura: las reglas en cada ancla (la más reciente primero; por regla
    --    y semana se conserva el disparo del ancla más reciente).
    FOREACH a IN ARRAY v_anclas LOOP
        v_semana := to_char(a AT TIME ZONE 'America/Mexico_City', 'IYYY-IW');
        v_w := ARRAY(SELECT ROW(m.entidad, m.transaccion_id, m.sentido, m.canal, m.monto, m.contraparte,
                                m.creado_en)::aml.movimiento
                       FROM unnest(v_m) m WHERE m.creado_en <= a AND m.creado_en > a - interval '720 hours');

        -- R20: (A) abonos en banda en la ventana, en al menos 3 días distintos;
        -- o (B) abonos bajo el umbral en 30 días que suman el doble del umbral.
        r := aml.regla('R20_ESTRUCTURACION_LENTA', v_tier);
        IF r.id IS NOT NULL AND NOT v_disparos ? (r.codigo || '|' || v_semana) THEN
            v_wm := aml.marca_falso_positivo(p_cliente, r.codigo, 'ENTRADA');
            v_desde := greatest(a - r.ventana_horas * interval '1 hour', v_wm);
            SELECT count(*), count(DISTINCT (m.creado_en AT TIME ZONE 'America/Mexico_City')::date),
                   coalesce(sum(m.monto), 0), coalesce(jsonb_agg(m.transaccion_id ORDER BY m.creado_en), '[]'),
                   max(m.creado_en)
              INTO v_n, v_dias, v_suma, v_ids, v_ultimo
              FROM unnest(v_w) m
             WHERE m.sentido = 'ENTRADA' AND aml.canal_en_banda('ENTRADA', m.canal)
               AND m.monto >= r.banda_inferior * r.umbral_monto AND m.monto < r.umbral_monto AND m.creado_en > v_desde;
            v_disparo := v_n >= r.frecuencia_min AND v_dias >= 3;
            v_evid := jsonb_build_object('variante', 'A', 'n', v_n, 'dias', v_dias, 'monto', v_suma::text, 'desde', v_desde);
            IF NOT v_disparo THEN
                v_desde := greatest(a - interval '720 hours', v_wm);
                SELECT count(*), coalesce(sum(m.monto), 0), coalesce(jsonb_agg(m.transaccion_id ORDER BY m.creado_en), '[]'),
                       max(m.creado_en)
                  INTO v_n, v_suma, v_ids, v_ultimo
                  FROM unnest(v_w) m
                 WHERE m.sentido = 'ENTRADA' AND aml.canal_en_banda('ENTRADA', m.canal) AND m.monto < r.umbral_monto
                   AND m.creado_en > v_desde;
                v_disparo := v_n >= r.operaciones_max AND v_suma >= 2 * r.umbral_monto;
                v_evid := jsonb_build_object('variante', 'B', 'n', v_n, 'monto', v_suma::text, 'desde', v_desde);
            END IF;
            IF v_disparo THEN
                v_disparos := v_disparos || jsonb_build_object(r.codigo || '|' || v_semana, jsonb_build_object(
                    'regla', r.id, 'semana', v_semana, 'monto', v_suma::text, 'ultimo', v_ultimo,
                    'evidencia', v_evid || jsonb_build_object('transacciones', v_ids)));
            END IF;
        END IF;

        -- R21: volumen de entradas de 30 días.
        r := aml.regla('R21_VOLUMEN_MENSUAL', v_tier);
        IF r.id IS NOT NULL AND NOT v_disparos ? (r.codigo || '|' || v_semana) THEN
            v_desde := greatest(a - r.ventana_horas * interval '1 hour',
                                aml.marca_falso_positivo(p_cliente, r.codigo, 'ENTRADA'));
            SELECT coalesce(sum(m.monto), 0), coalesce(jsonb_agg(m.transaccion_id ORDER BY m.creado_en), '[]'),
                   max(m.creado_en)
              INTO v_suma, v_ids, v_ultimo
              FROM unnest(v_w) m WHERE m.sentido = 'ENTRADA' AND m.creado_en > v_desde;
            IF v_suma > r.volumen_max OR (v_ingreso > 0 AND v_suma > r.multiplo_ingreso * v_ingreso) THEN
                v_disparos := v_disparos || jsonb_build_object(r.codigo || '|' || v_semana, jsonb_build_object(
                    'regla', r.id, 'semana', v_semana, 'monto', v_suma::text, 'ultimo', v_ultimo,
                    'evidencia', jsonb_build_object(
                        'entradas', v_suma::text, 'volumen_max', r.volumen_max::text,
                        'ingreso_mensual_estimado', v_ingreso::text, 'desde', v_desde, 'transacciones', v_ids)));
            END IF;
        END IF;

        -- R22 / R23: concentración de entradas y dispersión de salidas.
        r := aml.regla('R22_CONCENTRACION_ENTRADAS', v_tier);
        IF r.id IS NOT NULL AND NOT v_disparos ? (r.codigo || '|' || v_semana) THEN
            v_desde := greatest(a - r.ventana_horas * interval '1 hour',
                                aml.marca_falso_positivo(p_cliente, r.codigo, 'ENTRADA'));
            SELECT count(DISTINCT m.contraparte), coalesce(sum(m.monto), 0),
                   coalesce(jsonb_agg(m.transaccion_id ORDER BY m.creado_en), '[]'), max(m.creado_en)
              INTO v_n, v_suma, v_ids, v_ultimo
              FROM unnest(v_w) m WHERE m.sentido = 'ENTRADA' AND m.contraparte IS NOT NULL AND m.creado_en > v_desde;
            IF v_n >= r.contrapartes_min AND v_suma >= r.umbral_monto THEN
                v_disparos := v_disparos || jsonb_build_object(r.codigo || '|' || v_semana, jsonb_build_object(
                    'regla', r.id, 'semana', v_semana, 'monto', v_suma::text, 'ultimo', v_ultimo,
                    'evidencia', jsonb_build_object(
                        'contrapartes', v_n, 'monto', v_suma::text, 'desde', v_desde, 'transacciones', v_ids)));
            END IF;
        END IF;
        r := aml.regla('R23_DISPERSION_SALIDAS', v_tier);
        IF r.id IS NOT NULL AND NOT v_disparos ? (r.codigo || '|' || v_semana) THEN
            v_desde := greatest(a - r.ventana_horas * interval '1 hour',
                                aml.marca_falso_positivo(p_cliente, r.codigo, 'SALIDA'));
            SELECT count(DISTINCT m.contraparte), coalesce(sum(m.monto), 0),
                   coalesce(jsonb_agg(m.transaccion_id ORDER BY m.creado_en), '[]'), max(m.creado_en)
              INTO v_n, v_suma, v_ids, v_ultimo
              FROM unnest(v_w) m
             WHERE m.sentido = 'SALIDA' AND aml.canal_de_salida(m.canal) AND m.contraparte IS NOT NULL
               AND m.creado_en > v_desde;
            IF v_n >= r.contrapartes_min AND v_suma >= r.umbral_monto THEN
                v_disparos := v_disparos || jsonb_build_object(r.codigo || '|' || v_semana, jsonb_build_object(
                    'regla', r.id, 'semana', v_semana, 'monto', v_suma::text, 'ultimo', v_ultimo,
                    'evidencia', jsonb_build_object(
                        'contrapartes', v_n, 'monto', v_suma::text, 'desde', v_desde, 'transacciones', v_ids)));
            END IF;
        END IF;

        -- R24: paso rápido de fondos en 48 h (solo entradas que abonan la bóveda).
        r := aml.regla('R24_PASO_RAPIDO_48H', v_tier);
        IF r.id IS NOT NULL AND NOT v_disparos ? (r.codigo || '|' || v_semana) THEN
            v_desde := greatest(a - r.ventana_horas * interval '1 hour',
                                aml.marca_falso_positivo(p_cliente, r.codigo, 'SALIDA'));
            SELECT coalesce(sum(m.monto) FILTER (WHERE m.sentido = 'ENTRADA' AND aml.canal_abona_boveda(m.canal)), 0),
                   coalesce(sum(m.monto) FILTER (WHERE m.sentido = 'SALIDA' AND aml.canal_de_salida(m.canal)), 0),
                   coalesce(jsonb_agg(m.transaccion_id ORDER BY m.creado_en)
                                FILTER (WHERE (m.sentido = 'ENTRADA' AND aml.canal_abona_boveda(m.canal))
                                           OR (m.sentido = 'SALIDA' AND aml.canal_de_salida(m.canal))), '[]'),
                   max(m.creado_en) FILTER (WHERE (m.sentido = 'ENTRADA' AND aml.canal_abona_boveda(m.canal))
                                               OR (m.sentido = 'SALIDA' AND aml.canal_de_salida(m.canal)))
              INTO v_e, v_o, v_ids, v_ultimo
              FROM unnest(v_w) m WHERE m.creado_en > v_desde;
            IF v_e >= r.umbral_monto AND v_o >= r.porcentaje_min * v_e THEN
                v_disparos := v_disparos || jsonb_build_object(r.codigo || '|' || v_semana, jsonb_build_object(
                    'regla', r.id, 'semana', v_semana, 'monto', v_o::text, 'ultimo', v_ultimo,
                    'evidencia', jsonb_build_object(
                        'entradas', v_e::text, 'salidas', v_o::text, 'porcentaje_min', r.porcentaje_min::text,
                        'desde', v_desde, 'transacciones', v_ids)));
            END IF;
        END IF;

        -- R25: flujo circular con otro cliente (envía y recibe, cada tramo >=
        -- umbral). Cada tramo cuenta una vez (sin el producto cruzado de las
        -- dos listas): la suma es la evidencia real y el costo es lineal.
        r := aml.regla('R25_CIRCULAR', v_tier);
        IF r.id IS NOT NULL AND NOT v_disparos ? (r.codigo || '|' || v_semana) THEN
            v_desde := greatest(a - r.ventana_horas * interval '1 hour',
                                aml.marca_falso_positivo(p_cliente, r.codigo, 'NINGUNO'));
            WITH s AS (SELECT * FROM unnest(v_w) m
                        WHERE m.sentido = 'SALIDA' AND m.canal = 'TRANSFERENCIA_ENVIADA' AND m.monto >= r.umbral_monto
                          AND m.creado_en > v_desde),
                 e AS (SELECT * FROM unnest(v_w) m
                        WHERE m.sentido = 'ENTRADA' AND m.canal = 'TRANSFERENCIA_RECIBIDA' AND m.monto >= r.umbral_monto
                          AND m.creado_en > v_desde),
                 c AS (SELECT s.contraparte FROM s INTERSECT SELECT e.contraparte FROM e),
                 x AS (SELECT s.* FROM s WHERE s.contraparte IN (SELECT c.contraparte FROM c)
                       UNION ALL
                       SELECT e.* FROM e WHERE e.contraparte IN (SELECT c.contraparte FROM c))
            SELECT (SELECT count(*) FROM c), coalesce(sum(x.monto), 0),
                   coalesce(jsonb_agg(x.transaccion_id ORDER BY x.creado_en), '[]'), max(x.creado_en)
              INTO v_n, v_suma, v_ids, v_ultimo
              FROM x;
            IF v_n >= 1 THEN
                v_disparos := v_disparos || jsonb_build_object(r.codigo || '|' || v_semana, jsonb_build_object(
                    'regla', r.id, 'semana', v_semana, 'monto', v_suma::text, 'ultimo', v_ultimo,
                    'evidencia', jsonb_build_object('contrapartes', v_n, 'desde', v_desde, 'transacciones', v_ids)));
            END IF;
        END IF;
    END LOOP;

    IF v_disparos = '{}' THEN
        DELETE FROM aml.monitoreo_fallos WHERE cliente_id = p_cliente;
        RETURN 0;
    END IF;

    -- 3. Escritura: el candado del cliente (sin esperar) solo para alertar y congelar.
    IF EXISTS (SELECT 1 FROM ifpe.cuentas WHERE cliente_id = p_cliente AND moneda = 'MXN') THEN
        PERFORM 1 FROM ifpe.cuentas WHERE cliente_id = p_cliente AND moneda = 'MXN' FOR NO KEY UPDATE SKIP LOCKED;
        IF NOT FOUND THEN
            RETURN -1;
        END IF;
    END IF;
    IF NOT pg_try_advisory_xact_lock(hashtextextended('aml:cliente:' || p_cliente::text, 0)) THEN
        RETURN -1;
    END IF;

    FOR d IN SELECT value FROM jsonb_each(v_disparos) ORDER BY key LOOP
        SELECT * INTO r FROM aml.reglas_riesgo WHERE id = (d ->> 'regla')::uuid;
        v_base := r.codigo || ':' || p_cliente || ':' || (d ->> 'semana');
        v_ultimo := (d ->> 'ultimo')::timestamptz;
        v_detalles := jsonb_build_object(
            'lote', jsonb_build_object('regla', r.codigo, 'version', r.version, 'semana', d ->> 'semana',
                                       'evidencia', d -> 'evidencia', 'modificadores', v_mod.factores,
                                       'puntos', r.puntos::text),
            'transacciones', d -> 'evidencia' -> 'transacciones');
        SELECT * INTO v_prev FROM aml.alertas
         WHERE cliente_id = p_cliente AND regla_codigo = r.codigo AND origen = 'LOTE'
           AND (clave_deduplicacion = v_base OR starts_with(clave_deduplicacion, v_base || ':'))
         ORDER BY folio DESC LIMIT 1;
        IF v_prev.estado IN ('PENDIENTE', 'EN_REVISION') THEN
            -- Abierta: no se duplica; la evidencia posterior a una propuesta de
            -- falso positivo la retira (el segundo oficial no aprueba a ciegas).
            IF v_prev.resolucion_propuesta IS NOT NULL AND v_ultimo > v_prev.propuesta_en THEN
                PERFORM aml.fijar_motivo('Evidencia nueva del monitoreo por lotes después de la propuesta de falso positivo');
                UPDATE aml.alertas
                   SET ocurrencias = ocurrencias + 1, ultima_ocurrencia_en = clock_timestamp(),
                       detalles = aml.fusionar_detalles(detalles, v_detalles),
                       resolucion_propuesta = NULL, propuesta_por = NULL, dictamen_propuesto = NULL,
                       propuesta_en = NULL, solicitud_propuesta_id = NULL
                 WHERE id = v_prev.id;
                PERFORM aml.fijar_motivo(NULL);
            END IF;
            CONTINUE;
        ELSIF v_prev.id IS NOT NULL THEN
            -- Cerrada: solo un patrón con evidencia posterior al cierre.
            CONTINUE WHEN v_ultimo IS NULL OR v_ultimo <= v_prev.resuelta_en;
            v_clave := v_base || ':' || v_prev.folio;
        ELSE
            v_clave := v_base;
        END IF;
        v_puntaje := LEAST(100, r.puntos + v_mod.puntos);
        v_alerta := aml.registrar_alerta(p_cliente, 'IFPE', CASE WHEN r.sentido IN ('ENTRADA', 'SALIDA') THEN r.sentido
                                                                 ELSE 'NINGUNO' END,
            NULL, NULL, r.id, 'LOTE', 'REGISTRADA', v_puntaje, nullif((d ->> 'monto')::numeric, 0), v_detalles,
            FALSE, v_clave, FALSE);
        v_creadas := v_creadas + 1;
        IF v_puntaje >= par.umbral_critico AND r.permite_congelar AND NOT par.modo_observacion
           AND NOT aml.cliente_congelado(p_cliente) THEN
            PERFORM aml.congelar(p_cliente, 'LOTE', v_alerta, NULL, v_puntaje,
                format('Congelamiento por monitoreo: %s, puntaje %s', r.codigo, v_puntaje), NULL);
        END IF;
    END LOOP;
    DELETE FROM aml.monitoreo_fallos WHERE cliente_id = p_cliente;
    RETURN v_creadas;
END
$$;

-- Redes entre clientes (R26: un ordenante que abona en banda a varios
-- clientes; R27: varios clientes que envían a una misma CLABE). Sin
-- congelamiento; una alerta por cliente, red y semana, con la misma
-- deduplicación que aml.barrer_cliente. Cada cliente cuenta solo sus
-- movimientos posteriores a su marca de falso positivo de la regla.
CREATE FUNCTION aml.barrer_redes(p_ahora TIMESTAMPTZ) RETURNS INTEGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    par       aml.parametros := aml.parametros_vigentes();
    v_ahora   TIMESTAMPTZ := aml.reloj(p_ahora);
    v_semana  TEXT;
    r         aml.reglas_riesgo;
    g         RECORD;
    v_cliente UUID;
    v_base    TEXT;
    v_clave   TEXT;
    v_prev    aml.alertas;
    v_ids     JSONB;
    v_ultimo  TIMESTAMPTZ;
    v_desde   TIMESTAMPTZ;
    v_mod     RECORD;
    v_creadas INTEGER := 0;
BEGIN
    PERFORM aml.fijar_actor(NULL);
    v_semana := to_char(v_ahora AT TIME ZONE 'America/Mexico_City', 'IYYY-IW');

    r := aml.regla('R26_RED_ORDENANTE', 'TODOS');
    IF r.id IS NOT NULL THEN
        v_desde := greatest(v_ahora - r.ventana_horas * interval '1 hour', par.alertar_desde);
        FOR g IN
            SELECT s.cuenta_ordenante AS clave, array_agg(DISTINCT c.cliente_id ORDER BY c.cliente_id) AS clientes,
                   jsonb_agg(jsonb_build_object('tx', s.transaccion_id, 'cliente', c.cliente_id, 'en', s.recibido_en))
                       AS movimientos
              FROM ifpe.spei_inbound s
              JOIN ifpe.cuentas c ON c.id = s.cuenta_id
             WHERE s.estado = 'ABONADO' AND s.cuenta_ordenante IS NOT NULL
               AND s.monto >= r.banda_inferior * r.umbral_monto AND s.monto < r.umbral_monto
               AND s.recibido_en > v_desde AND s.recibido_en <= v_ahora
               AND s.recibido_en > aml.marca_falso_positivo(c.cliente_id, r.codigo, 'ENTRADA')
               AND NOT EXISTS (SELECT 1 FROM ifpe.spei_anulaciones a WHERE a.spei_inbound_id = s.id)
             GROUP BY s.cuenta_ordenante
            HAVING count(DISTINCT c.cliente_id) >= r.contrapartes_min
        LOOP
            FOREACH v_cliente IN ARRAY g.clientes LOOP
                SELECT coalesce(jsonb_agg(m ->> 'tx'), '[]'), max((m ->> 'en')::timestamptz) INTO v_ids, v_ultimo
                  FROM jsonb_array_elements(g.movimientos) m WHERE m ->> 'cliente' = v_cliente::text;
                v_base := 'R26:' || v_cliente || ':' || g.clave || ':' || v_semana;
                SELECT * INTO v_prev FROM aml.alertas
                 WHERE cliente_id = v_cliente AND regla_codigo = r.codigo
                   AND (clave_deduplicacion = v_base OR starts_with(clave_deduplicacion, v_base || ':'))
                 ORDER BY folio DESC LIMIT 1;
                CONTINUE WHEN v_prev.estado IN ('PENDIENTE', 'EN_REVISION');
                CONTINUE WHEN v_prev.id IS NOT NULL AND v_ultimo <= v_prev.resuelta_en;
                v_clave := CASE WHEN v_prev.id IS NULL THEN v_base ELSE v_base || ':' || v_prev.folio END;
                SELECT * INTO v_mod FROM aml.modificadores(aml.perfil_cliente(v_cliente, v_ahora));
                PERFORM aml.registrar_alerta(v_cliente, 'IFPE', 'ENTRADA', NULL, NULL, r.id, 'LOTE', 'REGISTRADA',
                    LEAST(100, r.puntos + v_mod.puntos), NULL,
                    jsonb_build_object('red', jsonb_build_object('clave', g.clave, 'clientes', to_jsonb(g.clientes)),
                                       'lote', jsonb_build_object('regla', r.codigo, 'version', r.version, 'semana', v_semana,
                                                                  'modificadores', v_mod.factores),
                                       'transacciones', v_ids),
                    FALSE, v_clave, FALSE);
                v_creadas := v_creadas + 1;
            END LOOP;
        END LOOP;
    END IF;

    r := aml.regla('R27_RED_BENEFICIARIO', 'TODOS');
    IF r.id IS NOT NULL THEN
        v_desde := greatest(v_ahora - r.ventana_horas * interval '1 hour', par.alertar_desde);
        FOR g IN
            SELECT o.beneficiario_clabe::text AS clave, array_agg(DISTINCT o.cliente_id ORDER BY o.cliente_id) AS clientes,
                   jsonb_agg(jsonb_build_object('tx', o.tx_retiro_id, 'cliente', o.cliente_id, 'en', o.creado_en))
                       AS movimientos
              FROM ifpe.spei_outbound o
             WHERE o.estado <> 'RECHAZADA' AND o.creado_en > v_desde AND o.creado_en <= v_ahora
               AND o.creado_en > aml.marca_falso_positivo(o.cliente_id, r.codigo, 'SALIDA')
             GROUP BY o.beneficiario_clabe
            HAVING count(DISTINCT o.cliente_id) >= r.contrapartes_min AND sum(o.monto) >= r.umbral_monto
        LOOP
            FOREACH v_cliente IN ARRAY g.clientes LOOP
                SELECT coalesce(jsonb_agg(m ->> 'tx'), '[]'), max((m ->> 'en')::timestamptz) INTO v_ids, v_ultimo
                  FROM jsonb_array_elements(g.movimientos) m WHERE m ->> 'cliente' = v_cliente::text;
                v_base := 'R27:' || v_cliente || ':' || g.clave || ':' || v_semana;
                SELECT * INTO v_prev FROM aml.alertas
                 WHERE cliente_id = v_cliente AND regla_codigo = r.codigo
                   AND (clave_deduplicacion = v_base OR starts_with(clave_deduplicacion, v_base || ':'))
                 ORDER BY folio DESC LIMIT 1;
                CONTINUE WHEN v_prev.estado IN ('PENDIENTE', 'EN_REVISION');
                CONTINUE WHEN v_prev.id IS NOT NULL AND v_ultimo <= v_prev.resuelta_en;
                v_clave := CASE WHEN v_prev.id IS NULL THEN v_base ELSE v_base || ':' || v_prev.folio END;
                SELECT * INTO v_mod FROM aml.modificadores(aml.perfil_cliente(v_cliente, v_ahora));
                PERFORM aml.registrar_alerta(v_cliente, 'IFPE', 'SALIDA', NULL, NULL, r.id, 'LOTE', 'REGISTRADA',
                    LEAST(100, r.puntos + v_mod.puntos), NULL,
                    jsonb_build_object('red', jsonb_build_object('clave', g.clave, 'clientes', to_jsonb(g.clientes)),
                                       'lote', jsonb_build_object('regla', r.codigo, 'version', r.version, 'semana', v_semana,
                                                                  'modificadores', v_mod.factores),
                                       'transacciones', v_ids),
                    FALSE, v_clave, FALSE);
                v_creadas := v_creadas + 1;
            END LOOP;
        END LOOP;
    END IF;
    RETURN v_creadas;
END
$$;

-- Marcas DIFERIDA y calificaciones fallidas que esperan su reevaluación.
CREATE FUNCTION aml.marcadores_pendientes(p_lote INTEGER) RETURNS SETOF UUID
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT e.id FROM aml.evaluaciones e
     WHERE e.resultado = 'PENDIENTE_REEVALUACION'
       AND NOT EXISTS (SELECT 1 FROM aml.evaluaciones x WHERE x.evaluacion_origen_id = e.id)
     ORDER BY e.creado_en, e.id
     LIMIT p_lote
$$;

-- Reevaluación de una marca con la foto del ledger en el instante del
-- movimiento. Sin retención: el abono ya es pasado.
CREATE FUNCTION aml.reevaluar(p_evaluacion UUID) RETURNS BOOLEAN
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    par       aml.parametros;
    e         aml.evaluaciones;
    v_creado  TIMESTAMPTZ;
    v_contra  TEXT;
    v         JSONB;
    v_eval    UUID;
    v_alerta  UUID;
    v_puntaje NUMERIC;
BEGIN
    PERFORM aml.fijar_actor(NULL);
    SELECT * INTO e FROM aml.evaluaciones WHERE id = p_evaluacion;
    IF e.id IS NULL OR e.resultado <> 'PENDIENTE_REEVALUACION' THEN
        RAISE EXCEPTION 'SC_AML_PARAMETRO_INVALIDO: la evaluación % no espera reevaluación', p_evaluacion;
    END IF;
    PERFORM aml.bloquear_cliente(e.cliente_id);
    par := aml.parametros_vigentes();
    IF e.entidad = 'IFPE' THEN
        SELECT creado_en INTO v_creado FROM ifpe.transacciones WHERE id = e.transaccion_id;
    ELSE
        SELECT creado_en INTO v_creado FROM sofom.transacciones WHERE id = e.transaccion_id;
    END IF;
    IF v_creado IS NULL THEN
        RAISE EXCEPTION 'SC_AML_EVALUACION_HUERFANA: la evaluación % no tiene su movimiento %', e.id, e.transaccion_id;
    END IF;
    -- La contraparte como la ve el ledger (una marca DIFERIDA no trae hechos).
    SELECT m.contraparte INTO v_contra
      FROM aml.movimientos_cliente(e.cliente_id, v_creado - interval '1 second', v_creado, NULL) m
     WHERE m.transaccion_id = e.transaccion_id AND m.sentido = e.sentido
     LIMIT 1;
    v := aml.calcular(e.cliente_id, e.entidad, e.sentido, e.canal, e.monto, e.transaccion_id, v_creado,
                      coalesce(v_contra, aml.clave_contraparte(e.canal, e.contraparte)));
    v_puntaje := (v ->> 'puntaje')::numeric;
    INSERT INTO aml.evaluaciones (cliente_id, entidad, sentido, canal, monto, transaccion_id, clave_operacion, contraparte,
                                  puntaje, nivel, resultado, origen, evaluacion_origen_id, factores, reglas_aplicadas,
                                  parametros_id)
    VALUES (e.cliente_id, e.entidad, e.sentido, e.canal, e.monto, e.transaccion_id, e.clave_operacion, e.contraparte,
            v_puntaje, (v ->> 'nivel')::aml.nivel_riesgo, 'REEVALUADA', 'REEVALUACION', e.id, v,
            ARRAY(SELECT jsonb_array_elements_text(v -> 'reglas_aplicadas')::uuid), par.id)
    RETURNING id INTO v_eval;
    IF jsonb_array_length(v -> 'primarias') > 0 THEN
        v_alerta := aml.registrar_alerta(e.cliente_id, e.entidad, e.sentido, e.transaccion_id, v_eval,
            (v ->> 'dominante_regla_id')::uuid, 'DIFERIDA', 'REGISTRADA', v_puntaje, e.monto,
            jsonb_build_object(
                'operacion', jsonb_build_object('entidad', e.entidad, 'sentido', e.sentido, 'canal', e.canal,
                                                'monto', e.monto::text, 'transaccion_id', e.transaccion_id,
                                                'contraparte', e.contraparte),
                'evaluacion', v,
                'transacciones', CASE WHEN jsonb_array_length(v -> 'transacciones_patron') > 0
                                      THEN (v -> 'transacciones_patron') || to_jsonb(e.transaccion_id::text)
                                      ELSE jsonb_build_array(e.transaccion_id::text) END),
            FALSE, NULL, TRUE);
        IF (v ->> 'congelar')::boolean AND NOT par.modo_observacion AND NOT aml.cliente_congelado(e.cliente_id) THEN
            PERFORM aml.congelar(e.cliente_id, 'LOTE', v_alerta, v_eval, v_puntaje,
                format('Congelamiento por reevaluación: %s, puntaje %s', v ->> 'dominante', v_puntaje), NULL);
        END IF;
    END IF;
    RETURN TRUE;
END
$$;

-- Avisos de plazos (cada uno una sola vez, por los índices únicos), con el
-- reloj del servidor (aml.reloj): nadie consume un aviso antes de tiempo.
CREATE FUNCTION aml.revisar_plazos(p_ahora TIMESTAMPTZ) RETURNS INTEGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    par     aml.parametros := aml.parametros_vigentes();
    v_ahora TIMESTAMPTZ := aml.reloj(p_ahora);
    a       RECORD;
    f       RECORD;
    n       INTEGER := 0;
BEGIN
    PERFORM aml.fijar_actor(NULL);
    FOR a IN SELECT id, cliente_id, folio, regla_codigo FROM aml.alertas
              WHERE estado = 'PENDIENTE' AND nivel_riesgo = 'CRITICO'
                AND "timestamp" < v_ahora - par.sla_critica_horas * interval '1 hour'
              ORDER BY folio LOOP
        IF aml.notificar('ESCALAMIENTO', 'ALTA', a.cliente_id, a.id, NULL, NULL, NULL,
                         format('%s · CRITICO · alerta %s sin atender', a.regla_codigo, a.folio)) IS NOT NULL THEN
            n := n + 1;
        END IF;
    END LOOP;
    FOR f IN SELECT c.id, c.cliente_id, c.alerta_id, c.folio FROM aml.congelamientos c
              WHERE c.estado = 'ACTIVO' AND c.sla_vence_en < v_ahora
                AND NOT EXISTS (SELECT 1 FROM aml.solicitudes_levantamiento s
                                 WHERE s.congelamiento_id = c.id AND s.estado = 'PENDIENTE')
              ORDER BY c.folio LOOP
        IF aml.notificar('SLA_CONGELAMIENTO', 'ALTA', f.cliente_id, f.alerta_id, f.id, NULL, NULL,
                         format('congelamiento %s sin decisión en plazo', f.folio)) IS NOT NULL THEN
            n := n + 1;
        END IF;
    END LOOP;
    FOR a IN SELECT id, cliente_id, folio, regla_codigo, fecha_limite_reporte FROM aml.alertas
              WHERE (estado IN ('PENDIENTE', 'EN_REVISION') OR (estado = 'CONFIRMADO' AND folio_uif IS NULL))
                AND fecha_limite_reporte <= v_ahora + par.dias_aviso_plazo * interval '1 day'
              ORDER BY folio LOOP
        IF a.fecha_limite_reporte < v_ahora THEN
            IF aml.notificar('PLAZO_UIF_VENCIDO', 'ALTA', a.cliente_id, a.id, NULL, NULL, NULL,
                             format('%s · alerta %s · plazo UIF vencido', a.regla_codigo, a.folio)) IS NOT NULL THEN
                n := n + 1;
            END IF;
        ELSIF aml.notificar('PLAZO_UIF_PROXIMO', 'MEDIA', a.cliente_id, a.id, NULL, NULL, NULL,
                            format('%s · alerta %s · plazo UIF próximo', a.regla_codigo, a.folio)) IS NOT NULL THEN
            n := n + 1;
        END IF;
    END LOOP;
    RETURN n;
END
$$;

CREATE FUNCTION aml.tomar_notificaciones(p_lote INTEGER) RETURNS SETOF aml.notificaciones
LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT * FROM aml.notificaciones WHERE entregada_en IS NULL ORDER BY id LIMIT p_lote FOR UPDATE SKIP LOCKED
$$;

CREATE FUNCTION aml.marcar_notificacion(p_id BIGINT) RETURNS void
LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$ UPDATE aml.notificaciones SET entregada_en = clock_timestamp() WHERE id = p_id AND entregada_en IS NULL $$;

-- Sellado por lotes encadenados: todos los renglones de las transacciones ya
-- terminadas (txid menor que el xmin de la instantánea) que el sello anterior
-- no cubrió. Un renglón de una transacción aún en curso (aunque su id sea
-- menor) queda para el siguiente sello: nunca se sella un conjunto que luego
-- cambie, así que un COMMIT tardío no produce una falsa alteración.
CREATE FUNCTION aml.sellar_bitacora() RETURNS INTEGER
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_desde    BIGINT;
    v_hasta    BIGINT := pg_snapshot_xmin(pg_current_snapshot())::text::bigint;
    v_anterior TEXT;
    v_filas    INTEGER;
    v_cadena   TEXT;
    v_min      BIGINT;
    v_max      BIGINT;
BEGIN
    IF NOT pg_try_advisory_xact_lock(hashtextextended('aml:sello', 0)) THEN
        RETURN 0;
    END IF;
    SELECT hasta_txid, hash_lote INTO v_desde, v_anterior FROM aml.sellos_bitacora ORDER BY id DESC LIMIT 1;
    v_desde := coalesce(v_desde, 0);
    v_anterior := coalesce(v_anterior, nucleo.hash_genesis());
    IF v_hasta <= v_desde THEN
        RETURN 0;
    END IF;
    SELECT count(*), string_agg(hash_contenido, '' ORDER BY id), min(id), max(id)
      INTO v_filas, v_cadena, v_min, v_max
      FROM aml.bitacora WHERE txid >= v_desde AND txid < v_hasta;
    IF v_filas = 0 THEN
        RETURN 0;
    END IF;
    INSERT INTO aml.sellos_bitacora (desde_id, hasta_id, desde_txid, hasta_txid, filas, hash_anterior, hash_lote)
    VALUES (v_min, v_max, v_desde, v_hasta, v_filas, v_anterior, nucleo.sha256_hex(v_anterior || v_cadena));
    RETURN v_filas;
END
$$;

-- Verificación: hash de cada renglón, cadena de sellos, conteo y hash de cada
-- lote, y sellos contiguos. Incremental: desde p_desde_sello (1 = completa)
-- o, si es NULL, desde el sello siguiente al último verificado por el ciclo
-- (aml.verificaciones_bitacora), anclada en el hash ya verificado de ese
-- sello; así la consola nunca rehace toda la historia (límites de 30 s). Una
-- verificación fallida registrada responde inválida para siempre.
CREATE FUNCTION aml.verificar_bitacora(p_desde_sello BIGINT DEFAULT NULL)
RETURNS TABLE (valido BOOLEAN, filas BIGINT, sellos BIGINT, primer_error TEXT, desde_sello BIGINT,
               filas_verificadas BIGINT)
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_error    TEXT;
    v_filas    BIGINT;
    v_sellos   BIGINT;
    v_inicio   BIGINT;
    v_previo   aml.sellos_bitacora;
    s          aml.sellos_bitacora;
    v_anterior TEXT := nucleo.hash_genesis();
    v_fin_txid BIGINT := 0;
    v_cuenta   BIGINT;
    v_cadena   TEXT;
    v_revisadas BIGINT := 0;
BEGIN
    SELECT count(*) INTO v_filas FROM aml.bitacora;
    SELECT count(*) INTO v_sellos FROM aml.sellos_bitacora;
    IF p_desde_sello IS NULL THEN
        SELECT x.primer_error INTO v_error FROM aml.verificaciones_bitacora x WHERE NOT x.valido ORDER BY x.id LIMIT 1;
        IF v_error IS NOT NULL THEN
            RETURN QUERY SELECT FALSE, v_filas, v_sellos, v_error, NULL::bigint, 0::bigint;
            RETURN;
        END IF;
        v_inicio := coalesce((SELECT max(x.hasta_sello) FROM aml.verificaciones_bitacora x WHERE x.valido), 0) + 1;
    ELSE
        v_inicio := GREATEST(p_desde_sello, 1);
    END IF;
    IF v_inicio > 1 THEN
        SELECT * INTO v_previo FROM aml.sellos_bitacora WHERE id = v_inicio - 1;
        IF v_previo.id IS NULL THEN
            RETURN QUERY SELECT FALSE, v_filas, v_sellos, format('sello %s: no existe', v_inicio - 1), v_inicio, 0::bigint;
            RETURN;
        END IF;
        v_anterior := v_previo.hash_lote;
        v_fin_txid := v_previo.hasta_txid;
    END IF;
    -- Hash de cada renglón del tramo que se verifica (sellos desde v_inicio y los aún sin sellar).
    SELECT format('renglón %s: el hash no corresponde a su contenido', b.id) INTO v_error
      FROM aml.bitacora b WHERE b.txid >= v_fin_txid AND b.hash_contenido IS DISTINCT FROM aml.hash_bitacora(b)
     ORDER BY b.id LIMIT 1;
    SELECT count(*) INTO v_revisadas FROM aml.bitacora b WHERE b.txid >= v_fin_txid;
    IF v_error IS NULL THEN
        FOR s IN SELECT * FROM aml.sellos_bitacora x WHERE x.id >= v_inicio ORDER BY x.id LOOP
            IF s.desde_txid <> v_fin_txid THEN
                v_error := format('sello %s: no continúa al sello anterior (desde la transacción %s, se esperaba %s)',
                                  s.id, s.desde_txid, v_fin_txid);
                EXIT;
            END IF;
            IF s.hash_anterior <> v_anterior THEN
                v_error := format('sello %s: hash_anterior no es el hash del sello previo', s.id);
                EXIT;
            END IF;
            SELECT count(*), string_agg(b.hash_contenido, '' ORDER BY b.id) INTO v_cuenta, v_cadena
              FROM aml.bitacora b WHERE b.txid >= s.desde_txid AND b.txid < s.hasta_txid;
            IF v_cuenta <> s.filas THEN
                v_error := format('sello %s: declara %s renglones y hay %s', s.id, s.filas, v_cuenta);
                EXIT;
            END IF;
            IF s.hash_lote <> nucleo.sha256_hex(v_anterior || coalesce(v_cadena, '')) THEN
                v_error := format('sello %s: el hash del lote no corresponde a sus renglones', s.id);
                EXIT;
            END IF;
            v_anterior := s.hash_lote;
            v_fin_txid := s.hasta_txid;
        END LOOP;
    END IF;
    RETURN QUERY SELECT v_error IS NULL, v_filas, v_sellos, v_error, v_inicio, v_revisadas;
END
$$;

-- El ciclo registra hasta qué sello quedó verificada la bitácora (o la
-- primera falla, que queda para siempre). TRUE: válida.
CREATE FUNCTION aml.registrar_verificacion_bitacora() RETURNS BOOLEAN
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v        RECORD;
    v_ultimo BIGINT := coalesce((SELECT max(id) FROM aml.sellos_bitacora), 0);
BEGIN
    IF NOT pg_try_advisory_xact_lock(hashtextextended('aml:sello', 0)) THEN
        RETURN TRUE;
    END IF;
    SELECT * INTO v FROM aml.verificar_bitacora(NULL);
    IF NOT v.valido THEN
        IF NOT EXISTS (SELECT 1 FROM aml.verificaciones_bitacora WHERE NOT valido) THEN
            INSERT INTO aml.verificaciones_bitacora (hasta_sello, valido, primer_error, filas)
            VALUES (v_ultimo, FALSE, v.primer_error, v.filas);
        END IF;
        RETURN FALSE;
    END IF;
    IF v_ultimo > coalesce((SELECT max(hasta_sello) FROM aml.verificaciones_bitacora WHERE valido), 0) THEN
        INSERT INTO aml.verificaciones_bitacora (hasta_sello, valido, primer_error, filas)
        VALUES (v_ultimo, TRUE, NULL, v.filas);
    END IF;
    RETURN TRUE;
END
$$;

-- -----------------------------------------------------------------------------
-- 12. Vista de la bandeja heredada (historia para auditores)
-- -----------------------------------------------------------------------------
CREATE VIEW aml.v_historial_pld AS SELECT * FROM kyc.alertas_pld;

-- -----------------------------------------------------------------------------
-- 13. Migración de la bandeja heredada y congelamiento de kyc.alertas_pld
-- -----------------------------------------------------------------------------
-- Las alertas abiertas INUSUAL / INTERNA_PREOCUPANTE pasan a aml.alertas con
-- origen LEGADO; las RELEVANTE con movimiento van al registro trimestral; las
-- cerradas se quedan donde están (historia). Nada se barre hacia atrás, no se
-- congela a nadie retroactivamente y las cuentas CONGELADA no se tocan.
DO $$
DECLARE
    a          kyc.alertas_pld;
    r          aml.reglas_riesgo;
    n_alertas  INTEGER := 0;
    n_rel      INTEGER := 0;
    n_sin_tx   INTEGER;
    n_cerradas INTEGER;
    filas      INTEGER;
BEGIN
    PERFORM aml.fijar_actor(NULL);
    FOR a IN SELECT * FROM kyc.alertas_pld
              WHERE estado IN ('ABIERTA', 'EN_REVISION') AND tipo_operacion IN ('INUSUAL', 'INTERNA_PREOCUPANTE')
              ORDER BY folio LOOP
        SELECT * INTO r FROM aml.reglas_riesgo
         WHERE codigo = a.regla AND activa
         ORDER BY (nivel_cliente = 'TODOS') DESC,
                  CASE nivel_cliente WHEN 'BAJO' THEN 1 WHEN 'MEDIO' THEN 2 WHEN 'ALTO' THEN 3 ELSE 4 END
         LIMIT 1;
        IF r.id IS NULL THEN
            SELECT * INTO r FROM aml.reglas_riesgo WHERE codigo = 'R00_LEGADO' ORDER BY version DESC LIMIT 1;
        END IF;
        INSERT INTO aml.alertas (cliente_id, transaccion_id, entidad, sentido, regla_id, regla_codigo, tipo_reporte, origen,
                                 decision, nivel_riesgo, puntaje, estado, "timestamp", detalles, monto, monto_acumulado,
                                 ultima_ocurrencia_en, clave_deduplicacion, fecha_limite_reporte, alerta_pld_id,
                                 legado_analista)
        VALUES (a.cliente_id, a.transaccion_id, a.entidad, 'NINGUNO', r.id, r.codigo, a.tipo_operacion, 'LEGADO',
                'REGISTRADA', a.nivel_riesgo::aml.nivel_riesgo, a.puntaje,
                CASE a.estado WHEN 'ABIERTA' THEN 'PENDIENTE' ELSE 'EN_REVISION' END::aml.estado_alerta,
                a.creado_en,
                jsonb_build_object('legado', jsonb_build_object(
                    'folio', a.folio, 'regla', a.regla, 'descripcion', a.descripcion, 'detalle', a.detalle,
                    'analista', a.analista, 'dictamen', a.dictamen)),
                nullif(a.monto, 0), nullif(a.monto, 0), a.creado_en, 'LEGADO:' || a.folio,
                a.creado_en + interval '60 days', a.id, a.analista);
        n_alertas := n_alertas + 1;
    END LOOP;

    FOR a IN SELECT * FROM kyc.alertas_pld
              WHERE tipo_operacion = 'RELEVANTE' AND transaccion_id IS NOT NULL ORDER BY folio LOOP
        INSERT INTO aml.operaciones_relevantes (cliente_id, entidad, transaccion_id, sentido, canal, monto, trimestre,
                                                creado_en, reporte_folio, reportada_en)
        VALUES (a.cliente_id, a.entidad, a.transaccion_id, 'NINGUNO', NULL, coalesce(a.monto, 0), aml.trimestre(a.creado_en),
                a.creado_en,
                CASE WHEN a.estado = 'REPORTADA_UIF' THEN a.folio_uif END,
                CASE WHEN a.estado = 'REPORTADA_UIF' THEN (a.reportada_uif_en AT TIME ZONE 'America/Mexico_City')::date END)
        ON CONFLICT DO NOTHING;
        GET DIAGNOSTICS filas = ROW_COUNT;
        n_rel := n_rel + filas;
    END LOOP;

    SELECT count(*) INTO n_sin_tx FROM kyc.alertas_pld WHERE tipo_operacion = 'RELEVANTE' AND transaccion_id IS NULL;
    SELECT count(*) INTO n_cerradas FROM kyc.alertas_pld WHERE estado IN ('DESCARTADA', 'REPORTADA_UIF');

    INSERT INTO aml.bitacora (evento, objeto, actor_tipo, datos)
    VALUES ('MIGRACION_0009', 'SISTEMA', 'MIGRACION',
            jsonb_build_object('alertas_copiadas', n_alertas, 'relevantes', n_rel,
                               'relevantes_sin_transaccion', n_sin_tx, 'historicas_cerradas', n_cerradas));
END
$$;

-- La bandeja heredada queda de solo lectura (ni el dueño del esquema la cambia).
CREATE FUNCTION aml.fn_pld_legado() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    RAISE EXCEPTION 'SC_PLD_LEGADO_INMUTABLE: kyc.alertas_pld es histórico; las alertas viven en aml.alertas'
        USING ERRCODE = 'restrict_violation';
END
$$;
CREATE TRIGGER trg_alertas_pld_legado BEFORE INSERT OR UPDATE OR DELETE ON kyc.alertas_pld
    FOR EACH ROW EXECUTE FUNCTION aml.fn_pld_legado();
CREATE TRIGGER trg_alertas_pld_legado_truncate BEFORE TRUNCATE ON kyc.alertas_pld
    FOR EACH STATEMENT EXECUTE FUNCTION aml.fn_pld_legado();

-- -----------------------------------------------------------------------------
-- 14. Índices nuevos sobre tablas existentes (IF NOT EXISTS: pueden crearse
--     antes con CONCURRENTLY en un ledger grande de producción)
-- -----------------------------------------------------------------------------
CREATE INDEX IF NOT EXISTS ix_ifpe_tx_aml_destino ON ifpe.transacciones (cuenta_destino_id, creado_en)
    INCLUDE (monto, tipo, clave_idempotencia) WHERE tipo IN ('DEPOSITO', 'TRANSFERENCIA');
CREATE INDEX IF NOT EXISTS ix_ifpe_tx_aml_origen ON ifpe.transacciones (cuenta_origen_id, creado_en)
    INCLUDE (monto, tipo, clave_idempotencia, referencia_externa) WHERE tipo IN ('RETIRO', 'TRANSFERENCIA', 'TRASPASO_A_SOFOM');
CREATE INDEX IF NOT EXISTS ix_spei_inbound_aml_ordenante ON ifpe.spei_inbound (cuenta_ordenante, recibido_en)
    WHERE estado = 'ABONADO' AND cuenta_ordenante IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_spei_outbound_aml_beneficiario ON ifpe.spei_outbound (beneficiario_clabe, creado_en);
CREATE INDEX IF NOT EXISTS ix_sofom_tx_aml_credito ON sofom.transacciones (credito_id, creado_en)
    WHERE tipo IN ('DESEMBOLSO_CREDITO', 'COBRO_CREDITO');

-- -----------------------------------------------------------------------------
-- 15. Permisos
-- -----------------------------------------------------------------------------
-- Ninguna función de 0009 es ejecutable por PUBLIC.
REVOKE ALL ON ALL FUNCTIONS IN SCHEMA aml FROM PUBLIC;
REVOKE ALL ON FUNCTION ifpe.aml_evaluar(UUID, TEXT, TEXT, NUMERIC, UUID, TEXT, JSONB) FROM PUBLIC;
REVOKE ALL ON FUNCTION ifpe.aml_retener(UUID) FROM PUBLIC;
REVOKE ALL ON FUNCTION ifpe.aml_cliente_congelado(UUID) FROM PUBLIC;
REVOKE ALL ON FUNCTION sofom.aml_bloquear_cliente(UUID) FROM PUBLIC;
REVOKE ALL ON FUNCTION sofom.aml_evaluar(UUID, TEXT, TEXT, NUMERIC, UUID, TEXT, JSONB) FROM PUBLIC;

-- Roles transaccionales: sin USAGE en aml; solo las envolturas.
GRANT EXECUTE ON FUNCTION ifpe.aml_evaluar(UUID, TEXT, TEXT, NUMERIC, UUID, TEXT, JSONB) TO sc_ifpe, sc_spei, sc_puente;
GRANT EXECUTE ON FUNCTION ifpe.aml_retener(UUID) TO sc_ifpe, sc_spei;
GRANT EXECUTE ON FUNCTION ifpe.aml_cliente_congelado(UUID) TO sc_spei;
GRANT EXECUTE ON FUNCTION sofom.aml_bloquear_cliente(UUID) TO sc_sofom;
GRANT EXECUTE ON FUNCTION sofom.aml_evaluar(UUID, TEXT, TEXT, NUMERIC, UUID, TEXT, JSONB) TO sc_sofom;

-- sc_aml (consola y monitoreo): lectura de aml; ningún DML. sc_auditor no
-- recibe nada en aml: lo usan rutas del cliente (/tablero, /reserva/prueba),
-- así que un error en ellas no puede serializar datos de PLD (§4.3). El
-- AUDITOR de la consola lee por sc_aml, con aml.exigir_lector.
GRANT USAGE ON SCHEMA aml TO sc_aml;
GRANT SELECT ON ALL TABLES IN SCHEMA aml TO sc_aml;

-- Constructores JSON (errata E4: persona_json).
GRANT EXECUTE ON FUNCTION aml.persona_json(UUID), aml.alerta_json(UUID), aml.congelamiento_json(UUID),
                          aml.solicitud_json(UUID), aml.retencion_json(UUID), aml.cliente_json(UUID),
                          aml.cuentas_json(UUID), aml.regla_json(UUID), aml.parametros_json(), aml.propuesta_json(UUID),
                          aml.evaluacion_json(UUID), aml.resumen(UUID)
    TO sc_aml;
-- Errata E5: cada transacción de lectura de la consola valida el rol del actor.
GRANT EXECUTE ON FUNCTION aml.exigir_lector(UUID) TO sc_aml;
-- Funciones de los oficiales.
GRANT EXECUTE ON FUNCTION aml.consultar_alerta(UUID, UUID), aml.consultar_cliente(UUID, UUID),
                          aml.resolver_alerta(UUID, UUID, TEXT, TEXT, TEXT, BOOLEAN, BOOLEAN, TEXT, TEXT),
                          aml.registrar_reporte_uif(UUID, UUID, TEXT, DATE, TEXT),
                          aml.congelar_manual(UUID, UUID, TEXT, UUID, TEXT),
                          aml.solicitar_levantamiento(UUID, UUID, TEXT, TEXT, TEXT),
                          aml.decidir_levantamiento(UUID, UUID, BOOLEAN, TEXT, TEXT),
                          aml.proponer_cambio(UUID, TEXT, TEXT, TEXT, JSONB, TEXT, TEXT),
                          aml.decidir_propuesta(UUID, UUID, BOOLEAN, TEXT, TEXT),
                          aml.movimientos_ledger(UUID[])
    TO sc_aml;
-- Monitoreo por lotes.
GRANT EXECUTE ON FUNCTION aml.monitoreo_tomar_turno(INTEGER), aml.monitoreo_renovar_turno(UUID, INTEGER),
                          aml.monitoreo_liberar_turno(UUID, JSONB), aml.monitoreo_rango(UUID, INTEGER),
                          aml.monitoreo_candidatos(UUID), aml.monitoreo_avanzar_marca(UUID),
                          aml.monitoreo_registrar_fallo(UUID, UUID, TEXT), aml.monitoreo_cuarentena(),
                          aml.barrer_cliente(UUID, TIMESTAMPTZ, TIMESTAMPTZ[]),
                          aml.barrer_redes(TIMESTAMPTZ), aml.marcadores_pendientes(INTEGER), aml.reevaluar(UUID),
                          aml.revisar_plazos(TIMESTAMPTZ), aml.tomar_notificaciones(INTEGER),
                          aml.marcar_notificacion(BIGINT), aml.sellar_bitacora(), aml.verificar_bitacora(BIGINT),
                          aml.registrar_verificacion_bitacora()
    TO sc_aml;
-- Verificador externo de la bitácora: solo la verificación completa.
GRANT USAGE ON SCHEMA aml TO sc_auditor_pld;
GRANT EXECUTE ON FUNCTION aml.verificar_bitacora(BIGINT) TO sc_auditor_pld;
