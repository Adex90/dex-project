-- =============================================================================
--  Synthetic Capital — 0005: integración SPEI (STP / Banxico)
-- =============================================================================
--
--  Transferencias interbancarias reales sobre la bóveda IFPE. Todo vive en el
--  esquema `ifpe`: la SOFOM no ve ni mueve SPEI (segregación estricta).
--
--  1. Rol `sc_spei`: el único que registra SPEI recibidos, cambia el estado de
--     las órdenes y reembolsa. Lo usan solo los webhooks firmados, el
--     despachador y el operador. `sc_ifpe` (todas las rutas del cliente) solo
--     puede crear órdenes de envío: no puede fabricar un abono ni un reembolso.
--  2. `ifpe.clabes_asignadas`: CLABE (18 dígitos, dígito verificador módulo 10)
--     -> cuenta IFPE. Una CLABE pertenece a una sola cuenta para siempre (nunca
--     se recicla) y la cuenta no cambia de dueño.
--  3. `ifpe.spei_outbound`: órdenes de envío. Al crearse, saldo del cliente y
--     reserva bajan juntos (RETIRO). Clave de rastreo única globalmente (no
--     solo por día): un "duplicado" del proveedor siempre significa "ya se
--     envió". La fecha de operación la asigna el proveedor y se guarda aparte
--     de la fecha de solicitud.
--  4. `ifpe.spei_inbound`: bitácora inmutable de SPEI recibidos. Un pago se
--     identifica por participante emisor + clave de rastreo + fecha de
--     operación (Banxico exige rastreo único por emisor y día). Abonados,
--     devueltos al emisor, o devoluciones de nuestras propias órdenes.
--  5. `ifpe.spei_cuarentena`: notificaciones autenticadas que no se pudieron
--     interpretar (se devuelven al emisor y quedan para revisión).
--  6. `ifpe.spei_correcciones`: corrección de un abono erróneo (cargo al
--     cliente y a la reserva ligado al abono original, hasta su monto).
--  7. Lista blanca en el ledger: todo DEPOSITO nuevo es un SPEI registrado o el
--     reverso de una orden; todo RETIRO nuevo es una orden SPEI o una
--     corrección. Cada movimiento debe crearse en la MISMA transacción que su
--     registro (mismo xmin): nadie puede "adoptar" un movimiento anterior.
--
--  La reserva del 100% no cambia: SC_RESERVA_100 y SC_SALDO_SIN_LEDGER siguen
--  rechazando al COMMIT cualquier abono o cargo que no mueva la reserva en
--  custodia por el mismo monto.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. Rol sc_spei y permisos más estrechos sobre saldos
-- -----------------------------------------------------------------------------
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sc_spei') THEN
        CREATE ROLE sc_spei NOLOGIN;
    END IF;
    EXECUTE format('GRANT sc_spei TO %I', current_user);
END
$$;
GRANT USAGE ON SCHEMA nucleo, kyc, ifpe TO sc_spei;
GRANT SELECT ON kyc.clientes TO sc_spei;
GRANT INSERT ON kyc.alertas_pld TO sc_spei;
GRANT SELECT ON ifpe.cuentas, ifpe.reserva_custodia, ifpe.estado_reserva, ifpe.ledger_cabeza TO sc_spei;
GRANT SELECT, INSERT ON ifpe.transacciones TO sc_spei;

-- Hasta 0004, sc_ifpe y sc_puente podían actualizar cualquier columna de las
-- cuentas (incluido cliente_id: cambiar de dueño una cuenta con su saldo y su
-- CLABE) y de la reserva (tipo, activa). Ahora solo los saldos (y el estado).
REVOKE UPDATE ON ifpe.cuentas FROM sc_ifpe, sc_puente;
REVOKE UPDATE ON ifpe.reserva_custodia FROM sc_ifpe, sc_puente;
GRANT UPDATE (saldo_disponible, saldo_bloqueado, estado) ON ifpe.cuentas TO sc_ifpe;
GRANT UPDATE (saldo_disponible) ON ifpe.cuentas TO sc_puente, sc_spei;
GRANT UPDATE (saldo) ON ifpe.reserva_custodia TO sc_ifpe, sc_puente, sc_spei;

-- La identidad de una cuenta (dueño, CLABE, moneda) no cambia, ni siquiera
-- para el dueño del esquema.
CREATE FUNCTION ifpe.fn_identidad_de_cuenta_inmutable() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.cliente_id <> OLD.cliente_id OR NEW.clabe <> OLD.clabe OR NEW.moneda <> OLD.moneda
       OR NEW.tipo_entidad <> OLD.tipo_entidad THEN
        RAISE EXCEPTION 'SC_CUENTA_INMUTABLE: la cuenta % no cambia de dueño, CLABE ni moneda', OLD.id;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_ifpe_cuentas_identidad_inmutable
    BEFORE UPDATE OF cliente_id, clabe, moneda, tipo_entidad ON ifpe.cuentas
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_identidad_de_cuenta_inmutable();

-- -----------------------------------------------------------------------------
-- 2. CLABEs asignadas
-- -----------------------------------------------------------------------------
CREATE TABLE ifpe.clabes_asignadas (
    clabe          CHAR(18) PRIMARY KEY CHECK (nucleo.clabe_valida(clabe)),
    cuenta_id      UUID NOT NULL REFERENCES ifpe.cuentas (id) ON DELETE RESTRICT,
    -- Quién enruta la CLABE: 'INTERNA' (participante directo) o el socio
    -- liquidador ('STP' para CLABEs 646...).
    proveedor      TEXT NOT NULL CHECK (proveedor ~ '^[A-Z][A-Z0-9_]{1,19}$'),
    principal      BOOLEAN NOT NULL DEFAULT FALSE,
    activa         BOOLEAN NOT NULL DEFAULT TRUE,
    creado_en      TIMESTAMPTZ NOT NULL DEFAULT now(),
    actualizado_en TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX ux_clabes_asignadas_principal ON ifpe.clabes_asignadas (cuenta_id) WHERE principal;
CREATE INDEX ix_clabes_asignadas_cuenta ON ifpe.clabes_asignadas (cuenta_id);

-- Solo `activa` puede cambiar: la CLABE nunca cambia de dueño ni de proveedor.
CREATE FUNCTION ifpe.fn_proteger_clabe() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'SC_CLABE_INMUTABLE: una CLABE asignada no se borra (se desactiva)';
    END IF;
    IF NEW.clabe <> OLD.clabe OR NEW.cuenta_id <> OLD.cuenta_id OR NEW.proveedor <> OLD.proveedor
       OR NEW.principal <> OLD.principal OR NEW.creado_en <> OLD.creado_en THEN
        RAISE EXCEPTION 'SC_CLABE_INMUTABLE: la CLABE % no puede reasignarse', OLD.clabe;
    END IF;
    NEW.actualizado_en := now();
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_clabes_asignadas_proteger BEFORE UPDATE OR DELETE ON ifpe.clabes_asignadas
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_proteger_clabe();
CREATE TRIGGER trg_clabes_asignadas_no_truncate BEFORE TRUNCATE ON ifpe.clabes_asignadas
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- El proveedor se deduce del código de banco de la CLABE (646 = STP).
CREATE FUNCTION ifpe.proveedor_de_clabe(p_clabe TEXT) RETURNS TEXT
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
AS $$ SELECT CASE substr(p_clabe, 1, 3) WHEN '646' THEN 'STP' ELSE 'INTERNA' END $$;

-- La CLABE principal de cada cuenta nueva queda asignada en la misma
-- transacción en que se abre la cuenta (la abre sc_kyc, sin permisos sobre
-- esta tabla: la función corre como dueño del esquema).
CREATE FUNCTION ifpe.fn_asignar_clabe_principal() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    INSERT INTO ifpe.clabes_asignadas (clabe, cuenta_id, proveedor, principal)
    VALUES (NEW.clabe, NEW.id, ifpe.proveedor_de_clabe(NEW.clabe), TRUE);
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_ifpe_cuentas_clabe_principal AFTER INSERT ON ifpe.cuentas
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_asignar_clabe_principal();

INSERT INTO ifpe.clabes_asignadas (clabe, cuenta_id, proveedor, principal, creado_en)
SELECT clabe, id, ifpe.proveedor_de_clabe(clabe), TRUE, creado_en FROM ifpe.cuentas;

-- -----------------------------------------------------------------------------
-- 3. SPEI enviados
-- -----------------------------------------------------------------------------
CREATE SEQUENCE ifpe.spei_rastreo_seq;

CREATE TABLE ifpe.spei_outbound (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cliente_id            UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    cuenta_id             UUID NOT NULL REFERENCES ifpe.cuentas (id) ON DELETE RESTRICT,
    clave_idempotencia    TEXT NOT NULL UNIQUE,
    -- Clave de rastreo que asignamos como emisor: única para siempre.
    clave_rastreo         VARCHAR(30) NOT NULL UNIQUE CHECK (clave_rastreo ~ '^[A-Z0-9]{1,30}$'),
    -- Día (Ciudad de México) en que el cliente pidió el envío.
    fecha_solicitud       DATE NOT NULL,
    -- Fecha de operación que asignó el proveedor (después del corte, en fin de
    -- semana o día inhábil es el siguiente día hábil). Se fija una sola vez.
    fecha_operacion       DATE,
    -- Ordenante: el cliente (lo exigen las reglas SPEI y PLD).
    ordenante_nombre      TEXT NOT NULL CHECK (length(ordenante_nombre) BETWEEN 1 AND 40),
    ordenante_clabe       CHAR(18) NOT NULL CHECK (nucleo.clabe_valida(ordenante_clabe)),
    ordenante_rfc_curp    TEXT CHECK (ordenante_rfc_curp ~ '^[A-Z0-9&Ñ]{12,18}$'),
    -- Beneficiario (CLABE externa con dígito verificador válido).
    banco_receptor        CHAR(3) NOT NULL CHECK (banco_receptor ~ '^[0-9]{3}$'),
    beneficiario_clabe    CHAR(18) NOT NULL CHECK (nucleo.clabe_valida(beneficiario_clabe)),
    beneficiario_nombre   TEXT NOT NULL CHECK (length(beneficiario_nombre) BETWEEN 1 AND 40),
    beneficiario_rfc_curp TEXT CHECK (beneficiario_rfc_curp ~ '^[A-Z0-9&Ñ]{12,18}$'),
    monto                 NUMERIC(18, 8) NOT NULL CHECK (monto > 0 AND monto = round(monto, 2)),
    -- Juego de caracteres que aceptan los participantes (sin acentos ni ñ).
    concepto              TEXT NOT NULL CHECK (concepto ~ '^[A-Za-z0-9 .,/-]{1,40}$'),
    referencia_numerica   INTEGER NOT NULL CHECK (referencia_numerica BETWEEN 1 AND 9999999),
    -- PENDIENTE -> ENVIADA -> LIQUIDADA [-> DEVUELTA]; PENDIENTE/ENVIADA ->
    -- RECHAZADA. RECHAZADA y DEVUELTA llevan reverso.
    estado                TEXT NOT NULL DEFAULT 'PENDIENTE'
                              CHECK (estado IN ('PENDIENTE', 'ENVIADA', 'LIQUIDADA', 'RECHAZADA', 'DEVUELTA')),
    tx_retiro_id          UUID NOT NULL UNIQUE REFERENCES ifpe.transacciones (id) ON DELETE RESTRICT,
    tx_reverso_id         UUID UNIQUE REFERENCES ifpe.transacciones (id) ON DELETE RESTRICT,
    id_proveedor          TEXT CHECK (length(id_proveedor) <= 64),
    -- Intentos iniciados con el proveedor (se cuentan ANTES de llamarlo).
    intentos_envio        INTEGER NOT NULL DEFAULT 0 CHECK (intentos_envio >= 0),
    ultimo_error          TEXT,
    revision_manual       BOOLEAN NOT NULL DEFAULT FALSE,
    causa_devolucion      SMALLINT CHECK (causa_devolucion BETWEEN 1 AND 99),
    motivo                TEXT,
    proximo_intento_en    TIMESTAMPTZ NOT NULL DEFAULT now(),
    creado_en             TIMESTAMPTZ NOT NULL DEFAULT now(),
    actualizado_en        TIMESTAMPTZ NOT NULL DEFAULT now(),
    enviada_en            TIMESTAMPTZ,
    liquidada_en          TIMESTAMPTZ,
    finalizada_en         TIMESTAMPTZ,

    CONSTRAINT ck_spei_outbound_banco CHECK (banco_receptor = substr(beneficiario_clabe, 1, 3)),
    CONSTRAINT ck_spei_outbound_reverso CHECK ((estado IN ('RECHAZADA', 'DEVUELTA')) = (tx_reverso_id IS NOT NULL)),
    CONSTRAINT ck_spei_outbound_enviada CHECK (estado NOT IN ('ENVIADA', 'LIQUIDADA', 'DEVUELTA') OR enviada_en IS NOT NULL),
    CONSTRAINT ck_spei_outbound_liquidada CHECK (estado NOT IN ('LIQUIDADA', 'DEVUELTA') OR liquidada_en IS NOT NULL)
);
CREATE INDEX ix_spei_outbound_cliente ON ifpe.spei_outbound (cliente_id, creado_en);
CREATE INDEX ix_spei_outbound_vigilancia ON ifpe.spei_outbound (proximo_intento_en)
    WHERE estado IN ('PENDIENTE', 'ENVIADA', 'LIQUIDADA');
CREATE INDEX ix_spei_outbound_proveedor ON ifpe.spei_outbound (id_proveedor);

-- Máquina de estados e inmutabilidad de la orden.
CREATE FUNCTION ifpe.fn_proteger_spei_outbound() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'SC_SPEI_INMUTABLE: una orden SPEI no se borra';
    END IF;
    IF (NEW.cliente_id, NEW.cuenta_id, NEW.clave_idempotencia, NEW.clave_rastreo, NEW.fecha_solicitud,
        NEW.ordenante_nombre, NEW.ordenante_clabe, NEW.ordenante_rfc_curp, NEW.banco_receptor,
        NEW.beneficiario_clabe, NEW.beneficiario_nombre, NEW.beneficiario_rfc_curp, NEW.monto, NEW.concepto,
        NEW.referencia_numerica, NEW.tx_retiro_id, NEW.creado_en)
       IS DISTINCT FROM
       (OLD.cliente_id, OLD.cuenta_id, OLD.clave_idempotencia, OLD.clave_rastreo, OLD.fecha_solicitud,
        OLD.ordenante_nombre, OLD.ordenante_clabe, OLD.ordenante_rfc_curp, OLD.banco_receptor,
        OLD.beneficiario_clabe, OLD.beneficiario_nombre, OLD.beneficiario_rfc_curp, OLD.monto, OLD.concepto,
        OLD.referencia_numerica, OLD.tx_retiro_id, OLD.creado_en) THEN
        RAISE EXCEPTION 'SC_SPEI_INMUTABLE: los datos de la orden SPEI % no se modifican', OLD.clave_rastreo;
    END IF;
    -- Lo que informa el proveedor se fija una sola vez.
    IF (OLD.tx_reverso_id IS NOT NULL AND NEW.tx_reverso_id IS DISTINCT FROM OLD.tx_reverso_id)
       OR (OLD.fecha_operacion IS NOT NULL AND NEW.fecha_operacion IS DISTINCT FROM OLD.fecha_operacion)
       OR (OLD.id_proveedor IS NOT NULL AND NEW.id_proveedor IS DISTINCT FROM OLD.id_proveedor) THEN
        RAISE EXCEPTION 'SC_SPEI_INMUTABLE: el reverso, la fecha de operación o el id del proveedor de la orden % ya están fijados',
            OLD.clave_rastreo;
    END IF;
    IF NOT (
        (OLD.estado = 'PENDIENTE' AND NEW.estado IN ('PENDIENTE', 'ENVIADA', 'RECHAZADA')) OR
        (OLD.estado = 'ENVIADA'   AND NEW.estado IN ('ENVIADA', 'LIQUIDADA', 'RECHAZADA')) OR
        (OLD.estado = 'LIQUIDADA' AND NEW.estado IN ('LIQUIDADA', 'DEVUELTA'))
    ) THEN
        RAISE EXCEPTION 'SC_SPEI_TRANSICION: la orden % no puede pasar de % a %',
            OLD.clave_rastreo, OLD.estado, NEW.estado;
    END IF;
    NEW.actualizado_en := now();
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_spei_outbound_proteger BEFORE UPDATE OR DELETE ON ifpe.spei_outbound
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_proteger_spei_outbound();
CREATE TRIGGER trg_spei_outbound_no_truncate BEFORE TRUNCATE ON ifpe.spei_outbound
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- La orden está ligada a su cargo (RETIRO) y, si se rechazó o devolvió, a su
-- reverso (DEPOSITO): misma cuenta, mismo monto, misma reserva, claves
-- derivadas de la orden y creados en la misma transacción que la orden (o que
-- el cambio de estado).
CREATE FUNCTION ifpe.fn_verificar_spei_outbound() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    o       ifpe.spei_outbound;
    o_xmin  xid;
    retiro  ifpe.transacciones;
    r_xmin  xid;
    reverso ifpe.transacciones;
    v_xmin  xid;
    c       ifpe.cuentas;
BEGIN
    -- Estado vigente al COMMIT (la fila pudo cambiar después del evento).
    SELECT * INTO o FROM ifpe.spei_outbound WHERE id = NEW.id;
    SELECT x.xmin INTO o_xmin FROM ifpe.spei_outbound x WHERE x.id = NEW.id;
    SELECT * INTO c FROM ifpe.cuentas WHERE id = o.cuenta_id;
    IF c.cliente_id IS DISTINCT FROM o.cliente_id OR c.clabe IS DISTINCT FROM o.ordenante_clabe THEN
        RAISE EXCEPTION 'SC_SPEI_DESCUADRE: la orden % no corresponde al dueño de la cuenta', o.clave_rastreo;
    END IF;
    SELECT * INTO retiro FROM ifpe.transacciones WHERE id = o.tx_retiro_id;
    SELECT x.xmin INTO r_xmin FROM ifpe.transacciones x WHERE x.id = o.tx_retiro_id;
    IF retiro.id IS NULL OR retiro.tipo <> 'RETIRO' OR retiro.cuenta_origen_id <> o.cuenta_id
       OR retiro.monto <> o.monto OR retiro.clave_idempotencia <> o.clave_idempotencia
       OR retiro.referencia_externa IS DISTINCT FROM o.beneficiario_clabe THEN
        RAISE EXCEPTION 'SC_SPEI_DESCUADRE: la orden % no coincide con su cargo en el ledger', o.clave_rastreo;
    END IF;
    -- Al crear la orden, su RETIRO debe ser de la misma transacción: no se
    -- puede adoptar un retiro anterior (p. ej. uno ya pagado antes de 0005).
    IF TG_OP = 'INSERT' AND r_xmin IS DISTINCT FROM o_xmin THEN
        RAISE EXCEPTION 'SC_SPEI_DESCUADRE: el cargo de la orden % no se creó con ella', o.clave_rastreo;
    END IF;
    IF o.tx_reverso_id IS NOT NULL THEN
        SELECT * INTO reverso FROM ifpe.transacciones WHERE id = o.tx_reverso_id;
        SELECT x.xmin INTO v_xmin FROM ifpe.transacciones x WHERE x.id = o.tx_reverso_id;
        IF reverso.id IS NULL OR reverso.tipo <> 'DEPOSITO' OR reverso.cuenta_destino_id <> o.cuenta_id
           OR reverso.monto <> o.monto OR reverso.reserva_id IS DISTINCT FROM retiro.reserva_id
           OR reverso.clave_idempotencia <> 'spei-reverso:' || o.id::text
           OR v_xmin IS DISTINCT FROM o_xmin THEN
            RAISE EXCEPTION 'SC_SPEI_DESCUADRE: el reverso de la orden % no coincide con el ledger', o.clave_rastreo;
        END IF;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_spei_outbound_ledger
    AFTER INSERT OR UPDATE ON ifpe.spei_outbound
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_verificar_spei_outbound();

-- -----------------------------------------------------------------------------
-- 4. SPEI recibidos
-- -----------------------------------------------------------------------------
CREATE TABLE ifpe.spei_inbound (
    id                    UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    -- Identidad del pago.
    institucion_emisora   CHAR(5) NOT NULL CHECK (institucion_emisora ~ '^[0-9]{5}$'),
    clave_rastreo         VARCHAR(30) NOT NULL CHECK (clave_rastreo ~ '^[A-Z0-9]{1,30}$'),
    fecha_operacion       DATE NOT NULL,
    tipo_pago             SMALLINT CHECK (tipo_pago BETWEEN 0 AND 99),
    -- Destino.
    institucion_receptora CHAR(5) CHECK (institucion_receptora ~ '^[0-9]{5}$'),
    receptor_clabe        VARCHAR(20) NOT NULL CHECK (receptor_clabe ~ '^[0-9]{1,20}$'),
    cuenta_id             UUID REFERENCES ifpe.cuentas (id) ON DELETE RESTRICT,
    -- SPEI liquida en centavos.
    monto                 NUMERIC(18, 8) NOT NULL CHECK (monto > 0 AND monto = round(monto, 2)),
    -- Ordenante y datos del pago, tal como los informa el participante.
    nombre_ordenante      TEXT CHECK (length(nombre_ordenante) <= 120),
    cuenta_ordenante      TEXT CHECK (cuenta_ordenante ~ '^[0-9]{1,20}$'),
    rfc_curp_ordenante    TEXT CHECK (length(rfc_curp_ordenante) <= 18),
    referencia_numerica   INTEGER CHECK (referencia_numerica BETWEEN 0 AND 9999999),
    concepto              TEXT CHECK (length(concepto) <= 210),
    -- ABONADO: acreditado al cliente. DEVUELTO: regresado al emisor con causa
    -- Banxico. DEVOLUCION_DE_ORDEN: es la devolución de una orden nuestra (el
    -- reembolso se registra en la orden, una sola vez).
    estado                TEXT NOT NULL CHECK (estado IN ('ABONADO', 'DEVUELTO', 'DEVOLUCION_DE_ORDEN')),
    causa_devolucion      SMALLINT CHECK (causa_devolucion BETWEEN 1 AND 99),
    motivo                TEXT,
    transaccion_id        UUID UNIQUE REFERENCES ifpe.transacciones (id) ON DELETE RESTRICT,
    spei_outbound_id      UUID UNIQUE REFERENCES ifpe.spei_outbound (id) ON DELETE RESTRICT,
    -- Procedencia del registro.
    origen                TEXT NOT NULL CHECK (origen IN ('WEBHOOK', 'OPERADOR', 'DEMO')),
    registrado_por        UUID REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    id_proveedor          TEXT CHECK (length(id_proveedor) <= 64),
    raw_payload           JSONB NOT NULL,
    recibido_en           TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ux_spei_inbound_identidad UNIQUE (institucion_emisora, clave_rastreo, fecha_operacion),
    CONSTRAINT ck_spei_inbound_resultado CHECK (
        (estado = 'ABONADO'  AND transaccion_id IS NOT NULL AND cuenta_id IS NOT NULL
                             AND causa_devolucion IS NULL AND spei_outbound_id IS NULL) OR
        (estado = 'DEVUELTO' AND transaccion_id IS NULL AND causa_devolucion IS NOT NULL AND spei_outbound_id IS NULL) OR
        (estado = 'DEVOLUCION_DE_ORDEN' AND transaccion_id IS NULL AND spei_outbound_id IS NOT NULL)
    ),
    CONSTRAINT ck_spei_inbound_operador CHECK ((origen = 'OPERADOR') = (registrado_por IS NOT NULL))
);
CREATE INDEX ix_spei_inbound_cuenta ON ifpe.spei_inbound (cuenta_id, recibido_en);
CREATE INDEX ix_spei_inbound_rastreo ON ifpe.spei_inbound (clave_rastreo);

-- Bitácora inmutable.
CREATE TRIGGER trg_spei_inbound_inmutable BEFORE UPDATE OR DELETE ON ifpe.spei_inbound
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_spei_inbound_no_truncate BEFORE TRUNCATE ON ifpe.spei_inbound
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- Identidad canónica del pago, la misma que guarda el ledger en
-- referencia_externa y en la clave de idempotencia `spei:<identidad>`.
CREATE FUNCTION ifpe.identidad_spei(p_emisor TEXT, p_fecha DATE, p_rastreo TEXT) RETURNS TEXT
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
AS $$ SELECT p_emisor || ':' || to_char(p_fecha, 'YYYYMMDD') || ':' || p_rastreo $$;

-- Un abono SPEI está ligado a exactamente su DEPOSITO: misma cuenta (dueña de
-- la CLABE receptora activa), mismo monto, misma identidad de pago, creado en
-- la misma transacción.
CREATE FUNCTION ifpe.fn_verificar_spei_inbound() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    t ifpe.transacciones;
    t_xmin xid;
    s_xmin xid;
    identidad TEXT := ifpe.identidad_spei(NEW.institucion_emisora, NEW.fecha_operacion, NEW.clave_rastreo);
BEGIN
    IF NEW.estado <> 'ABONADO' THEN
        RETURN NULL;
    END IF;
    SELECT * INTO t FROM ifpe.transacciones WHERE id = NEW.transaccion_id;
    SELECT x.xmin INTO t_xmin FROM ifpe.transacciones x WHERE x.id = NEW.transaccion_id;
    SELECT x.xmin INTO s_xmin FROM ifpe.spei_inbound x WHERE x.id = NEW.id;
    IF t.id IS NULL OR t.tipo <> 'DEPOSITO' OR t.cuenta_destino_id <> NEW.cuenta_id OR t.monto <> NEW.monto
       OR t.referencia_externa IS DISTINCT FROM identidad OR t.clave_idempotencia <> 'spei:' || identidad
       OR t_xmin IS DISTINCT FROM s_xmin THEN
        RAISE EXCEPTION 'SC_SPEI_DESCUADRE: el SPEI % no coincide con su movimiento del ledger', identidad;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM ifpe.clabes_asignadas
                    WHERE clabe = NEW.receptor_clabe AND cuenta_id = NEW.cuenta_id AND activa) THEN
        RAISE EXCEPTION 'SC_SPEI_DESCUADRE: el SPEI % se abonó a una cuenta que no es dueña de la CLABE %',
            identidad, NEW.receptor_clabe;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_spei_inbound_ledger
    AFTER INSERT ON ifpe.spei_inbound
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_verificar_spei_inbound();

-- Notificaciones autenticadas que no se pudieron interpretar.
CREATE TABLE ifpe.spei_cuarentena (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tipo        TEXT NOT NULL CHECK (tipo IN ('ABONO', 'ESTADO')),
    cuerpo      BYTEA NOT NULL,
    error       TEXT NOT NULL,
    recibido_en TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TRIGGER trg_spei_cuarentena_inmutable BEFORE UPDATE OR DELETE ON ifpe.spei_cuarentena
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- -----------------------------------------------------------------------------
-- 5. Corrección de abonos erróneos
-- -----------------------------------------------------------------------------
CREATE TABLE ifpe.spei_correcciones (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    spei_inbound_id    UUID NOT NULL REFERENCES ifpe.spei_inbound (id) ON DELETE RESTRICT,
    cuenta_id          UUID NOT NULL REFERENCES ifpe.cuentas (id) ON DELETE RESTRICT,
    monto              NUMERIC(18, 8) NOT NULL CHECK (monto > 0 AND monto = round(monto, 2)),
    motivo             TEXT NOT NULL CHECK (length(btrim(motivo)) BETWEEN 5 AND 200),
    autorizado_por     UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    clave_idempotencia TEXT NOT NULL UNIQUE,
    tx_id              UUID NOT NULL UNIQUE REFERENCES ifpe.transacciones (id) ON DELETE RESTRICT,
    creado_en          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TRIGGER trg_spei_correcciones_inmutable BEFORE UPDATE OR DELETE ON ifpe.spei_correcciones
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- La corrección es un RETIRO de esta misma transacción, de la cuenta que
-- recibió el abono, y el total corregido nunca excede el abono.
CREATE FUNCTION ifpe.fn_verificar_spei_correccion() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    abono  ifpe.spei_inbound;
    t      ifpe.transacciones;
    t_xmin xid;
    c_xmin xid;
    total  NUMERIC;
BEGIN
    SELECT * INTO abono FROM ifpe.spei_inbound WHERE id = NEW.spei_inbound_id;
    IF abono.id IS NULL OR abono.estado <> 'ABONADO' OR abono.cuenta_id <> NEW.cuenta_id THEN
        RAISE EXCEPTION 'SC_SPEI_DESCUADRE: solo se corrige un abono de la misma cuenta';
    END IF;
    SELECT * INTO t FROM ifpe.transacciones WHERE id = NEW.tx_id;
    SELECT x.xmin INTO t_xmin FROM ifpe.transacciones x WHERE x.id = NEW.tx_id;
    SELECT x.xmin INTO c_xmin FROM ifpe.spei_correcciones x WHERE x.id = NEW.id;
    IF t.id IS NULL OR t.tipo <> 'RETIRO' OR t.cuenta_origen_id <> NEW.cuenta_id OR t.monto <> NEW.monto
       OR t.clave_idempotencia <> 'spei-correccion:' || NEW.id::text OR t_xmin IS DISTINCT FROM c_xmin THEN
        RAISE EXCEPTION 'SC_SPEI_DESCUADRE: la corrección no coincide con su cargo en el ledger';
    END IF;
    SELECT sum(monto) INTO total FROM ifpe.spei_correcciones WHERE spei_inbound_id = NEW.spei_inbound_id;
    IF total > abono.monto THEN
        RAISE EXCEPTION 'SC_SPEI_CORRECCION_EXCEDE: las correcciones (%) exceden el abono (%)', total, abono.monto;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_spei_correcciones_ledger
    AFTER INSERT ON ifpe.spei_correcciones
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_verificar_spei_correccion();

-- -----------------------------------------------------------------------------
-- 6. Lista blanca: ningún efectivo entra o sale sin su registro
-- -----------------------------------------------------------------------------
CREATE FUNCTION ifpe.fn_verificar_tx_spei() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF NEW.tipo = 'DEPOSITO' THEN
        IF NEW.clave_idempotencia LIKE 'spei:%' THEN
            IF EXISTS (SELECT 1 FROM ifpe.spei_inbound WHERE transaccion_id = NEW.id AND estado = 'ABONADO') THEN
                RETURN NULL;
            END IF;
        ELSIF NEW.clave_idempotencia LIKE 'spei-reverso:%' THEN
            IF EXISTS (SELECT 1 FROM ifpe.spei_outbound WHERE tx_reverso_id = NEW.id) THEN
                RETURN NULL;
            END IF;
        END IF;
        RAISE EXCEPTION 'SC_SPEI_SIN_REGISTRO: todo abono de efectivo debe ser un SPEI registrado o el reverso de una orden (clave %)',
            NEW.clave_idempotencia;
    ELSIF NEW.tipo = 'RETIRO' THEN
        IF EXISTS (SELECT 1 FROM ifpe.spei_outbound WHERE tx_retiro_id = NEW.id)
           OR EXISTS (SELECT 1 FROM ifpe.spei_correcciones WHERE tx_id = NEW.id) THEN
            RETURN NULL;
        END IF;
        RAISE EXCEPTION 'SC_SPEI_SIN_REGISTRO: todo retiro de efectivo debe ser una orden SPEI o una corrección (clave %)',
            NEW.clave_idempotencia;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_ifpe_tx_spei_registrado
    AFTER INSERT ON ifpe.transacciones
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN (NEW.tipo IN ('DEPOSITO', 'RETIRO'))
    EXECUTE FUNCTION ifpe.fn_verificar_tx_spei();

-- -----------------------------------------------------------------------------
-- 7. Conciliación con el custodio, por cuenta de reserva
-- -----------------------------------------------------------------------------
-- saldo esperado en el custodio = reserva contable + órdenes cuyo cargo salió
-- de los libros pero no necesariamente del banco. PENDIENTE: aún no se envía
-- (el dinero sigue ahí). ENVIADA: pudo liquidarse ya; se confirma consultando.
CREATE VIEW ifpe.v_spei_conciliacion AS
SELECT r.id AS reserva_id,
       r.institucion,
       r.tipo,
       r.saldo AS reserva_contable,
       coalesce(sum(o.monto) FILTER (WHERE o.estado = 'PENDIENTE'), 0) AS pendiente_de_envio,
       coalesce(sum(o.monto) FILTER (WHERE o.estado = 'ENVIADA'), 0)   AS enviado_sin_confirmar,
       count(o.id) FILTER (WHERE o.estado IN ('PENDIENTE', 'ENVIADA'))  AS ordenes_en_transito,
       count(o.id) FILTER (WHERE o.revision_manual)                     AS ordenes_en_revision,
       r.saldo + coalesce(sum(o.monto) FILTER (WHERE o.estado IN ('PENDIENTE', 'ENVIADA')), 0)
                                                                        AS saldo_esperado_custodio
FROM ifpe.reserva_custodia r
LEFT JOIN ifpe.transacciones t ON t.reserva_id = r.id AND t.tipo = 'RETIRO'
LEFT JOIN ifpe.spei_outbound o ON o.tx_retiro_id = t.id AND o.estado IN ('PENDIENTE', 'ENVIADA')
GROUP BY r.id, r.institucion, r.tipo, r.saldo;

-- -----------------------------------------------------------------------------
-- 8. Permisos
-- -----------------------------------------------------------------------------
GRANT SELECT ON ifpe.clabes_asignadas, ifpe.spei_inbound, ifpe.spei_outbound, ifpe.v_spei_conciliacion
    TO sc_ifpe, sc_spei, sc_auditor;
GRANT SELECT ON ifpe.spei_correcciones, ifpe.spei_cuarentena TO sc_spei, sc_auditor;
GRANT INSERT (clabe, cuenta_id, proveedor) ON ifpe.clabes_asignadas TO sc_spei;
GRANT UPDATE (activa) ON ifpe.clabes_asignadas TO sc_spei;

-- sc_ifpe (rutas del cliente) solo crea órdenes PENDIENTE.
GRANT INSERT (id, cliente_id, cuenta_id, clave_idempotencia, clave_rastreo, fecha_solicitud, ordenante_nombre,
              ordenante_clabe, ordenante_rfc_curp, banco_receptor, beneficiario_clabe, beneficiario_nombre,
              beneficiario_rfc_curp, monto, concepto, referencia_numerica, tx_retiro_id)
    ON ifpe.spei_outbound TO sc_ifpe;
GRANT USAGE ON SEQUENCE ifpe.spei_rastreo_seq TO sc_ifpe;

-- sc_spei (webhooks firmados, despachador, operador) registra lo que informa
-- el proveedor. La fecha de recepción la pone la base de datos.
GRANT INSERT (institucion_emisora, clave_rastreo, fecha_operacion, tipo_pago, institucion_receptora, receptor_clabe,
              cuenta_id, monto, nombre_ordenante, cuenta_ordenante, rfc_curp_ordenante, referencia_numerica,
              concepto, estado, causa_devolucion, motivo, transaccion_id, spei_outbound_id, origen,
              registrado_por, id_proveedor, raw_payload)
    ON ifpe.spei_inbound TO sc_spei;
GRANT UPDATE (estado, tx_reverso_id, fecha_operacion, id_proveedor, intentos_envio, ultimo_error, revision_manual,
              causa_devolucion, motivo, proximo_intento_en, enviada_en, liquidada_en, finalizada_en)
    ON ifpe.spei_outbound TO sc_spei;
GRANT INSERT (tipo, cuerpo, error) ON ifpe.spei_cuarentena TO sc_spei;
GRANT INSERT (id, spei_inbound_id, cuenta_id, monto, motivo, autorizado_por, clave_idempotencia, tx_id)
    ON ifpe.spei_correcciones TO sc_spei;
