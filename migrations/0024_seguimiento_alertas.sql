-- =============================================================================
--  Synthetic Capital — 0024: lista de seguimiento y alertas de precio
-- =============================================================================
--
--  1. `mercado.watchlist`: los activos que el cliente sigue, los tenga o no.
--     Es una preferencia, no un registro financiero: seguir agrega el renglón
--     y dejar de seguir lo borra. A lo más `mercado.maximo_seguidos()` por
--     cliente.
--  2. `mercado.alertas_precio`: avisar cuando el precio de un activo llegue a
--     un objetivo, al subir (ARRIBA: precio en o por encima del objetivo) o al
--     bajar (ABAJO: en o por debajo). Nace ACTIVA con el precio del catálogo
--     de ese momento como referencia (lo pone la base de datos) y un objetivo
--     del lado que aún no se alcanza; sus términos no cambian; pasa una sola
--     vez a DISPARADA (la base de datos anota el precio del catálogo que la
--     cumplió) o a CANCELADA, y ahí se queda. Nunca se borra. A lo más
--     `mercado.maximo_alertas()` activas por cliente.
--  3. Cada cambio de las dos sube `ifpe.cuentas.version_ordenes` (0015): la
--     bóveda avisa por su WebSocket, y la pantalla recarga y anuncia la
--     alerta disparada.
--  4. Escribe sc_mercado (el backend, y el motor que evalúa las alertas en su
--     pasada de cada 5 segundos); el cliente lee lo suyo (RLS). No exigen KYC
--     ni se niegan a una cuenta congelada: no mueven dinero, y negarlas
--     delataría el congelamiento.
--  5. Lo que llega al cliente nunca dice ALERTA (los códigos de error dicen
--     AVISO): esa palabra es de PLD, y el cliente no debe leerla (0009).
--
--  El historial de órdenes del cliente (`GET /api/v1/market/orders/history`)
--  lee lo que ya existe: `mercado.ordenes` y las órdenes pendientes cerradas
--  sin ejecutarse (0015). No cambia nada aquí.
-- =============================================================================

-- Cuántos activos sigue un cliente, y cuántas alertas activas tiene, a lo más:
-- el motor revisa las alertas en cada pasada.
CREATE FUNCTION mercado.maximo_seguidos() RETURNS INTEGER
LANGUAGE sql IMMUTABLE PARALLEL SAFE
AS $$ SELECT 50 $$;

CREATE FUNCTION mercado.maximo_alertas() RETURNS INTEGER
LANGUAGE sql IMMUTABLE PARALLEL SAFE
AS $$ SELECT 50 $$;

-- La bóveda del cliente avisa (la versión de sus órdenes sube): sus otras
-- pestañas y dispositivos recargan lo que cambió.
CREATE FUNCTION mercado.fn_avisar_boveda_cliente() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_cliente UUID := CASE WHEN TG_OP = 'DELETE' THEN OLD.cliente_id ELSE NEW.cliente_id END;
BEGIN
    UPDATE ifpe.cuentas SET version_ordenes = version_ordenes + 1 WHERE cliente_id = v_cliente AND moneda = 'MXN';
    RETURN NULL;
END
$$;

-- -----------------------------------------------------------------------------
-- 1. La lista de seguimiento
-- -----------------------------------------------------------------------------
CREATE TABLE mercado.watchlist (
    cliente_id UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    ticker     TEXT NOT NULL REFERENCES mercado.activos (ticker) ON DELETE RESTRICT,
    creado_en  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (cliente_id, ticker)
);
CREATE INDEX ix_mercado_watchlist_cliente ON mercado.watchlist (cliente_id, creado_en DESC);

COMMENT ON TABLE mercado.watchlist IS
    'Activos que sigue cada cliente (0024): una preferencia; seguir inserta, dejar de seguir borra.';

-- Al seguir: la fecha es de la base de datos y cabe en el máximo. Un renglón
-- no se edita: se sigue o se deja de seguir.
CREATE FUNCTION mercado.fn_validar_seguimiento() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    n INTEGER;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        RAISE EXCEPTION 'SC_SEGUIMIENTO_INMUTABLE: se sigue un activo o se deja de seguir; el renglón no se edita';
    END IF;
    NEW.creado_en := now();
    SELECT count(*) INTO n FROM mercado.watchlist WHERE cliente_id = NEW.cliente_id;
    IF n >= mercado.maximo_seguidos() THEN
        RAISE EXCEPTION 'SC_LIMITE_SEGUIMIENTO: el cliente ya sigue % activos', n USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_watchlist_validar BEFORE INSERT OR UPDATE ON mercado.watchlist
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_validar_seguimiento();
CREATE TRIGGER trg_watchlist_avisar AFTER INSERT OR DELETE ON mercado.watchlist
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_avisar_boveda_cliente();

-- -----------------------------------------------------------------------------
-- 2. Las alertas de precio
-- -----------------------------------------------------------------------------
CREATE TABLE mercado.alertas_precio (
    id                 UUID PRIMARY KEY,
    cliente_id         UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    ticker             TEXT NOT NULL REFERENCES mercado.activos (ticker) ON DELETE RESTRICT,
    -- El precio que la dispara.
    precio_objetivo    NUMERIC(18, 8) NOT NULL CHECK (precio_objetivo > 0),
    -- ARRIBA: con el precio en o por encima del objetivo; ABAJO: en o por debajo.
    direccion          TEXT NOT NULL CHECK (direccion IN ('ARRIBA', 'ABAJO')),
    estado             TEXT NOT NULL DEFAULT 'ACTIVA' CHECK (estado IN ('ACTIVA', 'DISPARADA', 'CANCELADA')),
    -- El precio del catálogo al crearla (lo pone la base de datos).
    precio_referencia  NUMERIC(18, 8) NOT NULL CHECK (precio_referencia > 0),
    -- El precio del catálogo que la disparó.
    precio_disparo     NUMERIC(18, 8) CHECK (precio_disparo > 0),
    disparada_en       TIMESTAMPTZ,
    cancelada_en       TIMESTAMPTZ,
    clave_idempotencia TEXT NOT NULL UNIQUE,
    creada_en          TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- Nace del lado que aún no se alcanza.
    CONSTRAINT ck_alerta_lado CHECK (
        (direccion = 'ARRIBA' AND precio_objetivo > precio_referencia)
     OR (direccion = 'ABAJO' AND precio_objetivo < precio_referencia)),
    -- Disparada: con su hora y un precio que cumple el objetivo.
    CONSTRAINT ck_alerta_disparo CHECK (
        (estado = 'DISPARADA') = (disparada_en IS NOT NULL)
        AND (estado = 'DISPARADA') = (precio_disparo IS NOT NULL)
        AND (precio_disparo IS NULL
             OR (direccion = 'ARRIBA' AND precio_disparo >= precio_objetivo)
             OR (direccion = 'ABAJO' AND precio_disparo <= precio_objetivo))),
    CONSTRAINT ck_alerta_cancelacion CHECK ((estado = 'CANCELADA') = (cancelada_en IS NOT NULL))
);
CREATE INDEX ix_mercado_alertas_cliente ON mercado.alertas_precio (cliente_id, creada_en DESC);
CREATE INDEX ix_mercado_alertas_activas ON mercado.alertas_precio (ticker) WHERE estado = 'ACTIVA';

COMMENT ON TABLE mercado.alertas_precio IS
    'Alertas de precio (0024): términos inmutables; de ACTIVA a DISPARADA (con el precio del catálogo) o CANCELADA.';

-- Al nacer: activa, con el precio del catálogo como referencia, un objetivo
-- que aún no se alcanza y dentro del máximo de activas.
CREATE FUNCTION mercado.fn_validar_alerta() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_precio NUMERIC;
    n        INTEGER;
BEGIN
    IF NEW.estado <> 'ACTIVA' OR NEW.precio_disparo IS NOT NULL OR NEW.disparada_en IS NOT NULL
       OR NEW.cancelada_en IS NOT NULL THEN
        RAISE EXCEPTION 'SC_AVISO_INCOHERENTE: una alerta de precio nace activa';
    END IF;
    SELECT precio INTO v_precio FROM mercado.activos WHERE ticker = NEW.ticker;
    IF v_precio IS NULL THEN
        RAISE EXCEPTION 'SC_AVISO_INCOHERENTE: el activo % no existe', NEW.ticker;
    END IF;
    NEW.precio_referencia := v_precio;
    NEW.creada_en := now();
    IF (NEW.direccion = 'ARRIBA' AND NEW.precio_objetivo <= v_precio)
       OR (NEW.direccion = 'ABAJO' AND NEW.precio_objetivo >= v_precio) THEN
        RAISE EXCEPTION 'SC_AVISO_YA_ALCANZADO: % ya está en % (objetivo %)', NEW.ticker, v_precio, NEW.precio_objetivo
            USING ERRCODE = 'check_violation';
    END IF;
    SELECT count(*) INTO n FROM mercado.alertas_precio WHERE cliente_id = NEW.cliente_id AND estado = 'ACTIVA';
    IF n >= mercado.maximo_alertas() THEN
        RAISE EXCEPTION 'SC_LIMITE_AVISOS_PRECIO: el cliente ya tiene % alertas activas', n
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END
$$;

-- Después: los términos no cambian; una activa se dispara o se cancela, una
-- sola vez. Al dispararse, el precio es el vigente del catálogo y debe cumplir
-- el objetivo: si el oráculo lo movió desde que el motor la eligió, la
-- operación se repite con el precio nuevo (SC_AVISO_NO_ALCANZADO, como
-- SC_MERCADO_PRECIO_CAMBIO en una orden). El precio y las fechas son de la
-- base de datos: lo que se escriba en ellos se ignora.
CREATE FUNCTION mercado.fn_transicion_alerta() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_precio NUMERIC;
BEGIN
    IF (NEW.id, NEW.cliente_id, NEW.ticker, NEW.precio_objetivo, NEW.direccion, NEW.precio_referencia,
        NEW.clave_idempotencia, NEW.creada_en)
       IS DISTINCT FROM
       (OLD.id, OLD.cliente_id, OLD.ticker, OLD.precio_objetivo, OLD.direccion, OLD.precio_referencia,
        OLD.clave_idempotencia, OLD.creada_en) THEN
        RAISE EXCEPTION 'SC_AVISO_INMUTABLE: los términos de la alerta % no cambian', OLD.id;
    END IF;
    IF OLD.estado <> 'ACTIVA' THEN
        RAISE EXCEPTION 'SC_AVISO_CERRADO: la alerta % ya está %', OLD.id, lower(OLD.estado);
    END IF;
    IF NEW.estado = 'DISPARADA' THEN
        SELECT precio INTO v_precio FROM mercado.activos WHERE ticker = OLD.ticker;
        IF (OLD.direccion = 'ARRIBA' AND v_precio < OLD.precio_objetivo)
           OR (OLD.direccion = 'ABAJO' AND v_precio > OLD.precio_objetivo) THEN
            RAISE EXCEPTION 'SC_AVISO_NO_ALCANZADO: % está en %: la alerta % (objetivo %) no se dispara',
                OLD.ticker, v_precio, OLD.id, OLD.precio_objetivo;
        END IF;
        NEW.precio_disparo := v_precio;
        NEW.disparada_en := now();
        NEW.cancelada_en := NULL;
    ELSIF NEW.estado = 'CANCELADA' THEN
        NEW.precio_disparo := NULL;
        NEW.cancelada_en := now();
        NEW.disparada_en := NULL;
    ELSE
        RAISE EXCEPTION 'SC_AVISO_TRANSICION: la alerta % sigue activa: se dispara o se cancela', OLD.id;
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER trg_alertas_a_validar BEFORE INSERT ON mercado.alertas_precio
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_validar_alerta();
CREATE TRIGGER trg_alertas_b_transicion BEFORE UPDATE ON mercado.alertas_precio
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_transicion_alerta();
CREATE TRIGGER trg_alertas_avisar AFTER INSERT OR UPDATE OF estado ON mercado.alertas_precio
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_avisar_boveda_cliente();
CREATE TRIGGER trg_alertas_sin_borrado BEFORE DELETE ON mercado.alertas_precio
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_alertas_no_truncate BEFORE TRUNCATE ON mercado.alertas_precio
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

COMMENT ON COLUMN ifpe.cuentas.version_ordenes IS
    '+1 por cada cambio en las órdenes pendientes (0015), los planes de ahorro (0023), la lista de seguimiento y las alertas de precio (0024) de la cuenta.';

-- -----------------------------------------------------------------------------
-- 3. Privilegios y RLS
-- -----------------------------------------------------------------------------
-- sc_mercado sigue y deja de seguir, da de alta una alerta (sus términos y la
-- clave; lo demás lo pone la base de datos), la dispara o la cancela (el
-- estado; el precio y las fechas los pone la base de datos).
GRANT SELECT, DELETE ON mercado.watchlist TO sc_mercado;
GRANT INSERT (cliente_id, ticker) ON mercado.watchlist TO sc_mercado;
GRANT SELECT ON mercado.alertas_precio TO sc_mercado;
GRANT INSERT (id, cliente_id, ticker, precio_objetivo, direccion, clave_idempotencia)
    ON mercado.alertas_precio TO sc_mercado;
GRANT UPDATE (estado) ON mercado.alertas_precio TO sc_mercado;
-- El cliente lee lo suyo; no escribe nada.
GRANT SELECT ON mercado.watchlist, mercado.alertas_precio TO sc_cliente;

ALTER TABLE mercado.watchlist ENABLE ROW LEVEL SECURITY;
CREATE POLICY por_privilegios ON mercado.watchlist USING (true) WITH CHECK (true);
CREATE POLICY solo_del_cliente ON mercado.watchlist AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual()) WITH CHECK (cliente_id = nucleo.cliente_actual());

ALTER TABLE mercado.alertas_precio ENABLE ROW LEVEL SECURITY;
CREATE POLICY por_privilegios ON mercado.alertas_precio USING (true) WITH CHECK (true);
CREATE POLICY solo_del_cliente ON mercado.alertas_precio AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual()) WITH CHECK (cliente_id = nucleo.cliente_actual());
