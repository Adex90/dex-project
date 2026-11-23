-- =============================================================================
--  Synthetic Capital — 0020: beneficiarios de la cuenta
-- =============================================================================
--
--  El titular de una cuenta de depósito designa a sus beneficiarios y el
--  porcentaje de cada uno (art. 56 de la Ley de Instituciones de Crédito; la
--  práctica se replica para la cuenta de fondos de pago electrónico). Sin
--  designación, el saldo sigue las reglas de la sucesión.
--
--  1. `kyc.designaciones_beneficiarios`: cada vez que el cliente guarda sus
--     beneficiarios nace una versión nueva, inmutable, con quién la hizo (la
--     sesión de 0019, la IP y el dispositivo) y cuándo. La vigente es la de
--     versión mayor. Las versiones de un cliente forman una cadena: cada una
--     guarda el hash de la anterior y su propio `hash` = SHA-256 del payload
--     canónico SC-BENEF-v1, calculado por la base de datos sobre la versión y
--     sus beneficiarios. El historial es la bitácora: nada se corrige ni se
--     borra, y alterar una versión rompe la cadena (backend/src/kyc/beneficiarios.rs
--     la recalcula al leerla).
--  2. `kyc.beneficiarios`: los de una versión, de 1 a 5, con su nombre legal
--     completo, parentesco, CURP o fecha de nacimiento (al menos uno; si van
--     los dos, la fecha es la de la CURP) y su porcentaje (positivo, a
--     centésimos). Se insertan antes que su versión (la llave foránea es
--     diferida), así el hash de la versión los cubre.
--  3. Al COMMIT, la base de datos comprueba cada versión nueva: tantos
--     beneficiarios como declara, en las posiciones 1..n, que suman
--     **exactamente 100%** si hay alguno (una versión vacía retira la
--     designación), ninguno es el titular (su CURP) ni se repite, y el hash
--     cubre exactamente lo guardado (un beneficiario agregado después, o a una
--     versión anterior, no pasa).
--  4. Solo un cliente con KYC VERIFICADO designa. Escribe sc_kyc; el cliente
--     lee lo suyo (sc_cliente, RLS como en 0011) y no escribe nada.
--  5. Cambiar a quién hereda la cuenta es además un evento de seguridad: la
--     base de datos escribe BENEFICIARIOS_DESIGNADOS en la bitácora inmutable
--     del cliente (`kyc.eventos_autenticacion`, 0010/0019) en la misma
--     transacción que cada versión, con su sesión, IP, dispositivo y la
--     versión. El cliente lo ve en su Centro de Seguridad; ninguna versión se
--     guarda sin su evento, y sc_kyc no puede escribir ningún otro.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. Versiones de la designación
-- -----------------------------------------------------------------------------
CREATE TABLE kyc.designaciones_beneficiarios (
    id                UUID PRIMARY KEY,
    cliente_id        UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    -- La pone la base de datos: la anterior más uno.
    version           INTEGER NOT NULL CONSTRAINT ck_designaciones_version CHECK (version > 0),
    num_beneficiarios SMALLINT NOT NULL CONSTRAINT ck_designaciones_num CHECK (num_beneficiarios BETWEEN 0 AND 5),
    total_porcentaje  NUMERIC(5, 2) NOT NULL,
    -- Quién la guardó: la sesión (0019), su IP y su dispositivo.
    sesion_id         UUID REFERENCES kyc.sesiones (id) ON DELETE RESTRICT,
    ip                INET,
    dispositivo       TEXT CONSTRAINT ck_designaciones_dispositivo CHECK (char_length(dispositivo) BETWEEN 1 AND 80),
    -- La cadena: los pone la base de datos.
    hash_anterior     CHAR(64) NOT NULL CONSTRAINT ck_designaciones_hash_anterior CHECK (hash_anterior ~ '^[0-9a-f]{64}$'),
    hash              CHAR(64) NOT NULL UNIQUE CONSTRAINT ck_designaciones_hash CHECK (hash ~ '^[0-9a-f]{64}$'),
    creada_en         TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ux_designaciones_version UNIQUE (cliente_id, version),
    -- Sin beneficiarios, 0%; con alguno, exactamente 100%.
    CONSTRAINT ck_designaciones_total CHECK (
        (num_beneficiarios = 0 AND total_porcentaje = 0) OR (num_beneficiarios > 0 AND total_porcentaje = 100))
);

COMMENT ON TABLE kyc.designaciones_beneficiarios IS
    'Versiones inmutables y encadenadas (SC-BENEF-v1) de la designación de beneficiarios (0020); la vigente es la mayor.';

-- -----------------------------------------------------------------------------
-- 2. Beneficiarios de cada versión
-- -----------------------------------------------------------------------------
CREATE TABLE kyc.beneficiarios (
    -- Diferida: los beneficiarios se insertan antes que su versión.
    designacion_id   UUID NOT NULL REFERENCES kyc.designaciones_beneficiarios (id) ON DELETE RESTRICT
                         DEFERRABLE INITIALLY DEFERRED,
    posicion         SMALLINT NOT NULL CONSTRAINT ck_beneficiarios_posicion CHECK (posicion BETWEEN 1 AND 5),
    -- Nombre(s) y apellidos como en su identificación; el backend revisa letras y palabras.
    nombre_completo  TEXT NOT NULL CONSTRAINT ck_beneficiarios_nombre CHECK (
                         char_length(nombre_completo) BETWEEN 3 AND 120
                         AND nombre_completo = btrim(nombre_completo)
                         AND nombre_completo !~ '[0-9[:cntrl:]]'),
    parentesco       TEXT NOT NULL CONSTRAINT ck_beneficiarios_parentesco CHECK (parentesco IN (
                         'CONYUGE', 'CONCUBINO', 'HIJO', 'PADRE', 'HERMANO', 'NIETO', 'ABUELO', 'OTRO')),
    curp             VARCHAR(18) CONSTRAINT ck_beneficiarios_curp
                         CHECK (curp ~ '^[A-Z]{4}[0-9]{6}[HMX][A-Z]{5}[A-Z0-9][0-9]$'),
    fecha_nacimiento DATE CONSTRAINT ck_beneficiarios_fecha CHECK (fecha_nacimiento >= DATE '1900-01-01'),
    -- Positivo, a centésimos. Con escala 4, un 33.333 no se redondea en silencio: se rechaza.
    porcentaje       NUMERIC(7, 4) NOT NULL CONSTRAINT ck_beneficiarios_porcentaje
                         CHECK (porcentaje > 0 AND porcentaje <= 100 AND porcentaje = round(porcentaje, 2)),

    PRIMARY KEY (designacion_id, posicion),
    CONSTRAINT ux_beneficiarios_curp UNIQUE (designacion_id, curp),
    -- Se identifica por su CURP o por su fecha de nacimiento.
    CONSTRAINT ck_beneficiarios_identidad CHECK (curp IS NOT NULL OR fecha_nacimiento IS NOT NULL),
    -- Con las dos, la fecha es la que codifica la CURP (AAMMDD; la posición 17
    -- es un dígito antes de 2000 y una letra desde 2000).
    CONSTRAINT ck_beneficiarios_curp_fecha CHECK (
        curp IS NULL OR fecha_nacimiento IS NULL OR (
            to_char(fecha_nacimiento, 'YYMMDD') = substr(curp, 5, 6)
            AND (substr(curp, 17, 1) ~ '[0-9]') = (fecha_nacimiento < DATE '2000-01-01')))
);

COMMENT ON TABLE kyc.beneficiarios IS
    'Beneficiarios de una versión de la designación (0020): inmutables, de 1 a 5, que suman 100%.';

-- -----------------------------------------------------------------------------
-- 3. Payload canónico, cadena y comprobación al COMMIT
-- -----------------------------------------------------------------------------
-- Payload canónico v1 (backend/src/kyc/beneficiarios.rs produce exactamente lo mismo):
--   'SC-BENEF-v1' | cliente_id | version | creada_en(epoch µs) | num_beneficiarios
--   | total_porcentaje | sesion_id | host(ip) | dispositivo
--   | por cada beneficiario, en orden de posición:
--       posicion | nombre_completo | parentesco | curp | fecha_nacimiento(AAAA-MM-DD)
--       | porcentaje (4 decimales: "60.0000")
--   | hash_anterior
--   El total va con 2 decimales ("100.00", "0.00").
CREATE FUNCTION kyc.payload_designacion(d kyc.designaciones_beneficiarios) RETURNS TEXT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT concat_ws('|',
        nucleo.campo('SC-BENEF-v1'),
        nucleo.campo(d.cliente_id::text),
        nucleo.campo(d.version::text),
        nucleo.campo(nucleo.epoch_micros(d.creada_en)),
        nucleo.campo(d.num_beneficiarios::text),
        nucleo.campo(d.total_porcentaje::text),
        nucleo.campo(d.sesion_id::text),
        nucleo.campo(host(d.ip)),
        nucleo.campo(d.dispositivo),
        (SELECT string_agg(concat_ws('|',
                    nucleo.campo(b.posicion::text),
                    nucleo.campo(b.nombre_completo),
                    nucleo.campo(b.parentesco),
                    nucleo.campo(b.curp),
                    nucleo.campo(to_char(b.fecha_nacimiento, 'YYYY-MM-DD')),
                    nucleo.campo(b.porcentaje::text)), '|' ORDER BY b.posicion)
           FROM kyc.beneficiarios b WHERE b.designacion_id = d.id),
        nucleo.campo(d.hash_anterior::text)
    )
$$;
REVOKE ALL ON FUNCTION kyc.payload_designacion(kyc.designaciones_beneficiarios) FROM PUBLIC;

-- Antes de insertar una versión: el cliente está verificado; la versión, el
-- hash anterior, la fecha y el hash los pone la base de datos (sobre los
-- beneficiarios ya insertados en esta transacción).
CREATE FUNCTION kyc.fn_preparar_designacion() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_estatus  TEXT;
    v_anterior kyc.designaciones_beneficiarios%ROWTYPE;
BEGIN
    SELECT estatus_kyc INTO v_estatus FROM kyc.clientes WHERE id = NEW.cliente_id;
    IF v_estatus IS DISTINCT FROM 'VERIFICADO' THEN
        RAISE EXCEPTION 'SC_KYC_NO_VERIFICADO: solo un cliente con identidad verificada designa beneficiarios'
            USING ERRCODE = 'check_violation';
    END IF;
    SELECT * INTO v_anterior FROM kyc.designaciones_beneficiarios
     WHERE cliente_id = NEW.cliente_id ORDER BY version DESC LIMIT 1;
    NEW.version := coalesce(v_anterior.version, 0) + 1;
    NEW.hash_anterior := coalesce(v_anterior.hash, nucleo.hash_genesis());
    NEW.creada_en := now();
    NEW.hash := repeat('0', 64);  -- marcador: el payload no lo incluye
    NEW.hash := nucleo.sha256_hex(kyc.payload_designacion(NEW));
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_designaciones_preparar BEFORE INSERT ON kyc.designaciones_beneficiarios
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_preparar_designacion();

-- Un beneficiario no nace en el futuro.
CREATE FUNCTION kyc.fn_validar_beneficiario() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.fecha_nacimiento > current_date THEN
        RAISE EXCEPTION 'SC_BENEFICIARIO_INVALIDO: la fecha de nacimiento % es futura', NEW.fecha_nacimiento
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_beneficiarios_validar BEFORE INSERT ON kyc.beneficiarios
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_validar_beneficiario();

-- Al COMMIT: la versión es exactamente lo que dice ser.
CREATE FUNCTION kyc.comprobar_designacion(p_designacion UUID) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    d       kyc.designaciones_beneficiarios%ROWTYPE;
    v_n     INTEGER;
    v_suma  NUMERIC;
    v_max   INTEGER;
    v_curp  TEXT;
BEGIN
    SELECT * INTO d FROM kyc.designaciones_beneficiarios WHERE id = p_designacion;
    IF d.id IS NULL THEN
        RAISE EXCEPTION 'SC_DESIGNACION_INVALIDA: beneficiarios sin su versión %', p_designacion;
    END IF;
    SELECT count(*), coalesce(sum(porcentaje), 0), coalesce(max(posicion), 0)
      INTO v_n, v_suma, v_max
      FROM kyc.beneficiarios WHERE designacion_id = d.id;
    IF v_n <> d.num_beneficiarios OR v_max <> v_n THEN
        RAISE EXCEPTION 'SC_DESIGNACION_INVALIDA: la versión % declara % beneficiarios en las posiciones 1..%, y tiene % (posición máxima %)',
            d.version, d.num_beneficiarios, d.num_beneficiarios, v_n, v_max;
    END IF;
    IF v_suma <> d.total_porcentaje THEN
        RAISE EXCEPTION 'SC_BENEFICIARIOS_PORCENTAJE: los porcentajes suman % y la versión declara %', v_suma, d.total_porcentaje
            USING ERRCODE = 'check_violation';
    END IF;
    SELECT curp INTO v_curp FROM kyc.clientes WHERE id = d.cliente_id;
    IF v_curp IS NOT NULL AND EXISTS (
        SELECT 1 FROM kyc.beneficiarios WHERE designacion_id = d.id AND curp = v_curp) THEN
        RAISE EXCEPTION 'SC_BENEFICIARIO_TITULAR: el titular no puede ser su propio beneficiario'
            USING ERRCODE = 'check_violation';
    END IF;
    IF d.hash <> nucleo.sha256_hex(kyc.payload_designacion(d)) THEN
        RAISE EXCEPTION 'SC_DESIGNACION_INVALIDA: el hash de la versión % no cubre sus beneficiarios', d.version;
    END IF;
END
$$;
REVOKE ALL ON FUNCTION kyc.comprobar_designacion(UUID) FROM PUBLIC;

CREATE FUNCTION kyc.fn_comprobar_designacion() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_TABLE_NAME = 'designaciones_beneficiarios' THEN
        PERFORM kyc.comprobar_designacion(NEW.id);
    ELSE
        PERFORM kyc.comprobar_designacion(NEW.designacion_id);
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_designaciones_comprobar AFTER INSERT ON kyc.designaciones_beneficiarios
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION kyc.fn_comprobar_designacion();
CREATE CONSTRAINT TRIGGER trg_beneficiarios_comprobar AFTER INSERT ON kyc.beneficiarios
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION kyc.fn_comprobar_designacion();

-- Inmutables: ni se corrigen ni se borran.
CREATE TRIGGER trg_designaciones_inmutable BEFORE UPDATE OR DELETE ON kyc.designaciones_beneficiarios
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_designaciones_no_truncate BEFORE TRUNCATE ON kyc.designaciones_beneficiarios
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_beneficiarios_inmutable BEFORE UPDATE OR DELETE ON kyc.beneficiarios
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_beneficiarios_no_truncate BEFORE TRUNCATE ON kyc.beneficiarios
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- Verificación de la cadena de un cliente (para el DBA y las pruebas): la
-- primera versión que no cuadra, o NULL si todas.
CREATE FUNCTION kyc.verificar_designaciones(p_cliente UUID) RETURNS INTEGER
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    d          kyc.designaciones_beneficiarios%ROWTYPE;
    v_anterior CHAR(64) := nucleo.hash_genesis();
    v_esperada INTEGER := 1;
BEGIN
    FOR d IN SELECT * FROM kyc.designaciones_beneficiarios WHERE cliente_id = p_cliente ORDER BY version LOOP
        IF d.version <> v_esperada OR d.hash_anterior <> v_anterior
           OR d.hash <> nucleo.sha256_hex(kyc.payload_designacion(d)) THEN
            RETURN d.version;
        END IF;
        v_anterior := d.hash;
        v_esperada := v_esperada + 1;
    END LOOP;
    RETURN NULL;
END
$$;
REVOKE ALL ON FUNCTION kyc.verificar_designaciones(UUID) FROM PUBLIC;

-- -----------------------------------------------------------------------------
-- 4. Privilegios y RLS
-- -----------------------------------------------------------------------------
-- sc_kyc guarda versiones: la versión, la cadena y la fecha las pone la base de datos.
GRANT SELECT ON kyc.designaciones_beneficiarios, kyc.beneficiarios TO sc_kyc;
GRANT INSERT (id, cliente_id, num_beneficiarios, total_porcentaje, sesion_id, ip, dispositivo)
    ON kyc.designaciones_beneficiarios TO sc_kyc;
GRANT INSERT (designacion_id, posicion, nombre_completo, parentesco, curp, fecha_nacimiento, porcentaje)
    ON kyc.beneficiarios TO sc_kyc;

-- El cliente lee lo suyo y nada más.
GRANT SELECT ON kyc.designaciones_beneficiarios, kyc.beneficiarios TO sc_cliente;

ALTER TABLE kyc.designaciones_beneficiarios ENABLE ROW LEVEL SECURITY;
ALTER TABLE kyc.beneficiarios ENABLE ROW LEVEL SECURITY;
CREATE POLICY por_privilegios ON kyc.designaciones_beneficiarios USING (true) WITH CHECK (true);
CREATE POLICY por_privilegios ON kyc.beneficiarios USING (true) WITH CHECK (true);
CREATE POLICY solo_del_cliente ON kyc.designaciones_beneficiarios AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual()) WITH CHECK (cliente_id = nucleo.cliente_actual());
CREATE POLICY solo_del_cliente ON kyc.beneficiarios AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (designacion_id IN (SELECT id FROM kyc.designaciones_beneficiarios WHERE cliente_id = nucleo.cliente_actual()))
    WITH CHECK (false);

-- -----------------------------------------------------------------------------
-- 5. Bitácora de seguridad: cada versión, también en la del cliente
-- -----------------------------------------------------------------------------
-- La bitácora es inmutable (filas); cambiar su catálogo es DDL y no la toca.
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
    'BENEFICIARIOS_DESIGNADOS'));

-- Uno por versión.
CREATE UNIQUE INDEX ux_eventos_autenticacion_designacion ON kyc.eventos_autenticacion (cliente_id, detalle)
    WHERE evento = 'BENEFICIARIOS_DESIGNADOS';

-- Corre con el rol de quien guarda la versión (sc_kyc, o el DBA), que queda en
-- `registrado_por`; si la versión no llega al COMMIT, su evento tampoco.
CREATE FUNCTION kyc.fn_auditar_designacion() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, sesion_id, dispositivo, detalle)
    VALUES (NEW.cliente_id, 'BENEFICIARIOS_DESIGNADOS', NEW.ip, NEW.sesion_id, NEW.dispositivo, NEW.version::text);
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_designaciones_auditar AFTER INSERT ON kyc.designaciones_beneficiarios
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_auditar_designacion();

-- sc_kyc escribe en la bitácora este evento y ningún otro: el de una versión
-- que existe, con su misma sesión, IP y dispositivo.
GRANT INSERT (cliente_id, evento, ip, sesion_id, dispositivo, detalle) ON kyc.eventos_autenticacion TO sc_kyc;
CREATE POLICY solo_designaciones ON kyc.eventos_autenticacion AS RESTRICTIVE FOR INSERT TO sc_kyc
    WITH CHECK (evento = 'BENEFICIARIOS_DESIGNADOS' AND EXISTS (
        SELECT 1 FROM kyc.designaciones_beneficiarios d
         WHERE d.cliente_id = eventos_autenticacion.cliente_id
           AND d.version::text = eventos_autenticacion.detalle
           AND d.sesion_id IS NOT DISTINCT FROM eventos_autenticacion.sesion_id
           AND d.ip IS NOT DISTINCT FROM eventos_autenticacion.ip
           AND d.dispositivo IS NOT DISTINCT FROM eventos_autenticacion.dispositivo));
