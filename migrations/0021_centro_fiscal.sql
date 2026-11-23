-- =============================================================================
--  Synthetic Capital — 0021: centro fiscal (W-8BEN y parámetros del ejercicio)
-- =============================================================================
--
--  1. `kyc.parametros_fiscales`: las tasas que la ley fija para cada ejercicio
--     y con las que el Centro Fiscal estima el ISR:
--     - la retención anual sobre el capital que genera intereses (LISR art.
--       54; la tasa es la de la Ley de Ingresos de la Federación de cada año:
--       0.50% en 2025, 0.90% en 2026, art. 24 de la LIF 2026);
--     - la tasa sobre la ganancia neta del ejercicio por enajenar acciones en
--       bolsa (LISR art. 129: 10%);
--     - la retención de EUA sobre dividendos de fuente estadounidense: con un
--       W-8BEN vigente, la del tratado México–EUA (art. 10(2): 10%); sin él,
--       la tasa legal (30%).
--     Un ejercicio publicado no cambia: es inmutable y uno nuevo entra con una
--     migración. Sin fila para un ejercicio, el Centro Fiscal no estima.
--  2. `kyc.formularios_w8ben`: cada certificación del W-8BEN (o su
--     renovación) es una versión nueva, inmutable y encadenada por hash
--     (SC-W8BEN-v1). La base de datos:
--     - copia la Parte I del expediente verificado (nombre legal, ciudadanía,
--       residencia, domicilio permanente, el RFC como TIN extranjero y la
--       fecha de nacimiento): el cliente no la escribe;
--     - exige las tres certificaciones de la Parte III (beneficiario
--       efectivo, no es persona de EUA, residente de México para el tratado)
--       y que la firma sea su nombre legal completo;
--     - pone la fecha de firma y la vigencia: hasta el 31 de diciembre del
--       tercer año siguiente al de la firma (instrucciones del IRS), salvo un
--       cambio de circunstancias.
--     Solo una persona física verificada, residente en México, que no sea
--     ciudadana ni residente de EUA, certifica.
--  3. Cada versión deja W8BEN_CERTIFICADO en la bitácora de seguridad del
--     cliente, en la misma transacción, como 0020 con sus beneficiarios.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. Parámetros fiscales de cada ejercicio
-- -----------------------------------------------------------------------------
CREATE TABLE kyc.parametros_fiscales (
    ejercicio                     SMALLINT PRIMARY KEY
                                  CONSTRAINT ck_parametros_fiscales_ejercicio CHECK (ejercicio BETWEEN 2000 AND 2100),
    -- % anual sobre el capital que genera los intereses (LISR art. 54, tasa de la LIF).
    tasa_retencion_intereses      NUMERIC(6, 4) NOT NULL
                                  CONSTRAINT ck_parametros_fiscales_intereses CHECK (tasa_retencion_intereses >= 0 AND tasa_retencion_intereses < 100),
    fundamento_intereses          TEXT NOT NULL
                                  CONSTRAINT ck_parametros_fiscales_fundamento_intereses CHECK (char_length(fundamento_intereses) BETWEEN 3 AND 120),
    -- % sobre la ganancia neta del ejercicio por enajenación de acciones en bolsa (LISR art. 129).
    tasa_ganancia_acciones        NUMERIC(6, 4) NOT NULL
                                  CONSTRAINT ck_parametros_fiscales_acciones CHECK (tasa_ganancia_acciones >= 0 AND tasa_ganancia_acciones < 100),
    fundamento_acciones           TEXT NOT NULL
                                  CONSTRAINT ck_parametros_fiscales_fundamento_acciones CHECK (char_length(fundamento_acciones) BETWEEN 3 AND 120),
    -- % que EUA retiene sobre dividendos de fuente estadounidense: con W-8BEN vigente y sin él.
    tasa_dividendos_eua_tratado   NUMERIC(6, 4) NOT NULL
                                  CONSTRAINT ck_parametros_fiscales_tratado CHECK (tasa_dividendos_eua_tratado >= 0 AND tasa_dividendos_eua_tratado < 100),
    tasa_dividendos_eua_sin_w8ben NUMERIC(6, 4) NOT NULL
                                  CONSTRAINT ck_parametros_fiscales_sin_w8ben CHECK (tasa_dividendos_eua_sin_w8ben >= 0 AND tasa_dividendos_eua_sin_w8ben < 100),
    fundamento_dividendos_eua     TEXT NOT NULL
                                  CONSTRAINT ck_parametros_fiscales_fundamento_eua CHECK (char_length(fundamento_dividendos_eua) BETWEEN 3 AND 120),
    CONSTRAINT ck_parametros_fiscales_beneficio CHECK (tasa_dividendos_eua_tratado <= tasa_dividendos_eua_sin_w8ben)
);

COMMENT ON TABLE kyc.parametros_fiscales IS
    'Tasas de ley por ejercicio con las que el Centro Fiscal estima el ISR (0021); inmutables, una fila por año.';

INSERT INTO kyc.parametros_fiscales
    (ejercicio, tasa_retencion_intereses, fundamento_intereses, tasa_ganancia_acciones, fundamento_acciones,
     tasa_dividendos_eua_tratado, tasa_dividendos_eua_sin_w8ben, fundamento_dividendos_eua)
VALUES
    (2025, 0.5000, 'LIF 2025; LISR art. 54', 10.0000, 'LISR art. 129',
     10.0000, 30.0000, 'Tratado México–EUA, art. 10(2); IRC §871(a) y §1441'),
    (2026, 0.9000, 'LIF 2026, art. 24; LISR art. 54', 10.0000, 'LISR art. 129',
     10.0000, 30.0000, 'Tratado México–EUA, art. 10(2); IRC §871(a) y §1441');

CREATE TRIGGER trg_parametros_fiscales_inmutable BEFORE UPDATE OR DELETE ON kyc.parametros_fiscales
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_parametros_fiscales_no_truncate BEFORE TRUNCATE ON kyc.parametros_fiscales
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- -----------------------------------------------------------------------------
-- 2. La Parte I del W-8BEN, tal como sale del expediente
-- -----------------------------------------------------------------------------
-- El nombre legal: nombre y apellidos, espacios simples, NFC y en mayúsculas
-- con ICU (igual sea cual sea el ctype de la base de datos; "Vázquez" da
-- "VÁZQUEZ"). backend/src/patrimonio/impuestos.rs normaliza igual.
CREATE FUNCTION kyc.nombre_legal(p_nombre TEXT, p_paterno TEXT, p_materno TEXT) RETURNS TEXT
LANGUAGE sql IMMUTABLE PARALLEL SAFE SET search_path = pg_catalog, pg_temp
AS $$
    SELECT upper(normalize(regexp_replace(btrim(concat_ws(' ', p_nombre, p_paterno, p_materno)), '\s+', ' ', 'g'), NFC)
                 COLLATE "und-x-icu")
$$;

-- El domicilio permanente en una línea: calle y números, colonia, municipio,
-- estado, código postal y país.
CREATE FUNCTION kyc.domicilio_w8ben(d JSONB) RETURNS TEXT
LANGUAGE sql IMMUTABLE PARALLEL SAFE SET search_path = pg_catalog, pg_temp
AS $$
    SELECT concat_ws(', ',
        nullif(btrim(concat_ws(' ', btrim(d ->> 'calle'), btrim(d ->> 'numero_exterior'),
                               CASE WHEN coalesce(btrim(d ->> 'numero_interior'), '') <> ''
                                    THEN 'Int. ' || btrim(d ->> 'numero_interior') END)), ''),
        nullif(btrim(d ->> 'colonia'), ''),
        nullif(btrim(d ->> 'municipio'), ''),
        nullif(btrim(d ->> 'estado'), ''),
        CASE WHEN coalesce(btrim(d ->> 'codigo_postal'), '') <> '' THEN 'C.P. ' || btrim(d ->> 'codigo_postal') END,
        'México')
$$;

-- La firma como se compara con el nombre legal: espacios simples, NFC, mayúsculas.
CREATE FUNCTION kyc.firma_normalizada(p_firma TEXT) RETURNS TEXT
LANGUAGE sql IMMUTABLE PARALLEL SAFE SET search_path = pg_catalog, pg_temp
AS $$
    SELECT upper(normalize(regexp_replace(btrim(p_firma), '\s+', ' ', 'g'), NFC) COLLATE "und-x-icu")
$$;

-- -----------------------------------------------------------------------------
-- 3. Versiones del W-8BEN
-- -----------------------------------------------------------------------------
CREATE TABLE kyc.formularios_w8ben (
    id                    UUID PRIMARY KEY,
    cliente_id            UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    -- La pone la base de datos: la anterior más uno.
    version               INTEGER NOT NULL CONSTRAINT ck_w8ben_version CHECK (version > 0),
    -- Parte I: la copia la base de datos del expediente verificado.
    nombre_legal          TEXT NOT NULL CONSTRAINT ck_w8ben_nombre CHECK (char_length(nombre_legal) BETWEEN 3 AND 200),
    pais_ciudadania       CHAR(2) NOT NULL CONSTRAINT ck_w8ben_ciudadania CHECK (pais_ciudadania ~ '^[A-Z]{2}$' AND pais_ciudadania <> 'US'),
    pais_residencia       CHAR(2) NOT NULL CONSTRAINT ck_w8ben_residencia CHECK (pais_residencia = 'MX'),
    domicilio             TEXT NOT NULL CONSTRAINT ck_w8ben_domicilio CHECK (char_length(domicilio) BETWEEN 10 AND 300),
    tin_extranjero        VARCHAR(13) NOT NULL CONSTRAINT ck_w8ben_tin CHECK (tin_extranjero ~ '^[A-ZÑ&]{3,4}[0-9]{6}[A-Z0-9]{3}$'),
    fecha_nacimiento      DATE NOT NULL,
    -- Parte II: el tratado del país de residencia.
    pais_tratado          CHAR(2) NOT NULL,
    -- Parte III: lo que el cliente certifica bajo protesta de decir verdad.
    beneficiario_efectivo BOOLEAN NOT NULL CONSTRAINT ck_w8ben_beneficiario CHECK (beneficiario_efectivo),
    no_persona_eua        BOOLEAN NOT NULL CONSTRAINT ck_w8ben_no_persona_eua CHECK (no_persona_eua),
    residente_tratado     BOOLEAN NOT NULL CONSTRAINT ck_w8ben_residente_tratado CHECK (residente_tratado),
    -- La firma: el nombre legal completo, escrito por el cliente (normalizado).
    firma                 TEXT NOT NULL,
    -- Las ponen la base de datos.
    firmado_en            TIMESTAMPTZ NOT NULL DEFAULT now(),
    vigente_hasta         DATE NOT NULL,
    -- Quién firmó: la sesión (0019), su IP y su dispositivo.
    sesion_id             UUID REFERENCES kyc.sesiones (id) ON DELETE RESTRICT,
    ip                    INET,
    dispositivo           TEXT CONSTRAINT ck_w8ben_dispositivo CHECK (char_length(dispositivo) BETWEEN 1 AND 80),
    -- La cadena.
    hash_anterior         CHAR(64) NOT NULL CONSTRAINT ck_w8ben_hash_anterior CHECK (hash_anterior ~ '^[0-9a-f]{64}$'),
    hash                  CHAR(64) NOT NULL UNIQUE CONSTRAINT ck_w8ben_hash CHECK (hash ~ '^[0-9a-f]{64}$'),

    CONSTRAINT ux_w8ben_version UNIQUE (cliente_id, version),
    CONSTRAINT ck_w8ben_firma CHECK (firma = nombre_legal),
    CONSTRAINT ck_w8ben_tratado CHECK (pais_tratado = pais_residencia),
    -- Vence un 31 de diciembre, después de firmarse.
    CONSTRAINT ck_w8ben_vigencia CHECK (extract(month FROM vigente_hasta) = 12 AND extract(day FROM vigente_hasta) = 31
                                        AND vigente_hasta > firmado_en::date)
);
CREATE INDEX ix_w8ben_cliente ON kyc.formularios_w8ben (cliente_id, version DESC);

COMMENT ON TABLE kyc.formularios_w8ben IS
    'Versiones inmutables y encadenadas (SC-W8BEN-v1) del W-8BEN de cada cliente (0021); la vigente es la mayor.';

-- Payload canónico v1 (backend/src/patrimonio/impuestos.rs produce exactamente lo mismo):
--   'SC-W8BEN-v1' | cliente_id | version | firmado_en(epoch µs) | nombre_legal
--   | pais_ciudadania | pais_residencia | domicilio | tin_extranjero
--   | fecha_nacimiento(AAAA-MM-DD) | pais_tratado | beneficiario_efectivo
--   | no_persona_eua | residente_tratado ("true"/"false") | firma
--   | vigente_hasta(AAAA-MM-DD) | sesion_id | host(ip) | dispositivo | hash_anterior
CREATE FUNCTION kyc.payload_w8ben(f kyc.formularios_w8ben) RETURNS TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT concat_ws('|',
        nucleo.campo('SC-W8BEN-v1'),
        nucleo.campo(f.cliente_id::text),
        nucleo.campo(f.version::text),
        nucleo.campo(nucleo.epoch_micros(f.firmado_en)),
        nucleo.campo(f.nombre_legal),
        nucleo.campo(f.pais_ciudadania::text),
        nucleo.campo(f.pais_residencia::text),
        nucleo.campo(f.domicilio),
        nucleo.campo(f.tin_extranjero::text),
        nucleo.campo(to_char(f.fecha_nacimiento, 'YYYY-MM-DD')),
        nucleo.campo(f.pais_tratado::text),
        nucleo.campo(f.beneficiario_efectivo::text),
        nucleo.campo(f.no_persona_eua::text),
        nucleo.campo(f.residente_tratado::text),
        nucleo.campo(f.firma),
        nucleo.campo(to_char(f.vigente_hasta, 'YYYY-MM-DD')),
        nucleo.campo(f.sesion_id::text),
        nucleo.campo(host(f.ip)),
        nucleo.campo(f.dispositivo),
        nucleo.campo(f.hash_anterior::text)
    )
$$;
REVOKE ALL ON FUNCTION kyc.payload_w8ben(kyc.formularios_w8ben) FROM PUBLIC;

-- Antes de insertar una versión: quién puede certificar, la Parte I del
-- expediente, la firma, la versión, la fecha, la vigencia y la cadena.
CREATE FUNCTION kyc.fn_preparar_w8ben() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    c          kyc.clientes%ROWTYPE;
    v_anterior kyc.formularios_w8ben%ROWTYPE;
    v_domicilio TEXT;
BEGIN
    SELECT * INTO c FROM kyc.clientes WHERE id = NEW.cliente_id;
    IF c.estatus_kyc IS DISTINCT FROM 'VERIFICADO' THEN
        RAISE EXCEPTION 'SC_KYC_NO_VERIFICADO: solo un cliente con identidad verificada certifica el W-8BEN'
            USING ERRCODE = 'check_violation';
    END IF;
    IF c.tipo_persona <> 'FISICA' THEN
        RAISE EXCEPTION 'SC_W8BEN_PERSONA_MORAL: el W-8BEN es de personas físicas; una empresa presenta el W-8BEN-E'
            USING ERRCODE = 'check_violation';
    END IF;
    IF c.nacionalidad = 'US' OR c.pais_residencia = 'US' THEN
        RAISE EXCEPTION 'SC_W8BEN_PERSONA_EUA: un ciudadano o residente de EUA presenta el W-9, no el W-8BEN'
            USING ERRCODE = 'check_violation';
    END IF;
    IF c.pais_residencia <> 'MX' THEN
        RAISE EXCEPTION 'SC_W8BEN_RESIDENCIA: aquí certifica un residente de México (tratado México–EUA)'
            USING ERRCODE = 'check_violation';
    END IF;
    v_domicilio := kyc.domicilio_w8ben(c.domicilio);
    IF c.rfc IS NULL OR c.fecha_nacimiento IS NULL
       OR coalesce(btrim(c.domicilio ->> 'calle'), '') = '' OR coalesce(btrim(c.domicilio ->> 'codigo_postal'), '') = '' THEN
        RAISE EXCEPTION 'SC_W8BEN_DATOS_INCOMPLETOS: el expediente no tiene RFC, fecha de nacimiento o domicilio'
            USING ERRCODE = 'check_violation';
    END IF;
    NEW.nombre_legal := kyc.nombre_legal(c.nombre, c.apellido_paterno, c.apellido_materno);
    NEW.firma := kyc.firma_normalizada(NEW.firma);
    IF NEW.firma IS DISTINCT FROM NEW.nombre_legal THEN
        RAISE EXCEPTION 'SC_W8BEN_FIRMA: la firma debe ser el nombre legal completo del expediente'
            USING ERRCODE = 'check_violation';
    END IF;
    NEW.pais_ciudadania := c.nacionalidad;
    NEW.pais_residencia := c.pais_residencia;
    NEW.pais_tratado := c.pais_residencia;
    NEW.domicilio := v_domicilio;
    NEW.tin_extranjero := c.rfc;
    NEW.fecha_nacimiento := c.fecha_nacimiento;
    SELECT * INTO v_anterior FROM kyc.formularios_w8ben
     WHERE cliente_id = NEW.cliente_id ORDER BY version DESC LIMIT 1;
    NEW.version := coalesce(v_anterior.version, 0) + 1;
    NEW.hash_anterior := coalesce(v_anterior.hash, nucleo.hash_genesis());
    NEW.firmado_en := now();
    -- Hasta el último día del tercer año calendario siguiente al de la firma
    -- (año de la Ciudad de México).
    NEW.vigente_hasta := make_date(extract(year FROM now() AT TIME ZONE 'America/Mexico_City')::int + 3, 12, 31);
    NEW.hash := repeat('0', 64);  -- marcador: el payload no lo incluye
    NEW.hash := nucleo.sha256_hex(kyc.payload_w8ben(NEW));
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_w8ben_preparar BEFORE INSERT ON kyc.formularios_w8ben
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_preparar_w8ben();

-- Inmutable: una certificación no se corrige ni se borra; se renueva.
CREATE TRIGGER trg_w8ben_inmutable BEFORE UPDATE OR DELETE ON kyc.formularios_w8ben
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_w8ben_no_truncate BEFORE TRUNCATE ON kyc.formularios_w8ben
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- Verificación de la cadena de un cliente (para el DBA y las pruebas): la
-- primera versión que no cuadra, o NULL si todas.
CREATE FUNCTION kyc.verificar_w8ben(p_cliente UUID) RETURNS INTEGER
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    f          kyc.formularios_w8ben%ROWTYPE;
    v_anterior CHAR(64) := nucleo.hash_genesis();
    v_esperada INTEGER := 1;
BEGIN
    FOR f IN SELECT * FROM kyc.formularios_w8ben WHERE cliente_id = p_cliente ORDER BY version LOOP
        IF f.version <> v_esperada OR f.hash_anterior <> v_anterior
           OR f.hash <> nucleo.sha256_hex(kyc.payload_w8ben(f)) THEN
            RETURN f.version;
        END IF;
        v_anterior := f.hash;
        v_esperada := v_esperada + 1;
    END LOOP;
    RETURN NULL;
END
$$;
REVOKE ALL ON FUNCTION kyc.verificar_w8ben(UUID) FROM PUBLIC;

-- -----------------------------------------------------------------------------
-- 4. Bitácora de seguridad: cada certificación, también en la del cliente
-- -----------------------------------------------------------------------------
ALTER TABLE kyc.eventos_autenticacion DROP CONSTRAINT ck_eventos_autenticacion_evento;
ALTER TABLE kyc.eventos_autenticacion ADD CONSTRAINT ck_eventos_autenticacion_evento CHECK (evento IN (
    -- 0010
    'MFA_SECRETO_GENERADO',
    'MFA_ACTIVADO',
    'MFA_VERIFICADO',
    'MFA_CODIGO_RECHAZADO',
    'MFA_DESACTIVADO',
    'RESTABLECIMIENTO_SOLICITADO',
    'CONTRASENA_RESTABLECIDA',
    -- 0019
    'INICIO_SESION',
    'INICIO_FALLIDO',
    'CUENTA_BLOQUEADA',
    'SESION_CERRADA',
    'SESION_REVOCADA',
    'SESIONES_REVOCADAS',
    'CONTRASENA_CAMBIADA',
    'CAMBIO_CONTRASENA_RECHAZADO',
    'PREFERENCIAS_ACTUALIZADAS',
    -- 0020: `detalle` es la versión guardada
    'BENEFICIARIOS_DESIGNADOS',
    -- 0021: `detalle` es la versión del W-8BEN
    'W8BEN_CERTIFICADO'));

-- Uno por versión.
CREATE UNIQUE INDEX ux_eventos_autenticacion_w8ben ON kyc.eventos_autenticacion (cliente_id, detalle)
    WHERE evento = 'W8BEN_CERTIFICADO';

-- Corre con el rol de quien certifica (sc_kyc, o el DBA), que queda en
-- `registrado_por`; si la versión no llega al COMMIT, su evento tampoco.
CREATE FUNCTION kyc.fn_auditar_w8ben() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, sesion_id, dispositivo, detalle)
    VALUES (NEW.cliente_id, 'W8BEN_CERTIFICADO', NEW.ip, NEW.sesion_id, NEW.dispositivo, NEW.version::text);
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_w8ben_auditar AFTER INSERT ON kyc.formularios_w8ben
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_auditar_w8ben();

-- sc_kyc escribe en la bitácora estos dos eventos y ningún otro: el de una
-- versión que existe (de beneficiarios o del W-8BEN), con su misma sesión, IP
-- y dispositivo. Reemplaza la política de 0020.
DROP POLICY solo_designaciones ON kyc.eventos_autenticacion;
CREATE POLICY solo_eventos_kyc ON kyc.eventos_autenticacion AS RESTRICTIVE FOR INSERT TO sc_kyc
    WITH CHECK (
        (evento = 'BENEFICIARIOS_DESIGNADOS' AND EXISTS (
            SELECT 1 FROM kyc.designaciones_beneficiarios d
             WHERE d.cliente_id = eventos_autenticacion.cliente_id
               AND d.version::text = eventos_autenticacion.detalle
               AND d.sesion_id IS NOT DISTINCT FROM eventos_autenticacion.sesion_id
               AND d.ip IS NOT DISTINCT FROM eventos_autenticacion.ip
               AND d.dispositivo IS NOT DISTINCT FROM eventos_autenticacion.dispositivo))
        OR (evento = 'W8BEN_CERTIFICADO' AND EXISTS (
            SELECT 1 FROM kyc.formularios_w8ben f
             WHERE f.cliente_id = eventos_autenticacion.cliente_id
               AND f.version::text = eventos_autenticacion.detalle
               AND f.sesion_id IS NOT DISTINCT FROM eventos_autenticacion.sesion_id
               AND f.ip IS NOT DISTINCT FROM eventos_autenticacion.ip
               AND f.dispositivo IS NOT DISTINCT FROM eventos_autenticacion.dispositivo)));

-- -----------------------------------------------------------------------------
-- 5. Privilegios y RLS
-- -----------------------------------------------------------------------------
-- sc_kyc certifica: solo las tres certificaciones, la firma y quién firmó; lo
-- demás lo pone la base de datos.
GRANT SELECT ON kyc.formularios_w8ben, kyc.parametros_fiscales TO sc_kyc;
GRANT INSERT (id, cliente_id, beneficiario_efectivo, no_persona_eua, residente_tratado, firma, sesion_id, ip, dispositivo)
    ON kyc.formularios_w8ben TO sc_kyc;

-- El cliente lee sus versiones y los parámetros públicos; no escribe nada.
GRANT SELECT ON kyc.formularios_w8ben, kyc.parametros_fiscales TO sc_cliente;

-- Las funciones de la Parte I son puras: las usan la base de datos y el backend.
GRANT EXECUTE ON FUNCTION kyc.nombre_legal(TEXT, TEXT, TEXT), kyc.domicilio_w8ben(JSONB),
    kyc.firma_normalizada(TEXT) TO sc_kyc, sc_cliente;

ALTER TABLE kyc.formularios_w8ben ENABLE ROW LEVEL SECURITY;
CREATE POLICY por_privilegios ON kyc.formularios_w8ben USING (true) WITH CHECK (true);
CREATE POLICY solo_del_cliente ON kyc.formularios_w8ben AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual()) WITH CHECK (cliente_id = nucleo.cliente_actual());
