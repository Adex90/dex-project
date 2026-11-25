-- =============================================================================
--  Synthetic Capital — 0025: aclaraciones SPEI
-- =============================================================================
--
--  El cliente abre una aclaración sobre uno de sus SPEI (un depósito abonado o
--  devuelto al emisor, o una orden de envío) cuando se retrasa, no llega o
--  regresa; el operador SPEI la revisa y la resuelve o la rechaza con una nota
--  que el cliente lee. La Ley para la Transparencia y Ordenamiento de los
--  Servicios Financieros (art. 23) da al cliente 90 días naturales desde la
--  operación para presentarla.
--
--  1. `ifpe.aclaraciones_spei`: cada aclaración, con su folio por cliente (lo
--     pone la base de datos: el anterior más uno), el movimiento (una orden de
--     `ifpe.spei_outbound` o un registro de `ifpe.spei_inbound`), su clave de
--     rastreo (copiada del movimiento), el motivo, la descripción del cliente,
--     quién la abrió (la sesión de 0019, la IP y el dispositivo), el estado y
--     la nota de resolución.
--  2. Al abrirla, la base de datos comprueba que nace ABIERTA y sin nota; que
--     el cliente tiene KYC VERIFICADO; que el movimiento es suyo (la orden es
--     suya; el depósito llegó, o se devolvió, a su cuenta; la devolución de
--     una orden se aclara en la orden); que tiene a lo más 90 días; que no hay
--     otra aclaración activa (ABIERTA o EN_REVISION) del mismo movimiento (y
--     un índice único parcial lo garantiza); y que el cliente tiene a lo más 10
--     activas. Las aclaraciones de un cliente se abren de una en una (candado
--     consultivo): el folio y los máximos no se cruzan.
--  3. Estados: ABIERTA -> EN_REVISION -> RESUELTA | RECHAZADA; una ABIERTA
--     también se cierra directamente. RESUELTA y RECHAZADA son finales y
--     llevan nota (de 10 a 500 caracteres). La nota la lee el cliente: nunca
--     nombra la prevención de lavado de dinero, la UIF, un congelamiento, una
--     alerta, una sospecha ni un riesgo (aviso indebido, art. 115 de la Ley de
--     Instituciones de Crédito y sus disposiciones). Los términos no cambian y
--     nada se borra.
--  4. Bitácora: abrirla escribe ACLARACION_SPEI_ABIERTA en la bitácora
--     inmutable del cliente (`kyc.eventos_autenticacion`, 0010/0019) con la
--     sesión, la IP y el dispositivo de quien la abrió; cada cambio de estado,
--     ACLARACION_SPEI_EN_REVISION, _RESUELTA o _RECHAZADA, sin ellos (los de
--     la institución no se muestran al cliente). `detalle` es el folio; uno por
--     folio y estado. Ninguna aclaración ni cambio se guarda sin su evento.
--  5. Roles: abre sc_ifpe (las rutas del cliente), sin elegir folio, estado,
--     clave de rastreo ni fechas; cambia el estado sc_spei (el operador SPEI),
--     solo el estado y la nota. Cada uno escribe en la bitácora solo sus
--     eventos, los del estado vigente de una aclaración que existe. El cliente
--     lee lo suyo (sc_cliente, RLS como en 0011); el auditor lee todo.
--  6. Abrir una aclaración y cada cambio de estado suben la versión de la
--     bóveda del cliente: el WebSocket avisa y la pantalla se actualiza.
-- =============================================================================

-- Aclaraciones activas (ABIERTA o EN_REVISION) por cliente, a lo más.
CREATE FUNCTION ifpe.maximo_aclaraciones_activas() RETURNS INTEGER
LANGUAGE sql IMMUTABLE PARALLEL SAFE
AS $$ SELECT 10 $$;

-- Días naturales desde la operación para abrir una aclaración (LTOSF, art. 23).
CREATE FUNCTION ifpe.plazo_aclaracion_dias() RETURNS INTEGER
LANGUAGE sql IMMUTABLE PARALLEL SAFE
AS $$ SELECT 90 $$;

-- Una nota que lee el cliente no nombra la prevención de lavado de dinero:
-- ni sus siglas (PLD, AML, UIF, PEP) ni alertas, cumplimiento, sospechas,
-- congelamientos, medidas preventivas, investigaciones, riesgos, retenciones
-- u operaciones inusuales, en español o en inglés.
CREATE FUNCTION ifpe.nota_neutral(p_nota TEXT) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE PARALLEL SAFE
AS $$
    SELECT p_nota !~* ('(\maml\M|\mpld\M|\muif\M|\mpep\M|alerta|\malerts?\M|cumplimiento|compliance|sospech|'
                       || 'suspicious|congel|frozen|freez|preventiv|investig|riesgo|\mrisks?\M|retenid|inusual)')
$$;

-- -----------------------------------------------------------------------------
-- 1. Las aclaraciones
-- -----------------------------------------------------------------------------
CREATE TABLE ifpe.aclaraciones_spei (
    id                 UUID PRIMARY KEY,
    cliente_id         UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    -- Folio por cliente: lo pone la base de datos (el anterior más uno).
    numero             INTEGER NOT NULL CONSTRAINT ck_aclaracion_numero CHECK (numero BETWEEN 1 AND 999999),
    -- El movimiento: una orden de envío o un SPEI recibido, uno de los dos.
    spei_outbound_id   UUID REFERENCES ifpe.spei_outbound (id) ON DELETE RESTRICT,
    spei_inbound_id    UUID REFERENCES ifpe.spei_inbound (id) ON DELETE RESTRICT,
    -- La del movimiento: la copia la base de datos.
    clave_rastreo      VARCHAR(30) NOT NULL CONSTRAINT ck_aclaracion_rastreo CHECK (clave_rastreo ~ '^[A-Z0-9]{1,30}$'),
    motivo             TEXT NOT NULL CONSTRAINT ck_aclaracion_motivo CHECK (motivo IN (
                           'RETRASO', 'NO_RECIBIDO', 'DEVOLUCION', 'MONTO_INCORRECTO', 'NO_RECONOCIDO', 'OTRO')),
    -- Lo que cuenta el cliente (obligatorio con OTRO).
    descripcion        TEXT CONSTRAINT ck_aclaracion_descripcion CHECK (char_length(btrim(descripcion)) BETWEEN 1 AND 500),
    estado             TEXT NOT NULL DEFAULT 'ABIERTA' CONSTRAINT ck_aclaracion_estado CHECK (estado IN (
                           'ABIERTA', 'EN_REVISION', 'RESUELTA', 'RECHAZADA')),
    -- Para el cliente: la escribe el operador.
    nota_resolucion    TEXT CONSTRAINT ck_aclaracion_nota CHECK (
                           char_length(nota_resolucion) BETWEEN 10 AND 500 AND ifpe.nota_neutral(nota_resolucion)),
    clave_idempotencia TEXT NOT NULL UNIQUE,
    -- Quién la abrió: la sesión (0019), su IP y su dispositivo.
    sesion_id          UUID REFERENCES kyc.sesiones (id) ON DELETE RESTRICT,
    ip                 INET,
    dispositivo        TEXT CONSTRAINT ck_aclaracion_dispositivo CHECK (char_length(dispositivo) BETWEEN 1 AND 80),
    -- Las pone la base de datos.
    creada_en          TIMESTAMPTZ NOT NULL DEFAULT now(),
    actualizada_en     TIMESTAMPTZ NOT NULL DEFAULT now(),
    cerrada_en         TIMESTAMPTZ,

    CONSTRAINT ux_aclaraciones_numero UNIQUE (cliente_id, numero),
    CONSTRAINT ck_aclaracion_movimiento CHECK ((spei_outbound_id IS NULL) <> (spei_inbound_id IS NULL)),
    CONSTRAINT ck_aclaracion_otro CHECK (motivo <> 'OTRO' OR coalesce(char_length(btrim(descripcion)), 0) >= 10),
    CONSTRAINT ck_aclaracion_cierre CHECK ((estado IN ('RESUELTA', 'RECHAZADA')) = (cerrada_en IS NOT NULL)),
    CONSTRAINT ck_aclaracion_cierre_con_nota CHECK (estado NOT IN ('RESUELTA', 'RECHAZADA') OR nota_resolucion IS NOT NULL)
);

-- Una aclaración activa por movimiento.
CREATE UNIQUE INDEX ux_aclaraciones_activa_outbound ON ifpe.aclaraciones_spei (spei_outbound_id)
    WHERE estado IN ('ABIERTA', 'EN_REVISION');
CREATE UNIQUE INDEX ux_aclaraciones_activa_inbound ON ifpe.aclaraciones_spei (spei_inbound_id)
    WHERE estado IN ('ABIERTA', 'EN_REVISION');
CREATE INDEX ix_aclaraciones_cliente ON ifpe.aclaraciones_spei (cliente_id, creada_en DESC);
-- La cola del operador.
CREATE INDEX ix_aclaraciones_activas ON ifpe.aclaraciones_spei (creada_en)
    WHERE estado IN ('ABIERTA', 'EN_REVISION');

COMMENT ON TABLE ifpe.aclaraciones_spei IS
    'Aclaraciones de SPEI del cliente (0025): folio, movimiento, motivo, estado y nota; la bitácora guarda cada paso.';

-- Al abrirla: nace ABIERTA, de un cliente verificado, sobre un movimiento suyo
-- de a lo más 90 días, sin otra activa del mismo movimiento y dentro del
-- máximo. Lee las órdenes, los depósitos y el expediente: SECURITY DEFINER, sin
-- dar esas lecturas a quien la abre.
CREATE FUNCTION ifpe.fn_validar_aclaracion() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_estatus TEXT;
    v_cliente UUID;
    v_rastreo TEXT;
    v_fecha   TIMESTAMPTZ;
    v_estado  TEXT;
BEGIN
    IF NEW.estado IS DISTINCT FROM 'ABIERTA' OR NEW.nota_resolucion IS NOT NULL THEN
        RAISE EXCEPTION 'SC_ACLARACION_INCOHERENTE: una aclaración nace ABIERTA y sin nota de resolución';
    END IF;
    IF (NEW.spei_outbound_id IS NULL) = (NEW.spei_inbound_id IS NULL) THEN
        RAISE EXCEPTION 'SC_ACLARACION_INCOHERENTE: una aclaración es de un movimiento: una orden o un SPEI recibido';
    END IF;
    IF NEW.sesion_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM kyc.sesiones s WHERE s.id = NEW.sesion_id AND s.cliente_id = NEW.cliente_id) THEN
        RAISE EXCEPTION 'SC_ACLARACION_INCOHERENTE: la sesión que abre la aclaración no es del cliente';
    END IF;
    -- Las de un cliente, de una en una: el folio y los máximos no se cruzan.
    PERFORM pg_advisory_xact_lock(hashtextextended('ifpe.aclaraciones_spei:' || NEW.cliente_id::text, 0));

    SELECT c.estatus_kyc INTO v_estatus FROM kyc.clientes c WHERE c.id = NEW.cliente_id;
    IF v_estatus IS DISTINCT FROM 'VERIFICADO' THEN
        RAISE EXCEPTION 'SC_KYC_NO_VERIFICADO: solo un cliente con identidad verificada abre una aclaración';
    END IF;

    IF NEW.spei_outbound_id IS NOT NULL THEN
        SELECT o.cliente_id, o.clave_rastreo, o.creado_en INTO v_cliente, v_rastreo, v_fecha
          FROM ifpe.spei_outbound o WHERE o.id = NEW.spei_outbound_id;
    ELSIF NEW.spei_inbound_id IS NOT NULL THEN
        SELECT c.cliente_id, i.clave_rastreo, i.recibido_en, i.estado INTO v_cliente, v_rastreo, v_fecha, v_estado
          FROM ifpe.spei_inbound i JOIN ifpe.cuentas c ON c.id = i.cuenta_id
         WHERE i.id = NEW.spei_inbound_id;
        -- La devolución de una orden nuestra se aclara en la orden.
        IF v_estado IS DISTINCT FROM 'ABONADO' AND v_estado IS DISTINCT FROM 'DEVUELTO' THEN
            v_cliente := NULL;
        END IF;
    END IF;
    IF v_cliente IS NULL OR v_cliente <> NEW.cliente_id THEN
        RAISE EXCEPTION 'SC_ACLARACION_AJENA: el movimiento SPEI no es del cliente';
    END IF;
    IF v_fecha < now() - make_interval(days => ifpe.plazo_aclaracion_dias()) THEN
        RAISE EXCEPTION 'SC_ACLARACION_VENCIDA: una aclaración se abre dentro de los % días naturales siguientes a la operación',
            ifpe.plazo_aclaracion_dias();
    END IF;
    IF EXISTS (SELECT 1 FROM ifpe.aclaraciones_spei a
                WHERE a.estado IN ('ABIERTA', 'EN_REVISION')
                  AND (a.spei_outbound_id = NEW.spei_outbound_id OR a.spei_inbound_id = NEW.spei_inbound_id)) THEN
        RAISE EXCEPTION 'SC_ACLARACION_ACTIVA: el SPEI % ya tiene una aclaración activa', v_rastreo;
    END IF;
    IF (SELECT count(*) FROM ifpe.aclaraciones_spei a
         WHERE a.cliente_id = NEW.cliente_id AND a.estado IN ('ABIERTA', 'EN_REVISION'))
       >= ifpe.maximo_aclaraciones_activas() THEN
        RAISE EXCEPTION 'SC_LIMITE_ACLARACIONES: a lo más % aclaraciones activas por cliente',
            ifpe.maximo_aclaraciones_activas();
    END IF;

    NEW.numero := (SELECT coalesce(max(a.numero), 0) + 1 FROM ifpe.aclaraciones_spei a WHERE a.cliente_id = NEW.cliente_id);
    NEW.clave_rastreo := v_rastreo;
    NEW.creada_en := now();
    NEW.actualizada_en := now();
    NEW.cerrada_en := NULL;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_aclaraciones_a_validar BEFORE INSERT ON ifpe.aclaraciones_spei
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_validar_aclaracion();

-- Los cambios: solo el estado (y la nota), por la máquina de estados; los
-- términos no cambian, ni para el dueño del esquema; cerrada, no cambia más.
CREATE FUNCTION ifpe.fn_transicion_aclaracion() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF (NEW.id, NEW.cliente_id, NEW.numero, NEW.spei_outbound_id, NEW.spei_inbound_id, NEW.clave_rastreo, NEW.motivo,
        NEW.descripcion, NEW.clave_idempotencia, NEW.sesion_id, NEW.ip, NEW.dispositivo, NEW.creada_en)
       IS DISTINCT FROM
       (OLD.id, OLD.cliente_id, OLD.numero, OLD.spei_outbound_id, OLD.spei_inbound_id, OLD.clave_rastreo, OLD.motivo,
        OLD.descripcion, OLD.clave_idempotencia, OLD.sesion_id, OLD.ip, OLD.dispositivo, OLD.creada_en) THEN
        RAISE EXCEPTION 'SC_ACLARACION_INMUTABLE: los términos de la aclaración % no cambian', OLD.numero;
    END IF;
    IF OLD.estado IN ('RESUELTA', 'RECHAZADA') THEN
        RAISE EXCEPTION 'SC_ACLARACION_CERRADA: la aclaración % ya se cerró (%)', OLD.numero, OLD.estado;
    END IF;
    IF NOT ((OLD.estado = 'ABIERTA' AND NEW.estado IN ('EN_REVISION', 'RESUELTA', 'RECHAZADA'))
            OR (OLD.estado = 'EN_REVISION' AND NEW.estado IN ('RESUELTA', 'RECHAZADA'))) THEN
        RAISE EXCEPTION 'SC_ACLARACION_TRANSICION: la aclaración % no pasa de % a %', OLD.numero, OLD.estado, NEW.estado;
    END IF;
    NEW.nota_resolucion := nullif(btrim(NEW.nota_resolucion), '');
    IF (NEW.estado IN ('RESUELTA', 'RECHAZADA') AND NEW.nota_resolucion IS NULL)
       OR (NEW.nota_resolucion IS NOT NULL
           AND (char_length(NEW.nota_resolucion) NOT BETWEEN 10 AND 500 OR NOT ifpe.nota_neutral(NEW.nota_resolucion))) THEN
        RAISE EXCEPTION 'SC_ACLARACION_NOTA: la nota la lee el cliente: de 10 a 500 caracteres (obligatoria al cerrar), sin nombrar la prevención de lavado de dinero, alertas, congelamientos, sospechas ni riesgos';
    END IF;
    NEW.actualizada_en := now();
    NEW.cerrada_en := CASE WHEN NEW.estado IN ('RESUELTA', 'RECHAZADA') THEN now() END;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_aclaraciones_b_transicion BEFORE UPDATE ON ifpe.aclaraciones_spei
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_transicion_aclaracion();

-- Nada se borra.
CREATE TRIGGER trg_aclaraciones_no_borrar BEFORE DELETE ON ifpe.aclaraciones_spei
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_aclaraciones_no_truncate BEFORE TRUNCATE ON ifpe.aclaraciones_spei
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- -----------------------------------------------------------------------------
-- 2. Bitácora de seguridad: cada aclaración y cada paso, en la del cliente
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
    'BENEFICIARIOS_DESIGNADOS',
    -- 0021: `detalle` es la versión del W-8BEN
    'W8BEN_CERTIFICADO',
    -- 0022: `detalle` son los segundos de la ventana abierta
    'STEP_UP_VERIFICADO',
    'STEP_UP_FALLIDO',
    -- 0025: `detalle` es el folio de la aclaración
    'ACLARACION_SPEI_ABIERTA',
    'ACLARACION_SPEI_EN_REVISION',
    'ACLARACION_SPEI_RESUELTA',
    'ACLARACION_SPEI_RECHAZADA'));

-- Uno por folio y estado.
CREATE UNIQUE INDEX ux_eventos_autenticacion_aclaracion ON kyc.eventos_autenticacion (cliente_id, evento, detalle)
    WHERE evento LIKE 'ACLARACION_SPEI_%';

-- Corre con el rol de quien abre o cambia la aclaración (sc_ifpe, sc_spei, o
-- el DBA), que queda en `registrado_por`; si el cambio no llega al COMMIT, su
-- evento tampoco. Los pasos de la institución van sin sesión, IP ni
-- dispositivo: el cliente no ve los del personal.
CREATE FUNCTION ifpe.fn_auditar_aclaracion() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, sesion_id, dispositivo, detalle)
        VALUES (NEW.cliente_id, 'ACLARACION_SPEI_ABIERTA', NEW.ip, NEW.sesion_id, NEW.dispositivo, NEW.numero::text);
    ELSIF NEW.estado IS DISTINCT FROM OLD.estado THEN
        INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, detalle)
        VALUES (NEW.cliente_id, 'ACLARACION_SPEI_' || NEW.estado, NEW.numero::text);
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_aclaraciones_c_auditar AFTER INSERT OR UPDATE OF estado ON ifpe.aclaraciones_spei
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_auditar_aclaracion();

-- sc_ifpe escribe en la bitácora este evento y ningún otro: el de una
-- aclaración ABIERTA que existe, con su misma sesión, IP y dispositivo.
GRANT INSERT (cliente_id, evento, ip, sesion_id, dispositivo, detalle) ON kyc.eventos_autenticacion TO sc_ifpe;
CREATE POLICY solo_aclaraciones_abiertas ON kyc.eventos_autenticacion AS RESTRICTIVE FOR INSERT TO sc_ifpe
    WITH CHECK (evento = 'ACLARACION_SPEI_ABIERTA' AND EXISTS (
        SELECT 1 FROM ifpe.aclaraciones_spei a
         WHERE a.cliente_id = eventos_autenticacion.cliente_id
           AND a.numero::text = eventos_autenticacion.detalle
           AND a.estado = 'ABIERTA'
           AND a.sesion_id IS NOT DISTINCT FROM eventos_autenticacion.sesion_id
           AND a.ip IS NOT DISTINCT FROM eventos_autenticacion.ip
           AND a.dispositivo IS NOT DISTINCT FROM eventos_autenticacion.dispositivo));

-- sc_spei escribe los pasos de la institución y ningún otro evento: el del
-- estado vigente de una aclaración que existe, sin sesión, IP ni dispositivo.
GRANT INSERT (cliente_id, evento, detalle) ON kyc.eventos_autenticacion TO sc_spei;
CREATE POLICY solo_pasos_de_aclaracion ON kyc.eventos_autenticacion AS RESTRICTIVE FOR INSERT TO sc_spei
    WITH CHECK (evento IN ('ACLARACION_SPEI_EN_REVISION', 'ACLARACION_SPEI_RESUELTA', 'ACLARACION_SPEI_RECHAZADA')
        AND sesion_id IS NULL AND ip IS NULL AND dispositivo IS NULL
        AND EXISTS (
            SELECT 1 FROM ifpe.aclaraciones_spei a
             WHERE a.cliente_id = eventos_autenticacion.cliente_id
               AND a.numero::text = eventos_autenticacion.detalle
               AND 'ACLARACION_SPEI_' || a.estado = eventos_autenticacion.evento));

-- -----------------------------------------------------------------------------
-- 3. La bóveda avisa
-- -----------------------------------------------------------------------------
-- Sube la versión de la bóveda del cliente: el WebSocket empuja una foto nueva
-- y la pantalla vuelve a leer sus aclaraciones. SECURITY DEFINER: quien abre o
-- cambia una aclaración no actualiza cuentas.
CREATE FUNCTION ifpe.fn_avisar_boveda_aclaracion() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    UPDATE ifpe.cuentas SET version_ordenes = version_ordenes + 1 WHERE cliente_id = NEW.cliente_id AND moneda = 'MXN';
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_aclaraciones_d_avisar AFTER INSERT OR UPDATE OF estado ON ifpe.aclaraciones_spei
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_avisar_boveda_aclaracion();

-- -----------------------------------------------------------------------------
-- 4. Privilegios y RLS
-- -----------------------------------------------------------------------------
REVOKE ALL ON ifpe.aclaraciones_spei FROM PUBLIC;
REVOKE ALL ON FUNCTION ifpe.fn_validar_aclaracion() FROM PUBLIC;
REVOKE ALL ON FUNCTION ifpe.fn_avisar_boveda_aclaracion() FROM PUBLIC;

-- sc_ifpe abre: los términos y quién la abrió; el folio, el estado, la clave
-- de rastreo y las fechas los pone la base de datos.
GRANT SELECT ON ifpe.aclaraciones_spei TO sc_ifpe;
GRANT INSERT (id, cliente_id, spei_outbound_id, spei_inbound_id, motivo, descripcion, clave_idempotencia, sesion_id,
              ip, dispositivo) ON ifpe.aclaraciones_spei TO sc_ifpe;
-- sc_spei (el operador SPEI) cambia el estado y la nota.
GRANT SELECT ON ifpe.aclaraciones_spei TO sc_spei;
GRANT UPDATE (estado, nota_resolucion) ON ifpe.aclaraciones_spei TO sc_spei;
-- El cliente lee lo suyo; el auditor, todo.
GRANT SELECT ON ifpe.aclaraciones_spei TO sc_cliente, sc_auditor;

ALTER TABLE ifpe.aclaraciones_spei ENABLE ROW LEVEL SECURITY;
CREATE POLICY por_privilegios ON ifpe.aclaraciones_spei USING (true) WITH CHECK (true);
CREATE POLICY solo_del_cliente ON ifpe.aclaraciones_spei AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual()) WITH CHECK (cliente_id = nucleo.cliente_actual());
