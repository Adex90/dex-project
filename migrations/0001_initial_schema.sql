-- =============================================================================
--  Synthetic Capital — Esquema inicial
--  Banca con reserva del 100% (modelo rothbardiano)
-- =============================================================================
--
--  REGLAS DE NEGOCIO QUE ESTE ESQUEMA HACE CUMPLIR EN LA BASE DE DATOS
--  (no dependen de que el backend se comporte bien):
--
--  1. IFPE (Bóveda a la vista) — reserva del 100%.
--     * Cada peso en `ifpe.cuentas` está respaldado por un peso líquido en
--       `ifpe.reserva_custodia` (solo efectivo / depósitos en Banxico o banco
--       custodio, nunca instrumentos de crédito).
--     * `ifpe.estado_reserva` lleva pasivo y activo agregados vía triggers; un
--       constraint trigger DIFERIDO aborta el COMMIT de cualquier transacción
--       que deje pasivo <> reserva. No hay forma de "prestar" un peso de la IFPE.
--
--  2. SOFOM (Crédito) — solo presta lo depositado a plazo (contratos de mutuo).
--     * Cada crédito debe estar 100% fondeado por `sofom.asignaciones_fondeo`
--       contra contratos de mutuo vigentes (verificado al COMMIT).
--     * Calce de plazos: vencimiento del pasivo (mutuo) >= vencimiento del
--       activo (crédito). Verificado por trigger al asignar y al modificar.
--     * Los términos de un mutuo son inmutables; no existe retiro anticipado.
--
--  3. Aislamiento estricto IFPE / SOFOM.
--     * Esquemas separados (`ifpe`, `sofom`); sin llaves foráneas entre ellos.
--     * Roles separados (`sc_ifpe`, `sc_sofom`). El backend ejecuta cada
--       transacción con `SET LOCAL ROLE`, de modo que una transacción de la IFPE
--       no puede ni leer tablas de la SOFOM y viceversa.
--     * El único cruce es el rol `sc_puente` (IFPE -> mutuo), que NO tiene
--       permisos sobre créditos ni asignaciones: el puente no puede prestar.
--
--  4. Ledger inmutable y encadenado (Proof of Reserve).
--     * `transacciones` solo admite INSERT. UPDATE/DELETE/TRUNCATE abortan.
--     * Cada movimiento recibe `secuencia`, `hash_anterior` y
--       `tx_hash = SHA-256(payload canónico)` calculados por la propia base de
--       datos; el backend recalcula el hash de forma independiente y verifica.
--
--  Montos: NUMERIC(18,8). Agregados: NUMERIC(20,8).
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 0. Esquemas
-- -----------------------------------------------------------------------------
CREATE SCHEMA nucleo;  -- utilidades compartidas (sin datos)
CREATE SCHEMA kyc;     -- identidad de clientes y cumplimiento PLD/FT
CREATE SCHEMA ifpe;    -- Institución de Fondos de Pago Electrónico (bóveda)
CREATE SCHEMA sofom;   -- Sociedad Financiera de Objeto Múltiple (crédito)

COMMENT ON SCHEMA ifpe  IS 'Bóveda a la vista. Reserva 100%. Este dinero NUNCA se presta.';
COMMENT ON SCHEMA sofom IS 'División de crédito. Solo presta fondos de contratos de mutuo a plazo con calce de vencimientos.';

-- -----------------------------------------------------------------------------
-- 1. Utilidades compartidas
-- -----------------------------------------------------------------------------

-- Hash genesis de cada ledger.
CREATE FUNCTION nucleo.hash_genesis() RETURNS CHAR(64)
LANGUAGE sql IMMUTABLE PARALLEL SAFE
AS $$ SELECT repeat('0', 64)::char(64) $$;

-- Codifica un campo del payload canónico sin ambigüedad:
--   NULL      -> '~'
--   'valor'   -> '<bytes UTF-8>:valor'
-- El backend en Rust implementa exactamente la misma codificación.
CREATE FUNCTION nucleo.campo(v TEXT) RETURNS TEXT
LANGUAGE sql IMMUTABLE PARALLEL SAFE
AS $$ SELECT CASE WHEN v IS NULL THEN '~' ELSE octet_length(v)::text || ':' || v END $$;

CREATE FUNCTION nucleo.sha256_hex(payload TEXT) RETURNS CHAR(64)
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
AS $$ SELECT encode(sha256(convert_to(payload, 'UTF8')), 'hex')::char(64) $$;

-- Marca de tiempo canónica: microsegundos desde epoch Unix.
CREATE FUNCTION nucleo.epoch_micros(ts TIMESTAMPTZ) RETURNS TEXT
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
AS $$ SELECT (extract(epoch FROM ts) * 1000000)::bigint::text $$;

-- Validación del dígito verificador de una CLABE (pesos 3,7,1).
CREATE FUNCTION nucleo.clabe_valida(clabe TEXT) RETURNS BOOLEAN
LANGUAGE plpgsql IMMUTABLE STRICT PARALLEL SAFE
AS $$
DECLARE
    pesos CONSTANT INT[] := ARRAY[3,7,1,3,7,1,3,7,1,3,7,1,3,7,1,3,7];
    suma  INT := 0;
BEGIN
    IF clabe !~ '^[0-9]{18}$' THEN
        RETURN FALSE;
    END IF;
    FOR i IN 1..17 LOOP
        suma := suma + (substr(clabe, i, 1)::int * pesos[i]) % 10;
    END LOOP;
    RETURN (10 - suma % 10) % 10 = substr(clabe, 18, 1)::int;
END
$$;

CREATE FUNCTION nucleo.fn_tocar_actualizado_en() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.actualizado_en := now();
    RETURN NEW;
END
$$;

-- Ledger inmutable: rechaza UPDATE, DELETE y TRUNCATE.
CREATE FUNCTION nucleo.fn_ledger_inmutable() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'SC_LEDGER_INMUTABLE: % sobre %.% no está permitido; el ledger solo admite INSERT',
        TG_OP, TG_TABLE_SCHEMA, TG_TABLE_NAME
        USING ERRCODE = 'restrict_violation';
END
$$;

-- -----------------------------------------------------------------------------
-- 2. KYC — clientes y credenciales
-- -----------------------------------------------------------------------------
CREATE TABLE kyc.clientes (
    id                         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tipo_persona               TEXT NOT NULL CHECK (tipo_persona IN ('FISICA', 'MORAL')),

    -- Perfil
    nombre                     TEXT NOT NULL CHECK (length(btrim(nombre)) > 0), -- nombre(s) o razón social
    apellido_paterno           TEXT,
    apellido_materno           TEXT,
    fecha_nacimiento           DATE,          -- o fecha de constitución (persona moral)
    nacionalidad               CHAR(2) NOT NULL DEFAULT 'MX' CHECK (nacionalidad ~ '^[A-Z]{2}$'),
    pais_residencia            CHAR(2) NOT NULL DEFAULT 'MX' CHECK (pais_residencia ~ '^[A-Z]{2}$'),
    rfc                        VARCHAR(13) UNIQUE CHECK (rfc ~ '^[A-ZÑ&]{3,4}[0-9]{6}[A-Z0-9]{3}$'),
    curp                       VARCHAR(18) UNIQUE CHECK (curp ~ '^[A-Z]{4}[0-9]{6}[HMX][A-Z]{5}[A-Z0-9][0-9]$'),
    email                      TEXT NOT NULL CHECK (email ~ '^[^@\s]+@[^@\s]+\.[^@\s]+$'),
    telefono                   TEXT CHECK (telefono ~ '^\+?[0-9]{10,15}$'),
    domicilio                  JSONB NOT NULL DEFAULT '{}'::jsonb,
    actividad_economica        TEXT,
    ocupacion                  TEXT,

    -- KYC
    nivel_cuenta               SMALLINT NOT NULL DEFAULT 1 CHECK (nivel_cuenta BETWEEN 1 AND 4), -- niveles CNBV
    estatus_kyc                TEXT NOT NULL DEFAULT 'PENDIENTE'
                                   CHECK (estatus_kyc IN ('PENDIENTE', 'EN_REVISION', 'VERIFICADO', 'RECHAZADO', 'SUSPENDIDO')),
    documento_identidad_tipo   TEXT CHECK (documento_identidad_tipo IN ('INE', 'PASAPORTE', 'CEDULA_PROFESIONAL', 'FM2_FM3', 'ACTA_CONSTITUTIVA')),
    documento_identidad_numero TEXT,
    kyc_verificado_en          TIMESTAMPTZ,

    -- Perfil transaccional y riesgo PLD/FT
    origen_recursos            TEXT,
    destino_recursos           TEXT,
    ingreso_mensual_estimado   NUMERIC(18, 8) CHECK (ingreso_mensual_estimado >= 0),
    es_pep                     BOOLEAN NOT NULL DEFAULT FALSE,  -- Persona Políticamente Expuesta
    en_lista_bloqueadas        BOOLEAN NOT NULL DEFAULT FALSE,  -- Lista de Personas Bloqueadas (UIF)
    nivel_riesgo_pld           TEXT NOT NULL DEFAULT 'MEDIO' CHECK (nivel_riesgo_pld IN ('BAJO', 'MEDIO', 'ALTO')),
    riesgo_evaluado_en         TIMESTAMPTZ,

    creado_en                  TIMESTAMPTZ NOT NULL DEFAULT now(),
    actualizado_en             TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ck_clientes_curp_solo_fisica   CHECK (tipo_persona = 'FISICA' OR curp IS NULL),
    CONSTRAINT ck_clientes_verificado_fecha   CHECK (estatus_kyc <> 'VERIFICADO' OR kyc_verificado_en IS NOT NULL),
    -- Una persona en la Lista de Personas Bloqueadas no puede operar.
    CONSTRAINT ck_clientes_lista_bloqueadas   CHECK (NOT en_lista_bloqueadas OR estatus_kyc IN ('SUSPENDIDO', 'RECHAZADO')),
    -- PEP y listas: diligencia reforzada obligatoria.
    CONSTRAINT ck_clientes_pep_riesgo_alto    CHECK (NOT es_pep OR nivel_riesgo_pld = 'ALTO')
);
CREATE UNIQUE INDEX ux_clientes_email ON kyc.clientes (lower(email));
CREATE INDEX ix_clientes_riesgo ON kyc.clientes (nivel_riesgo_pld, estatus_kyc);
CREATE TRIGGER trg_clientes_actualizado_en BEFORE UPDATE ON kyc.clientes
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_tocar_actualizado_en();

CREATE TABLE kyc.credenciales (
    cliente_id        UUID PRIMARY KEY REFERENCES kyc.clientes (id) ON DELETE CASCADE,
    password_hash     TEXT NOT NULL,                 -- Argon2id (PHC string)
    intentos_fallidos INTEGER NOT NULL DEFAULT 0 CHECK (intentos_fallidos >= 0),
    bloqueado_hasta   TIMESTAMPTZ,
    ultimo_acceso     TIMESTAMPTZ,
    creado_en         TIMESTAMPTZ NOT NULL DEFAULT now(),
    actualizado_en    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TRIGGER trg_credenciales_actualizado_en BEFORE UPDATE ON kyc.credenciales
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_tocar_actualizado_en();

-- -----------------------------------------------------------------------------
-- 3. KYC — alertas PLD/FT (monitoreo transaccional)
-- -----------------------------------------------------------------------------
-- Las alertas pueden referirse a movimientos de cualquiera de los dos ledgers;
-- por aislamiento, `transaccion_id` no tiene FK y se interpreta junto con `entidad`.
CREATE TABLE kyc.alertas_pld (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    folio            BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE,
    cliente_id       UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    entidad          TEXT NOT NULL CHECK (entidad IN ('IFPE', 'SOFOM')),
    transaccion_id   UUID,
    tipo_operacion   TEXT NOT NULL CHECK (tipo_operacion IN ('RELEVANTE', 'INUSUAL', 'INTERNA_PREOCUPANTE')),
    regla            TEXT NOT NULL,              -- código de la regla, p. ej. 'R01_UMBRAL_RELEVANTE'
    nivel_riesgo     TEXT NOT NULL CHECK (nivel_riesgo IN ('BAJO', 'MEDIO', 'ALTO', 'CRITICO')),
    puntaje          NUMERIC(5, 2) NOT NULL DEFAULT 0 CHECK (puntaje BETWEEN 0 AND 100),
    monto            NUMERIC(18, 8) CHECK (monto >= 0),
    descripcion      TEXT NOT NULL,
    detalle          JSONB NOT NULL DEFAULT '{}'::jsonb,
    estado           TEXT NOT NULL DEFAULT 'ABIERTA'
                         CHECK (estado IN ('ABIERTA', 'EN_REVISION', 'DESCARTADA', 'REPORTADA_UIF')),
    analista         TEXT,
    dictamen         TEXT,
    folio_uif        TEXT,
    reportada_uif_en TIMESTAMPTZ,
    creado_en        TIMESTAMPTZ NOT NULL DEFAULT now(),
    actualizado_en   TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ck_alertas_cierre_con_dictamen CHECK (estado NOT IN ('DESCARTADA', 'REPORTADA_UIF') OR dictamen IS NOT NULL),
    CONSTRAINT ck_alertas_reporte_uif         CHECK (estado <> 'REPORTADA_UIF' OR reportada_uif_en IS NOT NULL)
);
CREATE INDEX ix_alertas_pld_bandeja ON kyc.alertas_pld (estado, nivel_riesgo, creado_en);
CREATE INDEX ix_alertas_pld_cliente ON kyc.alertas_pld (cliente_id, creado_en);
CREATE INDEX ix_alertas_pld_tx      ON kyc.alertas_pld (entidad, transaccion_id);
CREATE TRIGGER trg_alertas_pld_actualizado_en BEFORE UPDATE ON kyc.alertas_pld
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_tocar_actualizado_en();

-- =============================================================================
-- 4. IFPE — Bóveda a la vista (reserva 100%)
-- =============================================================================

-- 4.1 Cuentas de clientes (pasivo de la IFPE)
CREATE TABLE ifpe.cuentas (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cliente_id       UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    tipo_entidad     TEXT NOT NULL DEFAULT 'IFPE' CHECK (tipo_entidad = 'IFPE'),
    clabe            CHAR(18) NOT NULL UNIQUE CHECK (nucleo.clabe_valida(clabe)),
    moneda           CHAR(3) NOT NULL DEFAULT 'MXN' CHECK (moneda = 'MXN'),
    -- Libre disposición.
    saldo_disponible NUMERIC(18, 8) NOT NULL DEFAULT 0
                         CONSTRAINT ck_ifpe_cuentas_saldo_disponible_no_negativo CHECK (saldo_disponible >= 0),
    -- Retenciones (PLD, orden de autoridad). Sigue respaldado al 100%; nunca se presta.
    saldo_bloqueado  NUMERIC(18, 8) NOT NULL DEFAULT 0
                         CONSTRAINT ck_ifpe_cuentas_saldo_bloqueado_no_negativo CHECK (saldo_bloqueado >= 0),
    estado           TEXT NOT NULL DEFAULT 'ACTIVA' CHECK (estado IN ('ACTIVA', 'CONGELADA', 'CERRADA')),
    creado_en        TIMESTAMPTZ NOT NULL DEFAULT now(),
    actualizado_en   TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ux_ifpe_cuentas_cliente_moneda UNIQUE (cliente_id, moneda),
    CONSTRAINT ck_ifpe_cuentas_cerrada_sin_saldo CHECK (estado <> 'CERRADA' OR (saldo_disponible = 0 AND saldo_bloqueado = 0))
);
CREATE TRIGGER trg_ifpe_cuentas_actualizado_en BEFORE UPDATE ON ifpe.cuentas
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_tocar_actualizado_en();

-- 4.2 Reserva en custodia (activo de la IFPE). Solo dinero líquido.
CREATE TABLE ifpe.reserva_custodia (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    institucion    TEXT NOT NULL,
    -- Únicamente efectivo o depósitos a la vista. Prohibidos bonos, reportos o
    -- cualquier instrumento que implique prestar el dinero de los clientes.
    tipo           TEXT NOT NULL CHECK (tipo IN ('BANXICO', 'BANCO_CUSTODIO', 'EFECTIVO_BOVEDA')),
    numero_cuenta  TEXT NOT NULL,
    moneda         CHAR(3) NOT NULL DEFAULT 'MXN' CHECK (moneda = 'MXN'),
    saldo          NUMERIC(18, 8) NOT NULL DEFAULT 0
                       CONSTRAINT ck_ifpe_reserva_saldo_no_negativo CHECK (saldo >= 0),
    activa         BOOLEAN NOT NULL DEFAULT TRUE,
    creado_en      TIMESTAMPTZ NOT NULL DEFAULT now(),
    actualizado_en TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ux_ifpe_reserva_cuenta UNIQUE (institucion, numero_cuenta)
);
CREATE TRIGGER trg_ifpe_reserva_actualizado_en BEFORE UPDATE ON ifpe.reserva_custodia
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_tocar_actualizado_en();

-- 4.3 Estado agregado de la reserva (fila única, mantenida por triggers).
CREATE TABLE ifpe.estado_reserva (
    id                  BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    pasivo_clientes     NUMERIC(20, 8) NOT NULL DEFAULT 0 CHECK (pasivo_clientes >= 0),
    activo_reserva      NUMERIC(20, 8) NOT NULL DEFAULT 0 CHECK (activo_reserva >= 0),
    coeficiente_reserva NUMERIC(20, 8) GENERATED ALWAYS AS (
                            CASE WHEN pasivo_clientes = 0 THEN 1 ELSE activo_reserva / pasivo_clientes END
                        ) STORED,
    actualizado_en      TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO ifpe.estado_reserva DEFAULT VALUES;

CREATE FUNCTION ifpe.fn_acumular_pasivo() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    delta NUMERIC(20, 8);
BEGIN
    delta := CASE TG_OP
        WHEN 'INSERT' THEN NEW.saldo_disponible + NEW.saldo_bloqueado
        WHEN 'UPDATE' THEN (NEW.saldo_disponible + NEW.saldo_bloqueado) - (OLD.saldo_disponible + OLD.saldo_bloqueado)
        WHEN 'DELETE' THEN -(OLD.saldo_disponible + OLD.saldo_bloqueado)
    END;
    IF delta <> 0 THEN
        UPDATE ifpe.estado_reserva
           SET pasivo_clientes = pasivo_clientes + delta, actualizado_en = now()
         WHERE id;
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_ifpe_cuentas_pasivo
    AFTER INSERT OR UPDATE OF saldo_disponible, saldo_bloqueado OR DELETE ON ifpe.cuentas
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_acumular_pasivo();

CREATE FUNCTION ifpe.fn_acumular_reserva() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    delta NUMERIC(20, 8);
BEGIN
    delta := CASE TG_OP
        WHEN 'INSERT' THEN NEW.saldo
        WHEN 'UPDATE' THEN NEW.saldo - OLD.saldo
        WHEN 'DELETE' THEN -OLD.saldo
    END;
    IF delta <> 0 THEN
        UPDATE ifpe.estado_reserva
           SET activo_reserva = activo_reserva + delta, actualizado_en = now()
         WHERE id;
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_ifpe_reserva_activo
    AFTER INSERT OR UPDATE OF saldo OR DELETE ON ifpe.reserva_custodia
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_acumular_reserva();

-- REGLA INVIOLABLE #1: al COMMIT, pasivo con clientes == reserva líquida.
CREATE FUNCTION ifpe.fn_verificar_reserva_100() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    pasivo  NUMERIC(20, 8);
    reserva NUMERIC(20, 8);
BEGIN
    -- Se relee la fila: el NEW del evento diferido puede no ser el estado final.
    SELECT pasivo_clientes, activo_reserva INTO pasivo, reserva
      FROM ifpe.estado_reserva WHERE id;
    IF pasivo <> reserva THEN
        RAISE EXCEPTION 'SC_RESERVA_100: pasivo con clientes (%) <> reserva líquida en custodia (%). Transacción abortada.',
            pasivo, reserva
            USING ERRCODE = 'check_violation',
                  HINT = 'Todo depósito/retiro de la IFPE debe mover la reserva en custodia por el mismo monto, en la misma transacción.';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_ifpe_reserva_100
    AFTER INSERT OR UPDATE ON ifpe.estado_reserva
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_verificar_reserva_100();

-- 4.4 Ledger IFPE (inmutable, encadenado por hash)
CREATE TABLE ifpe.ledger_cabeza (
    id               BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    ultima_secuencia BIGINT NOT NULL DEFAULT 0,
    ultimo_hash      CHAR(64) NOT NULL DEFAULT nucleo.hash_genesis()
);
INSERT INTO ifpe.ledger_cabeza DEFAULT VALUES;

CREATE TABLE ifpe.transacciones (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    secuencia             BIGINT NOT NULL UNIQUE,
    tipo                  TEXT NOT NULL CHECK (tipo IN (
                              'DEPOSITO',             -- entra efectivo: +cliente, +reserva
                              'RETIRO',               -- sale efectivo:  -cliente, -reserva
                              'TRANSFERENCIA',        -- entre clientes IFPE; reserva intacta
                              'TRASPASO_A_SOFOM',     -- aportación a mutuo: -cliente, -reserva (el efectivo sale a la SOFOM)
                              'RETORNO_DE_SOFOM',     -- vencimiento de mutuo: +cliente, +reserva
                              'RETENCION',            -- disponible -> bloqueado (misma cuenta)
                              'LIBERACION_RETENCION'  -- bloqueado -> disponible (misma cuenta)
                          )),
    cuenta_origen_id      UUID REFERENCES ifpe.cuentas (id) ON DELETE RESTRICT,
    cuenta_destino_id     UUID REFERENCES ifpe.cuentas (id) ON DELETE RESTRICT,
    reserva_id            UUID REFERENCES ifpe.reserva_custodia (id) ON DELETE RESTRICT,
    monto                 NUMERIC(18, 8) NOT NULL CHECK (monto > 0),
    moneda                CHAR(3) NOT NULL DEFAULT 'MXN' CHECK (moneda = 'MXN'),
    concepto              TEXT,
    referencia_externa    TEXT,          -- p. ej. clave de rastreo SPEI
    clave_idempotencia    TEXT NOT NULL UNIQUE,
    ref_movimiento_espejo UUID,          -- movimiento correspondiente en el ledger SOFOM (sin FK por aislamiento)
    hash_anterior         CHAR(64) NOT NULL,
    tx_hash               CHAR(64) NOT NULL UNIQUE CHECK (tx_hash ~ '^[0-9a-f]{64}$'),
    creado_en             TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ck_ifpe_tx_forma CHECK (
        (tipo = 'DEPOSITO'         AND cuenta_origen_id IS NULL     AND cuenta_destino_id IS NOT NULL AND reserva_id IS NOT NULL) OR
        (tipo = 'RETIRO'           AND cuenta_origen_id IS NOT NULL AND cuenta_destino_id IS NULL     AND reserva_id IS NOT NULL) OR
        (tipo = 'TRANSFERENCIA'    AND cuenta_origen_id IS NOT NULL AND cuenta_destino_id IS NOT NULL
                                   AND cuenta_origen_id <> cuenta_destino_id                         AND reserva_id IS NULL)     OR
        (tipo = 'TRASPASO_A_SOFOM' AND cuenta_origen_id IS NOT NULL AND cuenta_destino_id IS NULL     AND reserva_id IS NOT NULL
                                   AND ref_movimiento_espejo IS NOT NULL)                                                         OR
        (tipo = 'RETORNO_DE_SOFOM' AND cuenta_origen_id IS NULL     AND cuenta_destino_id IS NOT NULL AND reserva_id IS NOT NULL
                                   AND ref_movimiento_espejo IS NOT NULL)                                                         OR
        (tipo IN ('RETENCION', 'LIBERACION_RETENCION')
                                   AND cuenta_origen_id IS NOT NULL AND cuenta_destino_id = cuenta_origen_id
                                   AND reserva_id IS NULL)
    )
);
CREATE INDEX ix_ifpe_tx_origen  ON ifpe.transacciones (cuenta_origen_id, secuencia);
CREATE INDEX ix_ifpe_tx_destino ON ifpe.transacciones (cuenta_destino_id, secuencia);
CREATE INDEX ix_ifpe_tx_fecha   ON ifpe.transacciones (creado_en);

-- Payload canónico v1 (el backend en Rust debe producir exactamente lo mismo):
--   'SC-IFPE-v1' | secuencia | id | tipo | origen | destino | reserva | monto
--   | moneda | concepto | referencia_externa | clave_idempotencia
--   | ref_movimiento_espejo | creado_en(epoch µs) | hash_anterior
-- con cada campo codificado por nucleo.campo() y unido por '|'.
CREATE FUNCTION ifpe.payload_canonico(t ifpe.transacciones) RETURNS TEXT
LANGUAGE sql IMMUTABLE
AS $$
    SELECT concat_ws('|',
        nucleo.campo('SC-IFPE-v1'),
        nucleo.campo(t.secuencia::text),
        nucleo.campo(t.id::text),
        nucleo.campo(t.tipo),
        nucleo.campo(t.cuenta_origen_id::text),
        nucleo.campo(t.cuenta_destino_id::text),
        nucleo.campo(t.reserva_id::text),
        nucleo.campo(t.monto::text),
        nucleo.campo(t.moneda::text),
        nucleo.campo(t.concepto),
        nucleo.campo(t.referencia_externa),
        nucleo.campo(t.clave_idempotencia),
        nucleo.campo(t.ref_movimiento_espejo::text),
        nucleo.campo(nucleo.epoch_micros(t.creado_en)),
        nucleo.campo(t.hash_anterior::text)
    )
$$;

-- Asigna secuencia, hash_anterior, creado_en y tx_hash. Bloquea la cabeza del
-- ledger (FOR UPDATE) => orden total y cadena sin bifurcaciones.
CREATE FUNCTION ifpe.fn_encadenar_tx() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    cabeza ifpe.ledger_cabeza%ROWTYPE;
BEGIN
    SELECT * INTO cabeza FROM ifpe.ledger_cabeza WHERE id FOR UPDATE;

    NEW.secuencia     := cabeza.ultima_secuencia + 1;
    NEW.hash_anterior := cabeza.ultimo_hash;
    NEW.creado_en     := clock_timestamp();
    NEW.tx_hash       := nucleo.sha256_hex(ifpe.payload_canonico(NEW));

    UPDATE ifpe.ledger_cabeza
       SET ultima_secuencia = NEW.secuencia, ultimo_hash = NEW.tx_hash
     WHERE id;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_ifpe_tx_encadenar BEFORE INSERT ON ifpe.transacciones
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_encadenar_tx();
CREATE TRIGGER trg_ifpe_tx_inmutable BEFORE UPDATE OR DELETE ON ifpe.transacciones
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_ifpe_tx_no_truncate BEFORE TRUNCATE ON ifpe.transacciones
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- 4.5 Cortes de Proof of Reserve (raíces Merkle publicadas; inmutables)
CREATE TABLE ifpe.cortes_reserva (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    merkle_root      CHAR(64) NOT NULL CHECK (merkle_root ~ '^[0-9a-f]{64}$'),
    num_hojas        INTEGER NOT NULL CHECK (num_hojas >= 0),
    total_pasivo     NUMERIC(20, 8) NOT NULL CHECK (total_pasivo >= 0),
    total_reserva    NUMERIC(20, 8) NOT NULL CHECK (total_reserva >= 0),
    secuencia_ledger BIGINT NOT NULL,        -- último movimiento incluido
    hash_ledger      CHAR(64) NOT NULL,      -- tx_hash de ese movimiento (ancla la raíz a la cadena)
    algoritmo        TEXT NOT NULL DEFAULT 'SHA-256 merkle-sum v1',
    creado_en        TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ck_cortes_reserva_100 CHECK (total_reserva = total_pasivo)
);
CREATE INDEX ix_cortes_reserva_fecha ON ifpe.cortes_reserva (creado_en DESC);
CREATE TRIGGER trg_cortes_inmutable BEFORE UPDATE OR DELETE ON ifpe.cortes_reserva
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- Hojas del árbol Merkle: saldo total (disponible + retenido) de cada cuenta.
CREATE VIEW ifpe.v_hojas_reserva AS
SELECT c.id AS cuenta_id,
       c.cliente_id,
       (c.saldo_disponible + c.saldo_bloqueado)::numeric(18, 8) AS saldo_total
  FROM ifpe.cuentas c
 WHERE c.estado <> 'CERRADA';

-- =============================================================================
-- 5. SOFOM — División de crédito (solo fondeo a plazo)
-- =============================================================================

-- 5.1 Cuentas de inversión de clientes (pasivo de la SOFOM)
CREATE TABLE sofom.cuentas (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cliente_id       UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    tipo_entidad     TEXT NOT NULL DEFAULT 'SOFOM' CHECK (tipo_entidad = 'SOFOM'),
    numero_cuenta    BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE,
    moneda           CHAR(3) NOT NULL DEFAULT 'MXN' CHECK (moneda = 'MXN'),
    -- Principal + intereses de mutuos vencidos, pendientes de regresar a la IFPE.
    saldo_disponible NUMERIC(18, 8) NOT NULL DEFAULT 0
                         CONSTRAINT ck_sofom_cuentas_saldo_disponible_no_negativo CHECK (saldo_disponible >= 0),
    -- Principal comprometido en contratos de mutuo vigentes.
    saldo_bloqueado  NUMERIC(18, 8) NOT NULL DEFAULT 0
                         CONSTRAINT ck_sofom_cuentas_saldo_bloqueado_no_negativo CHECK (saldo_bloqueado >= 0),
    estado           TEXT NOT NULL DEFAULT 'ACTIVA' CHECK (estado IN ('ACTIVA', 'CONGELADA', 'CERRADA')),
    creado_en        TIMESTAMPTZ NOT NULL DEFAULT now(),
    actualizado_en   TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ux_sofom_cuentas_cliente_moneda UNIQUE (cliente_id, moneda),
    CONSTRAINT ck_sofom_cuentas_cerrada_sin_saldo CHECK (estado <> 'CERRADA' OR (saldo_disponible = 0 AND saldo_bloqueado = 0))
);
CREATE TRIGGER trg_sofom_cuentas_actualizado_en BEFORE UPDATE ON sofom.cuentas
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_tocar_actualizado_en();

-- 5.2 Tesorería segregada de la SOFOM (cuentas distintas a las de la IFPE)
CREATE TABLE sofom.caja (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    institucion    TEXT NOT NULL,
    numero_cuenta  TEXT NOT NULL,
    moneda         CHAR(3) NOT NULL DEFAULT 'MXN' CHECK (moneda = 'MXN'),
    saldo          NUMERIC(18, 8) NOT NULL DEFAULT 0
                       CONSTRAINT ck_sofom_caja_saldo_no_negativo CHECK (saldo >= 0),
    activa         BOOLEAN NOT NULL DEFAULT TRUE,
    creado_en      TIMESTAMPTZ NOT NULL DEFAULT now(),
    actualizado_en TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ux_sofom_caja_cuenta UNIQUE (institucion, numero_cuenta)
);
CREATE TRIGGER trg_sofom_caja_actualizado_en BEFORE UPDATE ON sofom.caja
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_tocar_actualizado_en();

-- 5.3 Contratos de mutuo (pasivo a plazo fijo; única fuente de fondeo)
CREATE TABLE sofom.contratos_mutuo (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    folio                 BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE,
    cliente_id            UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    cuenta_sofom_id       UUID NOT NULL REFERENCES sofom.cuentas (id) ON DELETE RESTRICT,
    cuenta_ifpe_origen_id UUID NOT NULL,  -- cuenta de la bóveda que aportó (sin FK por aislamiento)
    tx_aportacion_ifpe_id UUID NOT NULL UNIQUE, -- movimiento TRASPASO_A_SOFOM en el ledger IFPE
    principal             NUMERIC(18, 8) NOT NULL CHECK (principal > 0),
    tasa_anual            NUMERIC(9, 6) NOT NULL CHECK (tasa_anual >= 0 AND tasa_anual < 1),
    plazo_dias            INTEGER NOT NULL CHECK (plazo_dias BETWEEN 7 AND 3650),
    fecha_inicio          DATE NOT NULL,
    fecha_vencimiento     DATE GENERATED ALWAYS AS (fecha_inicio + plazo_dias) STORED,
    -- Porción del principal actualmente comprometida en créditos.
    principal_asignado    NUMERIC(18, 8) NOT NULL DEFAULT 0
                              CONSTRAINT ck_mutuo_sin_sobreasignacion CHECK (principal_asignado >= 0 AND principal_asignado <= principal),
    interes_pagado        NUMERIC(18, 8) NOT NULL DEFAULT 0 CHECK (interes_pagado >= 0),
    -- Sin estado de cancelación: el mutuo no admite retiro anticipado.
    estado                TEXT NOT NULL DEFAULT 'VIGENTE' CHECK (estado IN ('VIGENTE', 'VENCIDO', 'LIQUIDADO')),
    hash_terminos         CHAR(64) NOT NULL CHECK (hash_terminos ~ '^[0-9a-f]{64}$'), -- SHA-256 de los términos aceptados
    creado_en             TIMESTAMPTZ NOT NULL DEFAULT now(),
    actualizado_en        TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX ix_mutuo_cliente     ON sofom.contratos_mutuo (cliente_id);
CREATE INDEX ix_mutuo_vencimiento ON sofom.contratos_mutuo (estado, fecha_vencimiento);
CREATE TRIGGER trg_mutuo_actualizado_en BEFORE UPDATE ON sofom.contratos_mutuo
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_tocar_actualizado_en();

-- Términos inmutables y transiciones de estado solo hacia adelante.
CREATE FUNCTION sofom.fn_proteger_mutuo() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF (NEW.cliente_id, NEW.cuenta_sofom_id, NEW.cuenta_ifpe_origen_id, NEW.tx_aportacion_ifpe_id,
        NEW.principal, NEW.tasa_anual, NEW.plazo_dias, NEW.fecha_inicio, NEW.hash_terminos)
       IS DISTINCT FROM
       (OLD.cliente_id, OLD.cuenta_sofom_id, OLD.cuenta_ifpe_origen_id, OLD.tx_aportacion_ifpe_id,
        OLD.principal, OLD.tasa_anual, OLD.plazo_dias, OLD.fecha_inicio, OLD.hash_terminos) THEN
        RAISE EXCEPTION 'SC_MUTUO_INMUTABLE: los términos del contrato de mutuo % no pueden modificarse', OLD.folio
            USING ERRCODE = 'restrict_violation';
    END IF;

    IF NEW.estado IS DISTINCT FROM OLD.estado THEN
        IF NOT ((OLD.estado = 'VIGENTE' AND NEW.estado = 'VENCIDO')
             OR (OLD.estado = 'VENCIDO' AND NEW.estado = 'LIQUIDADO')) THEN
            RAISE EXCEPTION 'SC_MUTUO_TRANSICION: % -> % no permitida', OLD.estado, NEW.estado
                USING ERRCODE = 'check_violation';
        END IF;
        IF NEW.estado = 'VENCIDO' AND current_date < OLD.fecha_vencimiento THEN
            RAISE EXCEPTION 'SC_MUTUO_RETIRO_ANTICIPADO: el contrato % vence el %; no se admite retiro anticipado',
                OLD.folio, OLD.fecha_vencimiento
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_mutuo_proteger BEFORE UPDATE ON sofom.contratos_mutuo
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_proteger_mutuo();
CREATE TRIGGER trg_mutuo_no_delete BEFORE DELETE ON sofom.contratos_mutuo
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- 5.4 Créditos (activo de la SOFOM)
CREATE TABLE sofom.creditos (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    folio             BIGINT GENERATED ALWAYS AS IDENTITY UNIQUE,
    acreditado_id     UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    principal         NUMERIC(18, 8) NOT NULL CHECK (principal > 0),
    tasa_anual        NUMERIC(9, 6) NOT NULL CHECK (tasa_anual >= 0 AND tasa_anual < 1),
    plazo_dias        INTEGER NOT NULL CHECK (plazo_dias BETWEEN 1 AND 3650),
    fecha_desembolso  DATE NOT NULL,
    fecha_vencimiento DATE GENERATED ALWAYS AS (fecha_desembolso + plazo_dias) STORED,
    saldo_insoluto    NUMERIC(18, 8) NOT NULL DEFAULT 0
                          CONSTRAINT ck_creditos_saldo_insoluto CHECK (saldo_insoluto >= 0 AND saldo_insoluto <= principal),
    -- EN_FONDEO: se están asignando mutuos; no se ha desembolsado.
    estado            TEXT NOT NULL DEFAULT 'EN_FONDEO'
                          CHECK (estado IN ('EN_FONDEO', 'VIGENTE', 'VENCIDO', 'LIQUIDADO', 'CASTIGADO')),
    destino           TEXT,
    creado_en         TIMESTAMPTZ NOT NULL DEFAULT now(),
    actualizado_en    TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ck_creditos_sin_saldo_en_fondeo CHECK (estado <> 'EN_FONDEO' OR saldo_insoluto = 0)
);
CREATE INDEX ix_creditos_acreditado ON sofom.creditos (acreditado_id);
CREATE INDEX ix_creditos_estado     ON sofom.creditos (estado, fecha_vencimiento);
CREATE TRIGGER trg_creditos_actualizado_en BEFORE UPDATE ON sofom.creditos
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_tocar_actualizado_en();

-- 5.5 Asignaciones de fondeo: qué mutuo fondea qué crédito y por cuánto.
CREATE TABLE sofom.asignaciones_fondeo (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    credito_id  UUID NOT NULL REFERENCES sofom.creditos (id) ON DELETE RESTRICT,
    contrato_id UUID NOT NULL REFERENCES sofom.contratos_mutuo (id) ON DELETE RESTRICT,
    monto       NUMERIC(18, 8) NOT NULL CHECK (monto > 0),
    liberada    BOOLEAN NOT NULL DEFAULT FALSE,  -- TRUE cuando el crédito se liquida y el principal regresa
    creado_en   TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ux_asignacion_credito_contrato UNIQUE (credito_id, contrato_id)
);
CREATE INDEX ix_asignaciones_contrato ON sofom.asignaciones_fondeo (contrato_id);

-- REGLA INVIOLABLE #2 (calce de plazos) al asignar fondeo.
CREATE FUNCTION sofom.fn_validar_asignacion() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    mutuo   sofom.contratos_mutuo%ROWTYPE;
    credito sofom.creditos%ROWTYPE;
    ya_asignado NUMERIC(20, 8);
BEGIN
    IF TG_OP = 'INSERT' THEN
        -- Orden de bloqueo fijo (crédito, luego mutuo) para evitar deadlocks.
        SELECT * INTO credito FROM sofom.creditos       WHERE id = NEW.credito_id  FOR UPDATE;
        SELECT * INTO mutuo   FROM sofom.contratos_mutuo WHERE id = NEW.contrato_id FOR UPDATE;

        IF NEW.liberada THEN
            RAISE EXCEPTION 'SC_ASIGNACION: una asignación nueva no puede nacer liberada'
                USING ERRCODE = 'check_violation';
        END IF;
        IF credito.estado <> 'EN_FONDEO' THEN
            RAISE EXCEPTION 'SC_ASIGNACION: el crédito % ya no está en fondeo (estado %)', credito.folio, credito.estado
                USING ERRCODE = 'check_violation';
        END IF;
        IF mutuo.estado <> 'VIGENTE' THEN
            RAISE EXCEPTION 'SC_ASIGNACION: el mutuo % no está vigente (estado %)', mutuo.folio, mutuo.estado
                USING ERRCODE = 'check_violation';
        END IF;
        IF mutuo.fecha_vencimiento < credito.fecha_vencimiento THEN
            RAISE EXCEPTION 'SC_CALCE_PLAZOS: el mutuo % vence el % pero el crédito % vence el %. El pasivo debe vencer en o después del activo.',
                mutuo.folio, mutuo.fecha_vencimiento, credito.folio, credito.fecha_vencimiento
                USING ERRCODE = 'check_violation';
        END IF;
        IF mutuo.fecha_inicio > credito.fecha_desembolso THEN
            RAISE EXCEPTION 'SC_CALCE_PLAZOS: el mutuo % inicia el %, después del desembolso del crédito % (%)',
                mutuo.folio, mutuo.fecha_inicio, credito.folio, credito.fecha_desembolso
                USING ERRCODE = 'check_violation';
        END IF;
        IF mutuo.principal - mutuo.principal_asignado < NEW.monto THEN
            RAISE EXCEPTION 'SC_SOBREASIGNACION: el mutuo % solo tiene % disponibles para asignar (solicitado %)',
                mutuo.folio, mutuo.principal - mutuo.principal_asignado, NEW.monto
                USING ERRCODE = 'check_violation';
        END IF;
        SELECT coalesce(sum(monto), 0) INTO ya_asignado
          FROM sofom.asignaciones_fondeo WHERE credito_id = NEW.credito_id;
        IF ya_asignado + NEW.monto > credito.principal THEN
            RAISE EXCEPTION 'SC_SOBREFONDEO: el crédito % quedaría fondeado por % > principal %',
                credito.folio, ya_asignado + NEW.monto, credito.principal
                USING ERRCODE = 'check_violation';
        END IF;

        UPDATE sofom.contratos_mutuo
           SET principal_asignado = principal_asignado + NEW.monto
         WHERE id = NEW.contrato_id;
        RETURN NEW;

    ELSIF TG_OP = 'UPDATE' THEN
        -- Solo se permite liberar (FALSE -> TRUE); lo hace el trigger de créditos.
        IF (NEW.credito_id, NEW.contrato_id, NEW.monto) IS DISTINCT FROM (OLD.credito_id, OLD.contrato_id, OLD.monto)
           OR (OLD.liberada AND NOT NEW.liberada) THEN
            RAISE EXCEPTION 'SC_ASIGNACION_INMUTABLE: solo puede marcarse como liberada'
                USING ERRCODE = 'restrict_violation';
        END IF;
        RETURN NEW;

    ELSE -- DELETE: solo mientras el crédito sigue en fondeo.
        SELECT * INTO credito FROM sofom.creditos WHERE id = OLD.credito_id FOR UPDATE;
        IF credito.estado <> 'EN_FONDEO' THEN
            RAISE EXCEPTION 'SC_ASIGNACION_INMUTABLE: el crédito % ya fue desembolsado', credito.folio
                USING ERRCODE = 'restrict_violation';
        END IF;
        UPDATE sofom.contratos_mutuo
           SET principal_asignado = principal_asignado - OLD.monto
         WHERE id = OLD.contrato_id;
        RETURN OLD;
    END IF;
END
$$;
CREATE TRIGGER trg_asignacion_validar BEFORE INSERT OR UPDATE OR DELETE ON sofom.asignaciones_fondeo
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_validar_asignacion();

-- Protección de créditos: términos fijos tras el desembolso, calce de plazos si
-- se reprograma en fondeo, y liberación del fondeo al liquidarse.
CREATE FUNCTION sofom.fn_proteger_credito() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    min_venc_mutuo DATE;
    asignado       NUMERIC(20, 8);
BEGIN
    IF (NEW.acreditado_id, NEW.principal, NEW.tasa_anual, NEW.plazo_dias, NEW.fecha_desembolso)
       IS DISTINCT FROM
       (OLD.acreditado_id, OLD.principal, OLD.tasa_anual, OLD.plazo_dias, OLD.fecha_desembolso) THEN
        IF OLD.estado <> 'EN_FONDEO' THEN
            RAISE EXCEPTION 'SC_CREDITO_INMUTABLE: el crédito % ya fue desembolsado; sus términos son fijos', OLD.folio
                USING ERRCODE = 'restrict_violation';
        END IF;
        -- Reprogramación durante el fondeo: debe seguir calzando con cada mutuo asignado.
        SELECT min(m.fecha_vencimiento), coalesce(sum(a.monto), 0)
          INTO min_venc_mutuo, asignado
          FROM sofom.asignaciones_fondeo a
          JOIN sofom.contratos_mutuo m ON m.id = a.contrato_id
         WHERE a.credito_id = OLD.id;
        IF min_venc_mutuo IS NOT NULL AND min_venc_mutuo < NEW.fecha_desembolso + NEW.plazo_dias THEN
            RAISE EXCEPTION 'SC_CALCE_PLAZOS: el nuevo vencimiento del crédito % (%) excede el de su fondeo (%)',
                OLD.folio, NEW.fecha_desembolso + NEW.plazo_dias, min_venc_mutuo
                USING ERRCODE = 'check_violation';
        END IF;
        IF asignado > NEW.principal THEN
            RAISE EXCEPTION 'SC_SOBREFONDEO: el crédito % ya tiene % asignados > nuevo principal %',
                OLD.folio, asignado, NEW.principal
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;

    IF NEW.estado IS DISTINCT FROM OLD.estado THEN
        IF NOT ((OLD.estado = 'EN_FONDEO' AND NEW.estado = 'VIGENTE')
             OR (OLD.estado = 'VIGENTE'   AND NEW.estado IN ('VENCIDO', 'LIQUIDADO'))
             OR (OLD.estado = 'VENCIDO'   AND NEW.estado IN ('LIQUIDADO', 'CASTIGADO'))) THEN
            RAISE EXCEPTION 'SC_CREDITO_TRANSICION: % -> % no permitida', OLD.estado, NEW.estado
                USING ERRCODE = 'check_violation';
        END IF;
        -- Al liquidarse, el principal regresa y vuelve a estar disponible en el
        -- mutuo (que solo podrá fondear créditos que venzan antes que él).
        -- Un CASTIGO no libera: la pérdida consume la capacidad del mutuo.
        IF NEW.estado = 'LIQUIDADO' THEN
            UPDATE sofom.contratos_mutuo m
               SET principal_asignado = m.principal_asignado - a.monto
              FROM sofom.asignaciones_fondeo a
             WHERE a.credito_id = OLD.id AND a.contrato_id = m.id AND NOT a.liberada;
            UPDATE sofom.asignaciones_fondeo SET liberada = TRUE
             WHERE credito_id = OLD.id AND NOT liberada;
        END IF;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_credito_proteger BEFORE UPDATE ON sofom.creditos
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_proteger_credito();
CREATE TRIGGER trg_credito_no_delete BEFORE DELETE ON sofom.creditos
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- REGLA INVIOLABLE #2 (fondeo total) al COMMIT: todo crédito fuera de
-- EN_FONDEO debe estar fondeado al 100% por mutuos.
CREATE FUNCTION sofom.fn_verificar_fondeo_total() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    actual   sofom.creditos%ROWTYPE;
    fondeado NUMERIC(20, 8);
BEGIN
    SELECT * INTO actual FROM sofom.creditos WHERE id = NEW.id;
    IF actual.estado = 'EN_FONDEO' THEN
        RETURN NULL;
    END IF;
    SELECT coalesce(sum(monto), 0) INTO fondeado
      FROM sofom.asignaciones_fondeo WHERE credito_id = NEW.id;
    IF fondeado <> actual.principal THEN
        RAISE EXCEPTION 'SC_FONDEO_INCOMPLETO: el crédito % (principal %) solo está fondeado por % en contratos de mutuo',
            actual.folio, actual.principal, fondeado
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_credito_fondeo_total
    AFTER INSERT OR UPDATE ON sofom.creditos
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_verificar_fondeo_total();

-- 5.6 Ledger SOFOM (inmutable, encadenado por hash; cadena independiente)
CREATE TABLE sofom.ledger_cabeza (
    id               BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    ultima_secuencia BIGINT NOT NULL DEFAULT 0,
    ultimo_hash      CHAR(64) NOT NULL DEFAULT nucleo.hash_genesis()
);
INSERT INTO sofom.ledger_cabeza DEFAULT VALUES;

CREATE TABLE sofom.transacciones (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    secuencia             BIGINT NOT NULL UNIQUE,
    tipo                  TEXT NOT NULL CHECK (tipo IN (
                              'APORTACION_MUTUO',   -- efectivo llega de la IFPE: +caja, +bloqueado
                              'VENCIMIENTO_MUTUO',  -- bloqueado -> disponible
                              'PAGO_INTERES_MUTUO', -- -caja, +disponible
                              'RETORNO_A_IFPE',     -- -caja, -disponible (efectivo regresa a la IFPE)
                              'DESEMBOLSO_CREDITO', -- -caja, +cartera
                              'COBRO_CREDITO'       -- +caja, -cartera (principal y/o intereses)
                          )),
    cuenta_id             UUID REFERENCES sofom.cuentas (id) ON DELETE RESTRICT,
    contrato_id           UUID REFERENCES sofom.contratos_mutuo (id) ON DELETE RESTRICT,
    credito_id            UUID REFERENCES sofom.creditos (id) ON DELETE RESTRICT,
    caja_id               UUID REFERENCES sofom.caja (id) ON DELETE RESTRICT,
    monto                 NUMERIC(18, 8) NOT NULL CHECK (monto > 0),
    moneda                CHAR(3) NOT NULL DEFAULT 'MXN' CHECK (moneda = 'MXN'),
    concepto              TEXT,
    clave_idempotencia    TEXT NOT NULL UNIQUE,
    ref_movimiento_espejo UUID,          -- movimiento correspondiente en el ledger IFPE (sin FK por aislamiento)
    hash_anterior         CHAR(64) NOT NULL,
    tx_hash               CHAR(64) NOT NULL UNIQUE CHECK (tx_hash ~ '^[0-9a-f]{64}$'),
    creado_en             TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ck_sofom_tx_forma CHECK (
        (tipo = 'APORTACION_MUTUO'   AND cuenta_id IS NOT NULL AND contrato_id IS NOT NULL AND caja_id IS NOT NULL
                                     AND credito_id IS NULL AND ref_movimiento_espejo IS NOT NULL) OR
        (tipo = 'VENCIMIENTO_MUTUO'  AND cuenta_id IS NOT NULL AND contrato_id IS NOT NULL AND caja_id IS NULL
                                     AND credito_id IS NULL) OR
        (tipo = 'PAGO_INTERES_MUTUO' AND cuenta_id IS NOT NULL AND contrato_id IS NOT NULL AND caja_id IS NOT NULL
                                     AND credito_id IS NULL) OR
        (tipo = 'RETORNO_A_IFPE'     AND cuenta_id IS NOT NULL AND caja_id IS NOT NULL
                                     AND credito_id IS NULL AND ref_movimiento_espejo IS NOT NULL) OR
        (tipo IN ('DESEMBOLSO_CREDITO', 'COBRO_CREDITO')
                                     AND credito_id IS NOT NULL AND caja_id IS NOT NULL
                                     AND cuenta_id IS NULL AND contrato_id IS NULL)
    )
);
CREATE INDEX ix_sofom_tx_cuenta   ON sofom.transacciones (cuenta_id, secuencia);
CREATE INDEX ix_sofom_tx_contrato ON sofom.transacciones (contrato_id);
CREATE INDEX ix_sofom_tx_credito  ON sofom.transacciones (credito_id);

CREATE FUNCTION sofom.payload_canonico(t sofom.transacciones) RETURNS TEXT
LANGUAGE sql IMMUTABLE
AS $$
    SELECT concat_ws('|',
        nucleo.campo('SC-SOFOM-v1'),
        nucleo.campo(t.secuencia::text),
        nucleo.campo(t.id::text),
        nucleo.campo(t.tipo),
        nucleo.campo(t.cuenta_id::text),
        nucleo.campo(t.contrato_id::text),
        nucleo.campo(t.credito_id::text),
        nucleo.campo(t.caja_id::text),
        nucleo.campo(t.monto::text),
        nucleo.campo(t.moneda::text),
        nucleo.campo(t.concepto),
        nucleo.campo(t.clave_idempotencia),
        nucleo.campo(t.ref_movimiento_espejo::text),
        nucleo.campo(nucleo.epoch_micros(t.creado_en)),
        nucleo.campo(t.hash_anterior::text)
    )
$$;

CREATE FUNCTION sofom.fn_encadenar_tx() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    cabeza sofom.ledger_cabeza%ROWTYPE;
BEGIN
    SELECT * INTO cabeza FROM sofom.ledger_cabeza WHERE id FOR UPDATE;

    NEW.secuencia     := cabeza.ultima_secuencia + 1;
    NEW.hash_anterior := cabeza.ultimo_hash;
    NEW.creado_en     := clock_timestamp();
    NEW.tx_hash       := nucleo.sha256_hex(sofom.payload_canonico(NEW));

    UPDATE sofom.ledger_cabeza
       SET ultima_secuencia = NEW.secuencia, ultimo_hash = NEW.tx_hash
     WHERE id;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_sofom_tx_encadenar BEFORE INSERT ON sofom.transacciones
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_encadenar_tx();
CREATE TRIGGER trg_sofom_tx_inmutable BEFORE UPDATE OR DELETE ON sofom.transacciones
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_sofom_tx_no_truncate BEFORE TRUNCATE ON sofom.transacciones
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- =============================================================================
-- 6. Roles y aislamiento (REGLA INVIOLABLE #3)
-- =============================================================================
-- Roles NOLOGIN. El usuario de la aplicación es miembro de todos y, por cada
-- transacción, ejecuta `SET LOCAL ROLE <rol>` con el menor privilegio necesario:
--   sc_ifpe     operaciones de la bóveda (depósito, retiro, transferencia)
--   sc_sofom    operaciones de crédito (créditos, asignaciones, cobranza)
--   sc_puente   IFPE -> mutuo y retorno al vencimiento; SIN acceso a créditos
--   sc_auditor  Proof of Reserve (solo lectura + registrar cortes)
DO $$
DECLARE
    r TEXT;
BEGIN
    FOREACH r IN ARRAY ARRAY['sc_ifpe', 'sc_sofom', 'sc_puente', 'sc_auditor'] LOOP
        IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = r) THEN
            EXECUTE format('CREATE ROLE %I NOLOGIN', r);
        END IF;
        EXECUTE format('GRANT %I TO %I', r, current_user);
    END LOOP;
END
$$;

REVOKE ALL ON SCHEMA nucleo, kyc, ifpe, sofom FROM PUBLIC;
GRANT USAGE ON SCHEMA nucleo, kyc TO sc_ifpe, sc_sofom, sc_puente, sc_auditor;

-- Todos leen identidad/riesgo del cliente y pueden levantar alertas PLD.
GRANT SELECT ON kyc.clientes TO sc_ifpe, sc_sofom, sc_puente, sc_auditor;
GRANT INSERT ON kyc.alertas_pld TO sc_ifpe, sc_sofom, sc_puente;

-- sc_ifpe: solo el esquema ifpe.
GRANT USAGE ON SCHEMA ifpe TO sc_ifpe;
GRANT SELECT, INSERT, UPDATE ON ifpe.cuentas TO sc_ifpe;
GRANT SELECT, UPDATE         ON ifpe.reserva_custodia TO sc_ifpe;
GRANT SELECT, INSERT         ON ifpe.transacciones TO sc_ifpe;
GRANT SELECT                 ON ifpe.estado_reserva, ifpe.ledger_cabeza, ifpe.cortes_reserva, ifpe.v_hojas_reserva TO sc_ifpe;

-- sc_sofom: solo el esquema sofom.
GRANT USAGE ON SCHEMA sofom TO sc_sofom;
GRANT SELECT, INSERT, UPDATE         ON sofom.cuentas, sofom.creditos TO sc_sofom;
GRANT SELECT, UPDATE                 ON sofom.caja TO sc_sofom;
-- `principal_asignado` solo lo mueven los triggers de asignación/liquidación.
GRANT SELECT                         ON sofom.contratos_mutuo TO sc_sofom;
GRANT UPDATE (estado, interes_pagado) ON sofom.contratos_mutuo TO sc_sofom;
GRANT SELECT, INSERT, UPDATE, DELETE ON sofom.asignaciones_fondeo TO sc_sofom;
GRANT SELECT, INSERT                 ON sofom.transacciones TO sc_sofom;
GRANT SELECT                         ON sofom.ledger_cabeza TO sc_sofom;

-- sc_puente: mueve efectivo IFPE <-> SOFOM y crea mutuos. NO puede tocar
-- créditos ni asignaciones de fondeo: el puente nunca origina préstamos.
GRANT USAGE ON SCHEMA ifpe, sofom TO sc_puente;
GRANT SELECT, UPDATE ON ifpe.cuentas, ifpe.reserva_custodia TO sc_puente;
GRANT SELECT, INSERT ON ifpe.transacciones TO sc_puente;
GRANT SELECT ON ifpe.estado_reserva TO sc_puente;
GRANT SELECT, INSERT, UPDATE ON sofom.cuentas TO sc_puente;
GRANT SELECT, INSERT ON sofom.contratos_mutuo TO sc_puente;
GRANT UPDATE (estado, interes_pagado) ON sofom.contratos_mutuo TO sc_puente;
GRANT SELECT, UPDATE ON sofom.caja TO sc_puente;
GRANT SELECT, INSERT ON sofom.transacciones TO sc_puente;

-- sc_auditor: lectura de la bóveda y registro de cortes de Proof of Reserve.
GRANT USAGE ON SCHEMA ifpe TO sc_auditor;
GRANT SELECT ON ifpe.cuentas, ifpe.reserva_custodia, ifpe.transacciones,
                ifpe.estado_reserva, ifpe.ledger_cabeza, ifpe.cortes_reserva,
                ifpe.v_hojas_reserva TO sc_auditor;
GRANT INSERT ON ifpe.cortes_reserva TO sc_auditor;
