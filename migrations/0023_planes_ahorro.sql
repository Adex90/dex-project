-- =============================================================================
--  Synthetic Capital — 0023: planes de ahorro (compras periódicas, DCA)
-- =============================================================================
--
--  Un plan de ahorro compra el mismo monto de un activo cada semana, cada dos
--  semanas o cada mes, con el efectivo disponible de la bóveda. Cada compra es
--  una orden de mercado como cualquier otra (0013): el mismo ledger
--  encadenado, el mismo movimiento IFPE, la misma reserva del 100 % y la misma
--  evaluación PLD; solo que la coloca el programador del backend.
--
--  1. `mercado.planes_ahorro`: los términos (cliente, bóveda, activo, monto,
--     frecuencia) no cambian nunca; el estado va de ACTIVO a PAUSADO y de
--     regreso, y de cualquiera de los dos a CANCELADO, que es definitivo.
--  2. El calendario es de la base de datos. El día del alta (en la Ciudad de
--     México) es el ancla: la primera compra es inmediata y las siguientes son
--     a las 9:00 de la Ciudad de México, cada 7 o 14 días desde el ancla o el
--     mismo día de cada mes (el 31 cae en el último día de los meses más
--     cortos, sin correrse). El backend solo registra qué pasó con la compra
--     vencida (`ultimo_resultado`): la base de datos fija la siguiente, que es
--     siempre futura (una compra perdida mientras el servicio no corría no se
--     repone: el plan sigue en su fecha siguiente). Al reanudar, lo mismo.
--  3. Una compra EJECUTADA apunta a su orden del ledger del mercado, cuya
--     clave es `plan:<plan>:<periodo>` y cuyos términos (cliente, bóveda,
--     activo, compra, monto) son los del plan; una OMITIDA lleva el código de
--     la regla que la impidió (efectivo insuficiente, mercado cerrado...). A
--     la inversa, una orden con clave de plan solo existe con su registro.
--  4. `mercado.eventos_plan`: la bitácora inmutable del plan. Cada alta,
--     pausa, reanudación, cancelación y compra (hecha u omitida) deja su
--     renglón, escrito por un trigger: no hay cambio de plan sin su evento.
--  5. Cada cambio sube `ifpe.cuentas.version_ordenes` (0015): la bóveda del
--     cliente avisa por su WebSocket y sus otras pestañas recargan los planes.
--  6. Escribe sc_mercado (el backend) y lee el cliente lo suyo (RLS), como las
--     órdenes pendientes; el alta exige KYC operable (política de 0015).
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. El calendario
-- -----------------------------------------------------------------------------
-- El momento de la compra número `p_periodo` (desde 0) de un plan anclado en
-- `p_ancla`: ese día, a las 9:00 de la Ciudad de México.
CREATE FUNCTION mercado.momento_plan(p_frecuencia TEXT, p_ancla DATE, p_periodo INTEGER) RETURNS TIMESTAMPTZ
LANGUAGE sql STABLE PARALLEL SAFE SET search_path = pg_catalog, pg_temp
AS $$
    SELECT ((CASE p_frecuencia
                 WHEN 'SEMANAL' THEN p_ancla + 7 * p_periodo
                 WHEN 'QUINCENAL' THEN p_ancla + 14 * p_periodo
                 WHEN 'MENSUAL' THEN (p_ancla + make_interval(months => p_periodo))::date
             END) + time '09:00') AT TIME ZONE 'America/Mexico_City'
$$;

COMMENT ON FUNCTION mercado.momento_plan(TEXT, DATE, INTEGER) IS
    'Compra número p_periodo de un plan de ahorro (0023): el día que toca, a las 9:00 de la Ciudad de México.';

-- El primer periodo después de `p_desde` que aún no llega.
CREATE FUNCTION mercado.siguiente_periodo(p_frecuencia TEXT, p_ancla DATE, p_desde INTEGER) RETURNS INTEGER
LANGUAGE plpgsql STABLE SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    k INTEGER := p_desde + 1;
BEGIN
    WHILE mercado.momento_plan(p_frecuencia, p_ancla, k) <= now() LOOP
        k := k + 1;
        IF k > p_desde + 100000 THEN
            RAISE EXCEPTION 'SC_PLAN_CALENDARIO: sin fecha siguiente para el ancla % (%)', p_ancla, p_frecuencia;
        END IF;
    END LOOP;
    RETURN k;
END
$$;

-- Planes no cancelados por cliente: el programador los revisa en cada pasada.
CREATE FUNCTION mercado.maximo_planes() RETURNS INTEGER
LANGUAGE sql IMMUTABLE PARALLEL SAFE
AS $$ SELECT 20 $$;

-- -----------------------------------------------------------------------------
-- 2. Planes
-- -----------------------------------------------------------------------------
CREATE TABLE mercado.planes_ahorro (
    id                   UUID PRIMARY KEY,
    cliente_id           UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    -- Bóveda que paga las compras (sin FK por aislamiento, como en mercado.ordenes).
    cuenta_ifpe_id       UUID NOT NULL,
    ticker               TEXT NOT NULL REFERENCES mercado.activos (ticker) ON DELETE RESTRICT,
    -- Cada compra, en pesos y centavos.
    monto                NUMERIC(18, 8) NOT NULL CHECK (monto > 0 AND monto = trunc(monto, 2)),
    frecuencia           TEXT NOT NULL CHECK (frecuencia IN ('SEMANAL', 'QUINCENAL', 'MENSUAL')),
    estado               TEXT NOT NULL DEFAULT 'ACTIVO' CHECK (estado IN ('ACTIVO', 'PAUSADO', 'CANCELADO')),
    -- El día del alta en la Ciudad de México: de él se cuentan las compras.
    fecha_ancla          DATE NOT NULL,
    -- La compra que sigue (0 la del alta).
    periodo              INTEGER NOT NULL DEFAULT 0 CHECK (periodo >= 0),
    -- Cuándo; solo con el plan activo.
    proxima_ejecucion_en TIMESTAMPTZ,
    -- La última compra vencida que se procesó, y qué pasó con ella.
    ultima_ejecucion_en  TIMESTAMPTZ,
    ultimo_resultado     TEXT CHECK (ultimo_resultado IN ('EJECUTADA', 'OMITIDA')),
    motivo               TEXT CHECK (motivo ~ '^[A-Z][A-Z0-9_]{0,63}$'),
    ultima_orden_id      UUID REFERENCES mercado.ordenes (id) ON DELETE RESTRICT,
    clave_idempotencia   TEXT NOT NULL UNIQUE,
    creado_en            TIMESTAMPTZ NOT NULL DEFAULT now(),
    actualizado_en       TIMESTAMPTZ NOT NULL DEFAULT now(),
    cancelado_en         TIMESTAMPTZ,

    CONSTRAINT ck_plan_programacion CHECK ((estado = 'ACTIVO') = (proxima_ejecucion_en IS NOT NULL)),
    CONSTRAINT ck_plan_cancelacion CHECK ((estado = 'CANCELADO') = (cancelado_en IS NOT NULL)),
    CONSTRAINT ck_plan_resultado CHECK (
        (ultimo_resultado IS NULL AND ultima_ejecucion_en IS NULL AND motivo IS NULL AND ultima_orden_id IS NULL)
     OR (ultimo_resultado = 'EJECUTADA' AND ultima_ejecucion_en IS NOT NULL AND motivo IS NULL
         AND ultima_orden_id IS NOT NULL)
     OR (ultimo_resultado = 'OMITIDA' AND ultima_ejecucion_en IS NOT NULL AND motivo IS NOT NULL
         AND ultima_orden_id IS NULL))
);
CREATE INDEX ix_mercado_planes_cliente ON mercado.planes_ahorro (cliente_id, creado_en DESC);
CREATE INDEX ix_mercado_planes_vencidos ON mercado.planes_ahorro (proxima_ejecucion_en) WHERE estado = 'ACTIVO';

COMMENT ON TABLE mercado.planes_ahorro IS
    'Planes de ahorro (0023): compras periódicas de mercado; términos inmutables, calendario de la base de datos.';

-- -----------------------------------------------------------------------------
-- 3. La bitácora del plan
-- -----------------------------------------------------------------------------
CREATE TABLE mercado.eventos_plan (
    id                   BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    plan_id              UUID NOT NULL REFERENCES mercado.planes_ahorro (id) ON DELETE RESTRICT,
    cliente_id           UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    evento               TEXT NOT NULL CHECK (evento IN (
                             'CREADO', 'PAUSADO', 'REANUDADO', 'CANCELADO', 'EJECUTADO', 'OMITIDO')),
    -- Una compra: cuál (desde 0) y para cuándo estaba.
    periodo              INTEGER CHECK (periodo >= 0),
    programado_para      TIMESTAMPTZ,
    -- EJECUTADO: la orden del ledger del mercado. OMITIDO: el código de la regla.
    orden_id             UUID UNIQUE REFERENCES mercado.ordenes (id) ON DELETE RESTRICT,
    motivo               TEXT CHECK (motivo ~ '^[A-Z][A-Z0-9_]{0,63}$'),
    monto                NUMERIC(18, 8) NOT NULL CHECK (monto > 0),
    -- La compra que quedó programada después del evento (NULL si ninguna).
    proxima_ejecucion_en TIMESTAMPTZ,
    -- El usuario de la base de datos de la conexión: la aplicación o un DBA.
    registrado_por       TEXT NOT NULL DEFAULT session_user,
    ocurrido_en          TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),

    CONSTRAINT ck_evento_plan_forma CHECK (
        (evento = 'EJECUTADO') = (orden_id IS NOT NULL)
        AND (evento = 'OMITIDO') = (motivo IS NOT NULL)
        AND (evento IN ('EJECUTADO', 'OMITIDO')) = (periodo IS NOT NULL)
        AND (evento IN ('EJECUTADO', 'OMITIDO')) = (programado_para IS NOT NULL))
);
-- Una sola suerte por compra.
CREATE UNIQUE INDEX ux_mercado_eventos_plan_periodo ON mercado.eventos_plan (plan_id, periodo)
    WHERE evento IN ('EJECUTADO', 'OMITIDO');
CREATE INDEX ix_mercado_eventos_plan ON mercado.eventos_plan (plan_id, id);
CREATE TRIGGER trg_eventos_plan_inmutable BEFORE UPDATE OR DELETE ON mercado.eventos_plan
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_eventos_plan_no_truncate BEFORE TRUNCATE ON mercado.eventos_plan
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

COMMENT ON TABLE mercado.eventos_plan IS
    'Bitácora inmutable de los planes de ahorro (0023): altas, pausas, reanudaciones, cancelaciones y compras.';

-- -----------------------------------------------------------------------------
-- 4. Reglas del plan
-- -----------------------------------------------------------------------------
-- Al nacer: activo, de la bóveda del cliente, sobre un activo que opera, con un
-- monto que compra al menos un incremento al precio de hoy y dentro del
-- máximo de planes. El ancla, la primera compra (ya) y las fechas los pone la
-- base de datos.
CREATE FUNCTION mercado.fn_validar_plan() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    c ifpe.cuentas%ROWTYPE;
    a mercado.activos%ROWTYPE;
    n INTEGER;
BEGIN
    IF NEW.estado <> 'ACTIVO' OR NEW.periodo <> 0 OR NEW.ultimo_resultado IS NOT NULL OR NEW.motivo IS NOT NULL
       OR NEW.ultima_orden_id IS NOT NULL OR NEW.ultima_ejecucion_en IS NOT NULL OR NEW.cancelado_en IS NOT NULL THEN
        RAISE EXCEPTION 'SC_PLAN_INCOHERENTE: un plan de ahorro nace activo y sin compras';
    END IF;
    NEW.creado_en := now();
    NEW.actualizado_en := now();
    NEW.fecha_ancla := (now() AT TIME ZONE 'America/Mexico_City')::date;
    NEW.proxima_ejecucion_en := now();

    SELECT * INTO c FROM ifpe.cuentas WHERE id = NEW.cuenta_ifpe_id;
    IF c.id IS NULL OR c.cliente_id <> NEW.cliente_id OR c.moneda <> 'MXN' THEN
        RAISE EXCEPTION 'SC_PLAN_INCOHERENTE: la cuenta % no es la bóveda MXN del cliente %',
            NEW.cuenta_ifpe_id, NEW.cliente_id;
    END IF;
    -- Congelamiento preventivo: el error constante, sin aviso (como 0013 y 0015).
    IF c.estado = 'CONGELADO_PREVENTIVO' OR aml.cliente_congelado(NEW.cliente_id) THEN
        RAISE EXCEPTION 'SC_OPERACION_NO_DISPONIBLE: la operación no está disponible en este momento'
            USING ERRCODE = 'check_violation';
    ELSIF c.estado = 'CONGELADA' THEN
        RAISE EXCEPTION 'SC_CUENTA_CONGELADA: la cuenta IFPE está congelada' USING ERRCODE = 'check_violation';
    ELSIF c.estado <> 'ACTIVA' THEN
        RAISE EXCEPTION 'SC_CUENTA_CERRADA: la cuenta IFPE está cerrada' USING ERRCODE = 'check_violation';
    END IF;

    SELECT * INTO a FROM mercado.activos WHERE ticker = NEW.ticker;
    IF a.ticker IS NULL THEN
        RAISE EXCEPTION 'SC_PLAN_INCOHERENTE: el activo % no existe', NEW.ticker;
    END IF;
    IF NOT a.operable THEN
        RAISE EXCEPTION 'SC_MERCADO_CERRADO: % no opera en este momento', NEW.ticker USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.monto < a.precio * power(10::numeric, -a.decimales_cantidad) THEN
        RAISE EXCEPTION 'SC_MONTO_INSUFICIENTE: % MXN no compran el incremento mínimo de %', NEW.monto, NEW.ticker
            USING ERRCODE = 'check_violation';
    END IF;

    SELECT count(*) INTO n FROM mercado.planes_ahorro WHERE cliente_id = NEW.cliente_id AND estado <> 'CANCELADO';
    IF n >= mercado.maximo_planes() THEN
        RAISE EXCEPTION 'SC_LIMITE_PLANES: el cliente ya tiene % planes', n USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END
$$;

-- Después: los términos no cambian, un cancelado no cambia nada, y el estado
-- solo pausa, reanuda o cancela. Las fechas son de la base de datos: lo que se
-- escriba en ellas se ignora; al pausar no hay compra programada, al reanudar
-- la siguiente es la primera que aún no llega y al cancelar ninguna.
CREATE FUNCTION mercado.fn_transicion_plan() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF OLD.estado = 'CANCELADO' THEN
        RAISE EXCEPTION 'SC_PLAN_CANCELADO: el plan % ya está cancelado', OLD.id USING ERRCODE = 'check_violation';
    END IF;
    IF (NEW.id, NEW.cliente_id, NEW.cuenta_ifpe_id, NEW.ticker, NEW.monto, NEW.frecuencia, NEW.fecha_ancla,
        NEW.clave_idempotencia, NEW.creado_en)
       IS DISTINCT FROM
       (OLD.id, OLD.cliente_id, OLD.cuenta_ifpe_id, OLD.ticker, OLD.monto, OLD.frecuencia, OLD.fecha_ancla,
        OLD.clave_idempotencia, OLD.creado_en) THEN
        RAISE EXCEPTION 'SC_PLAN_INMUTABLE: los términos del plan % no cambian', OLD.id;
    END IF;
    NEW.periodo := OLD.periodo;
    NEW.proxima_ejecucion_en := OLD.proxima_ejecucion_en;
    NEW.ultima_ejecucion_en := OLD.ultima_ejecucion_en;
    NEW.cancelado_en := OLD.cancelado_en;
    IF NEW.estado IS DISTINCT FROM OLD.estado THEN
        IF OLD.estado = 'ACTIVO' AND NEW.estado = 'PAUSADO' THEN
            NEW.proxima_ejecucion_en := NULL;
        ELSIF OLD.estado = 'PAUSADO' AND NEW.estado = 'ACTIVO' THEN
            NEW.periodo := mercado.siguiente_periodo(OLD.frecuencia, OLD.fecha_ancla, OLD.periodo - 1);
            NEW.proxima_ejecucion_en := mercado.momento_plan(OLD.frecuencia, OLD.fecha_ancla, NEW.periodo);
        ELSIF NEW.estado = 'CANCELADO' THEN
            NEW.proxima_ejecucion_en := NULL;
            NEW.cancelado_en := now();
        ELSE
            RAISE EXCEPTION 'SC_PLAN_TRANSICION: el plan % no pasa de % a %', OLD.id, OLD.estado, NEW.estado;
        END IF;
    END IF;
    NEW.actualizado_en := now();
    RETURN NEW;
END
$$;

-- Registrar qué pasó con la compra vencida (cualquier escritura de esas
-- columnas): solo con el plan activo y la compra ya vencida; una EJECUTADA, con
-- la orden de su periodo y de los términos del plan; una OMITIDA, con su
-- motivo. La base de datos fija la siguiente compra: la primera que aún no llega.
CREATE FUNCTION mercado.fn_ejecucion_plan() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    o mercado.ordenes%ROWTYPE;
BEGIN
    IF OLD.estado <> 'ACTIVO' OR NEW.estado <> 'ACTIVO' THEN
        RAISE EXCEPTION 'SC_PLAN_NO_ACTIVO: el plan % no está activo', OLD.id;
    END IF;
    IF OLD.proxima_ejecucion_en > now() THEN
        RAISE EXCEPTION 'SC_PLAN_NO_VENCIDO: la compra del plan % es hasta %', OLD.id, OLD.proxima_ejecucion_en;
    END IF;
    IF NEW.ultimo_resultado = 'EJECUTADA' THEN
        IF NEW.motivo IS NOT NULL OR NEW.ultima_orden_id IS NULL THEN
            RAISE EXCEPTION 'SC_PLAN_INCOHERENTE: una compra hecha lleva su orden y ningún motivo';
        END IF;
        SELECT * INTO o FROM mercado.ordenes WHERE id = NEW.ultima_orden_id;
        IF o.id IS NULL OR o.clave_idempotencia <> 'plan:' || OLD.id || ':' || OLD.periodo
           OR o.cliente_id <> OLD.cliente_id OR o.cuenta_ifpe_id <> OLD.cuenta_ifpe_id OR o.ticker <> OLD.ticker
           OR o.lado <> 'COMPRA' OR o.monto_solicitado <> OLD.monto THEN
            RAISE EXCEPTION 'SC_PLAN_INCOHERENTE: la orden % no es la compra % del plan %',
                NEW.ultima_orden_id, OLD.periodo, OLD.id;
        END IF;
    ELSIF NEW.ultimo_resultado = 'OMITIDA' THEN
        IF NEW.motivo IS NULL OR NEW.ultima_orden_id IS NOT NULL THEN
            RAISE EXCEPTION 'SC_PLAN_INCOHERENTE: una compra omitida lleva su motivo y ninguna orden';
        END IF;
    ELSE
        RAISE EXCEPTION 'SC_PLAN_INCOHERENTE: una compra se registra hecha u omitida';
    END IF;
    NEW.ultima_ejecucion_en := now();
    NEW.periodo := mercado.siguiente_periodo(OLD.frecuencia, OLD.fecha_ancla, OLD.periodo);
    NEW.proxima_ejecucion_en := mercado.momento_plan(OLD.frecuencia, OLD.fecha_ancla, NEW.periodo);
    RETURN NEW;
END
$$;

-- Los triggers BEFORE corren en orden alfabético: primero la transición
-- (que restaura las fechas), después la compra (que las avanza).
CREATE TRIGGER trg_planes_a_validar BEFORE INSERT ON mercado.planes_ahorro
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_validar_plan();
CREATE TRIGGER trg_planes_b_transicion BEFORE UPDATE ON mercado.planes_ahorro
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_transicion_plan();
CREATE TRIGGER trg_planes_c_ejecucion BEFORE UPDATE OF ultimo_resultado, motivo, ultima_orden_id
    ON mercado.planes_ahorro FOR EACH ROW EXECUTE FUNCTION mercado.fn_ejecucion_plan();
CREATE TRIGGER trg_planes_sin_borrado BEFORE DELETE ON mercado.planes_ahorro
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_planes_no_truncate BEFORE TRUNCATE ON mercado.planes_ahorro
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- -----------------------------------------------------------------------------
-- 5. Cada cambio, a la bitácora (y la bóveda avisa)
-- -----------------------------------------------------------------------------
CREATE FUNCTION mercado.fn_auditar_plan() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        INSERT INTO mercado.eventos_plan (plan_id, cliente_id, evento, monto, proxima_ejecucion_en)
        VALUES (NEW.id, NEW.cliente_id, 'CREADO', NEW.monto, NEW.proxima_ejecucion_en);
    ELSIF TG_ARGV[0] = 'estado' THEN
        INSERT INTO mercado.eventos_plan (plan_id, cliente_id, evento, monto, proxima_ejecucion_en)
        VALUES (NEW.id, NEW.cliente_id,
                CASE NEW.estado WHEN 'PAUSADO' THEN 'PAUSADO' WHEN 'ACTIVO' THEN 'REANUDADO' ELSE 'CANCELADO' END,
                NEW.monto, NEW.proxima_ejecucion_en);
    ELSE
        INSERT INTO mercado.eventos_plan (plan_id, cliente_id, evento, periodo, programado_para, orden_id, motivo,
                                          monto, proxima_ejecucion_en)
        VALUES (NEW.id, NEW.cliente_id, CASE NEW.ultimo_resultado WHEN 'EJECUTADA' THEN 'EJECUTADO' ELSE 'OMITIDO' END,
                OLD.periodo, OLD.proxima_ejecucion_en, NEW.ultima_orden_id, NEW.motivo, NEW.monto,
                NEW.proxima_ejecucion_en);
    END IF;
    UPDATE ifpe.cuentas SET version_ordenes = version_ordenes + 1 WHERE id = NEW.cuenta_ifpe_id;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_planes_auditar_alta AFTER INSERT ON mercado.planes_ahorro
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_auditar_plan();
CREATE TRIGGER trg_planes_auditar_estado AFTER UPDATE ON mercado.planes_ahorro
    FOR EACH ROW WHEN (NEW.estado IS DISTINCT FROM OLD.estado) EXECUTE FUNCTION mercado.fn_auditar_plan('estado');
CREATE TRIGGER trg_planes_auditar_compra AFTER UPDATE OF ultimo_resultado, motivo, ultima_orden_id
    ON mercado.planes_ahorro FOR EACH ROW EXECUTE FUNCTION mercado.fn_auditar_plan('compra');

-- Una orden con clave de plan solo existe con el registro de su compra (al COMMIT).
CREATE FUNCTION mercado.fn_orden_de_plan() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM mercado.eventos_plan WHERE orden_id = NEW.id AND evento = 'EJECUTADO') THEN
        RAISE EXCEPTION 'SC_PLAN_SIN_EJECUCION: la orden % (%) no es una compra registrada de su plan',
            NEW.id, NEW.clave_idempotencia;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_ordenes_de_plan AFTER INSERT ON mercado.ordenes
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW WHEN (NEW.clave_idempotencia LIKE 'plan:%')
    EXECUTE FUNCTION mercado.fn_orden_de_plan();

COMMENT ON COLUMN ifpe.cuentas.version_ordenes IS
    '+1 por cada cambio en las órdenes pendientes (0015) y los planes de ahorro (0023) de la cuenta.';

-- -----------------------------------------------------------------------------
-- 6. Privilegios y RLS
-- -----------------------------------------------------------------------------
-- sc_mercado da de alta (los términos y la clave; lo demás lo pone la base de
-- datos), cambia el estado y registra cada compra vencida.
GRANT SELECT ON mercado.planes_ahorro, mercado.eventos_plan TO sc_mercado;
GRANT INSERT (id, cliente_id, cuenta_ifpe_id, ticker, monto, frecuencia, clave_idempotencia)
    ON mercado.planes_ahorro TO sc_mercado;
GRANT UPDATE (estado, ultimo_resultado, motivo, ultima_orden_id) ON mercado.planes_ahorro TO sc_mercado;
-- El cliente lee sus planes y su bitácora; no escribe nada.
GRANT SELECT ON mercado.planes_ahorro, mercado.eventos_plan TO sc_cliente;

-- Como las órdenes pendientes (0015): da de alta quien tiene KYC verificado y
-- está fuera de la Lista de Personas Bloqueadas; pausar, reanudar y cancelar,
-- cualquiera.
ALTER TABLE mercado.planes_ahorro ENABLE ROW LEVEL SECURITY;
CREATE POLICY planes_lectura ON mercado.planes_ahorro FOR SELECT USING (true);
CREATE POLICY planes_alta_con_kyc ON mercado.planes_ahorro FOR INSERT WITH CHECK (kyc.puede_operar(cliente_id));
CREATE POLICY planes_cambio ON mercado.planes_ahorro FOR UPDATE USING (true) WITH CHECK (true);
CREATE POLICY solo_del_cliente ON mercado.planes_ahorro AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual()) WITH CHECK (cliente_id = nucleo.cliente_actual());

ALTER TABLE mercado.eventos_plan ENABLE ROW LEVEL SECURITY;
CREATE POLICY por_privilegios ON mercado.eventos_plan USING (true) WITH CHECK (true);
CREATE POLICY solo_del_cliente ON mercado.eventos_plan AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual()) WITH CHECK (cliente_id = nucleo.cliente_actual());
