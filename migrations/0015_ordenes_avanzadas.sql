-- =============================================================================
--  Synthetic Capital — 0015: órdenes límite y stop (efectivo y títulos comprometidos)
-- =============================================================================
--
--  Una orden límite o stop espera a su precio. Mientras espera, lo que usaría
--  queda comprometido para que nada lo gaste dos veces:
--
--  1. Efectivo comprometido (compras). Un tercer saldo de la cuenta IFPE,
--     `saldo_comprometido`, junto al disponible y al bloqueado (PLD). Lo mueven
--     dos movimientos nuevos del ledger IFPE, encadenados como todos:
--     COMPROMISO_ORDEN (disponible -> comprometido) y LIBERACION_ORDEN
--     (comprometido -> disponible), siempre dentro de la misma cuenta y sin
--     tocar la reserva. El dinero comprometido sigue siendo del cliente y
--     sigue en custodia: el pasivo, las hojas del árbol Merkle y la reserva del
--     100 % lo cuentan, y el saldo sombra (0004) lo reproduce desde el ledger.
--     `saldo_disponible` conserva su significado (lo que se puede gastar): los
--     retiros, transferencias, inversiones, SPEI y retenciones PLD no cambian.
--  2. Títulos comprometidos (ventas): `mercado.posiciones.cantidad_comprometida`.
--     Una venta de mercado solo vende los títulos libres.
--  3. `mercado.ordenes_pendientes`: el libro de órdenes en espera. Sus términos
--     son inmutables y su estado solo avanza (PENDIENTE -> EJECUTADA,
--     CANCELADA o RECHAZADA). Al COMMIT, el efectivo comprometido de cada
--     cuenta es la suma de sus compras pendientes, y los títulos comprometidos
--     de cada posición, la de sus ventas pendientes; cada compromiso y cada
--     liberación del ledger IFPE corresponden a la orden que los causó.
--  4. La ejecución de una orden pendiente es una orden del ledger del mercado
--     (0013) que la base de datos revisa contra los términos de la pendiente:
--     una orden límite nunca se ejecuta a un precio peor que su límite, una
--     stop solo después de alcanzar su precio, y una venta vende exactamente
--     los títulos que comprometió.
--  5. La versión de la bóveda (`seq`, 0012) sube también cuando cambia el
--     efectivo comprometido o una orden pendiente de la cuenta, así que el
--     WebSocket avisa al colocar, cancelar o ejecutar.
--
--  Orden de candados: el de 0013 (cuenta IFPE -> candado PLD -> reserva ->
--  ... -> activo -> posición); la orden pendiente se bloquea después de la
--  cuenta IFPE de su cliente.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. Efectivo comprometido en la cuenta IFPE y su sombra
-- -----------------------------------------------------------------------------
ALTER TABLE ifpe.cuentas
    ADD COLUMN saldo_comprometido NUMERIC(18, 8) NOT NULL DEFAULT 0
        CONSTRAINT ck_ifpe_cuentas_saldo_comprometido CHECK (saldo_comprometido >= 0),
    -- +1 por cada cambio en las órdenes pendientes de la cuenta (trigger de
    -- mercado.ordenes_pendientes): una venta pendiente no mueve efectivo,
    -- pero la bóveda que ve el cliente sí cambia.
    ADD COLUMN version_ordenes BIGINT NOT NULL DEFAULT 0
        CONSTRAINT ck_ifpe_cuentas_version_ordenes CHECK (version_ordenes >= 0);

COMMENT ON COLUMN ifpe.cuentas.saldo_comprometido IS
    'Efectivo comprometido en órdenes pendientes (0015): del cliente y en custodia, pero no disponible.';

-- Una cuenta cerrada no guarda dinero, tampoco comprometido.
ALTER TABLE ifpe.cuentas DROP CONSTRAINT ck_ifpe_cuentas_cerrada_sin_saldo;
ALTER TABLE ifpe.cuentas ADD CONSTRAINT ck_ifpe_cuentas_cerrada_sin_saldo
    CHECK (estado <> 'CERRADA' OR (saldo_disponible = 0 AND saldo_bloqueado = 0 AND saldo_comprometido = 0));

ALTER TABLE ifpe.saldos_ledger ADD COLUMN comprometido NUMERIC(20, 8) NOT NULL DEFAULT 0;

-- Mueve `p_monto` del disponible al comprometido de la sombra (negativo: de
-- regreso). Uso interno de la función de sombras: sin EXECUTE para PUBLIC.
CREATE FUNCTION ifpe.mover_sombra_comprometida(p_cuenta UUID, p_monto NUMERIC) RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
    IF p_cuenta IS NULL THEN
        RAISE EXCEPTION 'SC_SALDO_SIN_LEDGER: compromiso sin cuenta';
    END IF;
    INSERT INTO ifpe.saldos_ledger AS s (cuenta_id, disponible, bloqueado, comprometido)
    VALUES (p_cuenta, -p_monto, 0, p_monto)
    ON CONFLICT (cuenta_id) DO UPDATE SET disponible = s.disponible + EXCLUDED.disponible,
                                          comprometido = s.comprometido + EXCLUDED.comprometido;
END
$$;
REVOKE EXECUTE ON FUNCTION ifpe.mover_sombra_comprometida(UUID, NUMERIC) FROM PUBLIC;

CREATE OR REPLACE FUNCTION ifpe.fn_aplicar_tx_a_sombras() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    CASE NEW.tipo
        WHEN 'DEPOSITO', 'RETORNO_DE_SOFOM', 'VENTA_MERCADO' THEN
            PERFORM nucleo.mover_sombra('ifpe.cuentas', NEW.cuenta_destino_id, NEW.monto, 0);
            PERFORM nucleo.mover_sombra('ifpe.reserva', NEW.reserva_id, NEW.monto);
        WHEN 'RETIRO', 'TRASPASO_A_SOFOM', 'COMPRA_MERCADO' THEN
            PERFORM nucleo.mover_sombra('ifpe.cuentas', NEW.cuenta_origen_id, -NEW.monto, 0);
            PERFORM nucleo.mover_sombra('ifpe.reserva', NEW.reserva_id, -NEW.monto);
        WHEN 'TRANSFERENCIA' THEN
            PERFORM nucleo.mover_sombra('ifpe.cuentas', NEW.cuenta_origen_id, -NEW.monto, 0);
            PERFORM nucleo.mover_sombra('ifpe.cuentas', NEW.cuenta_destino_id, NEW.monto, 0);
        WHEN 'RETENCION' THEN
            PERFORM nucleo.mover_sombra('ifpe.cuentas', NEW.cuenta_origen_id, -NEW.monto, NEW.monto);
        WHEN 'LIBERACION_RETENCION' THEN
            PERFORM nucleo.mover_sombra('ifpe.cuentas', NEW.cuenta_origen_id, NEW.monto, -NEW.monto);
        WHEN 'COMPROMISO_ORDEN' THEN
            PERFORM ifpe.mover_sombra_comprometida(NEW.cuenta_origen_id, NEW.monto);
        WHEN 'LIBERACION_ORDEN' THEN
            PERFORM ifpe.mover_sombra_comprometida(NEW.cuenta_origen_id, -NEW.monto);
    END CASE;
    RETURN NULL;
END
$$;

-- Al COMMIT, el efectivo comprometido de cada cuenta es el que su ledger
-- justifica (el disponible y el bloqueado los revisa 0004).
CREATE FUNCTION ifpe.fn_verificar_comprometido_vs_ledger() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    clave  UUID;
    vivo   NUMERIC;
    sombra NUMERIC;
BEGIN
    IF TG_TABLE_NAME = 'cuentas' THEN clave := NEW.id; ELSE clave := NEW.cuenta_id; END IF;
    SELECT saldo_comprometido INTO vivo FROM ifpe.cuentas WHERE id = clave;
    SELECT comprometido INTO sombra FROM ifpe.saldos_ledger WHERE cuenta_id = clave;
    IF coalesce(vivo, 0) <> coalesce(sombra, 0) THEN
        RAISE EXCEPTION 'SC_SALDO_SIN_LEDGER: la cuenta % tiene % comprometido pero el ledger justifica %',
            clave, coalesce(vivo, 0), coalesce(sombra, 0)
            USING ERRCODE = 'check_violation',
                  HINT = 'Todo compromiso se registra con COMPROMISO_ORDEN o LIBERACION_ORDEN, en la misma transacción.';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_ifpe_cuentas_comprometido_saldo_ledger
    AFTER INSERT OR UPDATE OF saldo_comprometido ON ifpe.cuentas
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION ifpe.fn_verificar_comprometido_vs_ledger();
CREATE CONSTRAINT TRIGGER trg_ifpe_sombra_comprometido_saldo_ledger
    AFTER INSERT OR UPDATE OF comprometido ON ifpe.saldos_ledger
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION ifpe.fn_verificar_comprometido_vs_ledger();

-- El pasivo con clientes cuenta el efectivo comprometido: sigue siendo suyo.
-- Comprometer o liberar mueve dinero dentro de la cuenta y no cambia el total.
CREATE OR REPLACE FUNCTION ifpe.fn_acumular_pasivo() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    delta NUMERIC(20, 8);
BEGIN
    delta := CASE TG_OP
        WHEN 'INSERT' THEN NEW.saldo_disponible + NEW.saldo_bloqueado + NEW.saldo_comprometido
        WHEN 'UPDATE' THEN (NEW.saldo_disponible + NEW.saldo_bloqueado + NEW.saldo_comprometido)
                         - (OLD.saldo_disponible + OLD.saldo_bloqueado + OLD.saldo_comprometido)
        WHEN 'DELETE' THEN -(OLD.saldo_disponible + OLD.saldo_bloqueado + OLD.saldo_comprometido)
    END;
    IF delta <> 0 THEN
        UPDATE ifpe.estado_reserva
           SET pasivo_clientes = pasivo_clientes + delta, actualizado_en = now()
         WHERE id;
    END IF;
    RETURN NULL;
END
$$;
DROP TRIGGER trg_ifpe_cuentas_pasivo ON ifpe.cuentas;
CREATE TRIGGER trg_ifpe_cuentas_pasivo
    AFTER INSERT OR UPDATE OF saldo_disponible, saldo_bloqueado, saldo_comprometido OR DELETE ON ifpe.cuentas
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_acumular_pasivo();

-- Hojas del árbol Merkle: el saldo total de cada cuenta, comprometido incluido.
CREATE OR REPLACE VIEW ifpe.v_hojas_reserva AS
SELECT c.id AS cuenta_id,
       c.cliente_id,
       (c.saldo_disponible + c.saldo_bloqueado + c.saldo_comprometido)::numeric(18, 8) AS saldo_total
  FROM ifpe.cuentas c
 WHERE c.estado <> 'CERRADA';

-- La versión de la bóveda (0012) sube con cualquiera de los tres saldos y con
-- las órdenes pendientes de la cuenta.
CREATE OR REPLACE FUNCTION ifpe.fn_cuenta_seq() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_xact xid8;
BEGIN
    -- Toda cuenta nace en la versión 0, la escriba quien la escriba.
    IF TG_OP = 'INSERT' THEN
        NEW.seq := 0;
        NEW.seq_xact := NULL;
        RETURN NEW;
    END IF;
    IF (NEW.saldo_disponible, NEW.saldo_bloqueado, NEW.saldo_comprometido, NEW.version_ordenes)
       IS DISTINCT FROM (OLD.saldo_disponible, OLD.saldo_bloqueado, OLD.saldo_comprometido, OLD.version_ordenes) THEN
        v_xact := pg_current_xact_id();
        -- Una sola vez por transacción: varias sentencias de la misma
        -- transacción producen una única versión, la que se ve al confirmar.
        IF OLD.seq_xact IS DISTINCT FROM v_xact THEN
            NEW.seq := OLD.seq + 1;
            NEW.seq_xact := v_xact;
            RETURN NEW;
        END IF;
    END IF;
    -- Sin cambio (o ya contado): seq no se toca, ni aunque lo pida un UPDATE.
    NEW.seq := OLD.seq;
    NEW.seq_xact := OLD.seq_xact;
    RETURN NEW;
END
$$;

-- -----------------------------------------------------------------------------
-- 2. Movimientos COMPROMISO_ORDEN y LIBERACION_ORDEN del ledger IFPE
-- -----------------------------------------------------------------------------
-- NOT VALID, como en 0013: las restricciones nuevas admiten todo lo anterior.
ALTER TABLE ifpe.transacciones DROP CONSTRAINT transacciones_tipo_check;
ALTER TABLE ifpe.transacciones ADD CONSTRAINT transacciones_tipo_check CHECK (tipo IN (
    'DEPOSITO', 'RETIRO', 'TRANSFERENCIA', 'TRASPASO_A_SOFOM', 'RETORNO_DE_SOFOM', 'RETENCION', 'LIBERACION_RETENCION',
    'COMPRA_MERCADO', 'VENTA_MERCADO',
    'COMPROMISO_ORDEN',  -- disponible -> comprometido (misma cuenta): una compra pendiente
    'LIBERACION_ORDEN'   -- comprometido -> disponible (misma cuenta): la compra se cerró
)) NOT VALID;

ALTER TABLE ifpe.transacciones DROP CONSTRAINT ck_ifpe_tx_forma;
ALTER TABLE ifpe.transacciones ADD CONSTRAINT ck_ifpe_tx_forma CHECK (
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
                               AND reserva_id IS NULL)                                                                        OR
    (tipo = 'COMPRA_MERCADO'   AND cuenta_origen_id IS NOT NULL AND cuenta_destino_id IS NULL     AND reserva_id IS NOT NULL
                               AND ref_movimiento_espejo IS NOT NULL)                                                         OR
    (tipo = 'VENTA_MERCADO'    AND cuenta_origen_id IS NULL     AND cuenta_destino_id IS NOT NULL AND reserva_id IS NOT NULL
                               AND ref_movimiento_espejo IS NOT NULL)                                                         OR
    -- Su orden pendiente va en ref_movimiento_espejo.
    (tipo IN ('COMPROMISO_ORDEN', 'LIBERACION_ORDEN')
                               AND cuenta_origen_id IS NOT NULL AND cuenta_destino_id = cuenta_origen_id
                               AND reserva_id IS NULL           AND ref_movimiento_espejo IS NOT NULL)
) NOT VALID;

-- Comprometer efectivo es operar (KYC verificado); liberarlo siempre se puede.
CREATE OR REPLACE FUNCTION ifpe.kyc_permite_movimiento(p_tipo TEXT, p_origen UUID, p_destino UUID, p_clave TEXT)
RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT CASE p_tipo
        -- Entra dinero a la cuenta (SPEI recibido o depósito de demostración).
        WHEN 'DEPOSITO'         THEN starts_with(p_clave, 'spei-reverso:') OR ifpe.cuenta_puede_operar(p_destino)
        WHEN 'TRANSFERENCIA'    THEN ifpe.cuenta_puede_operar(p_origen) AND ifpe.cuenta_puede_operar(p_destino)
        WHEN 'RETIRO'           THEN starts_with(p_clave, 'spei-correccion:') OR ifpe.cuenta_puede_operar(p_origen)
        -- Inversión en mutuo.
        WHEN 'TRASPASO_A_SOFOM' THEN ifpe.cuenta_puede_operar(p_origen)
        -- Compra y venta de valores (0013): solo quien puede operar.
        WHEN 'COMPRA_MERCADO'   THEN ifpe.cuenta_puede_operar(p_origen)
        WHEN 'VENTA_MERCADO'    THEN ifpe.cuenta_puede_operar(p_destino)
        -- Una compra pendiente (0015).
        WHEN 'COMPROMISO_ORDEN' THEN ifpe.cuenta_puede_operar(p_origen)
        -- RETORNO_DE_SOFOM, RETENCION, LIBERACION_RETENCION y LIBERACION_ORDEN.
        ELSE TRUE
    END
$$;

-- Solo sc_mercado escribe los movimientos del mercado, y sc_mercado solo esos.
DROP POLICY tx_mercado_exclusivo ON ifpe.transacciones;
DROP POLICY tx_mercado_solo_sus_movimientos ON ifpe.transacciones;
CREATE POLICY tx_mercado_exclusivo ON ifpe.transacciones AS RESTRICTIVE FOR INSERT
    WITH CHECK (tipo NOT IN ('COMPRA_MERCADO', 'VENTA_MERCADO', 'COMPROMISO_ORDEN', 'LIBERACION_ORDEN')
                OR pg_has_role(current_user, 'sc_mercado', 'MEMBER'));
CREATE POLICY tx_mercado_solo_sus_movimientos ON ifpe.transacciones AS RESTRICTIVE FOR INSERT TO sc_mercado
    WITH CHECK (tipo IN ('COMPRA_MERCADO', 'VENTA_MERCADO', 'COMPROMISO_ORDEN', 'LIBERACION_ORDEN'));

GRANT UPDATE (saldo_comprometido) ON ifpe.cuentas TO sc_mercado;

-- -----------------------------------------------------------------------------
-- 3. Títulos comprometidos en las posiciones
-- -----------------------------------------------------------------------------
ALTER TABLE mercado.posiciones
    ADD COLUMN cantidad_comprometida NUMERIC(18, 8) NOT NULL DEFAULT 0,
    ADD CONSTRAINT ck_mercado_posicion_comprometida
        CHECK (cantidad_comprometida >= 0 AND cantidad_comprometida <= cantidad);

COMMENT ON COLUMN mercado.posiciones.cantidad_comprometida IS
    'Títulos comprometidos en ventas pendientes (0015): siguen siendo del cliente, pero no se venden en el mercado.';

-- -----------------------------------------------------------------------------
-- 4. Libro de órdenes pendientes
-- -----------------------------------------------------------------------------
CREATE TABLE mercado.ordenes_pendientes (
    id                    UUID PRIMARY KEY,
    cliente_id            UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    -- Bóveda que respalda la orden (sin FK por aislamiento, como en mercado.ordenes).
    cuenta_ifpe_id        UUID NOT NULL,
    ticker                TEXT NOT NULL REFERENCES mercado.activos (ticker) ON DELETE RESTRICT,
    lado                  TEXT NOT NULL CHECK (lado IN ('COMPRA', 'VENTA')),
    tipo                  TEXT NOT NULL CHECK (tipo IN ('LIMITE', 'STOP')),
    -- Lo que pidió el cliente, en pesos y centavos.
    monto                 NUMERIC(18, 8) NOT NULL CHECK (monto > 0 AND monto = trunc(monto, 2)),
    precio_limite         NUMERIC(18, 8) CHECK (precio_limite > 0),
    precio_stop           NUMERIC(18, 8) CHECK (precio_stop > 0),
    -- Compra: el monto completo, comprometido en la cuenta IFPE.
    efectivo_comprometido NUMERIC(18, 8) NOT NULL DEFAULT 0 CHECK (efectivo_comprometido >= 0),
    -- Venta: los títulos que el monto vende al precio de la orden, comprometidos en la posición.
    titulos_comprometidos NUMERIC(18, 8) NOT NULL DEFAULT 0 CHECK (titulos_comprometidos >= 0),
    estado                TEXT NOT NULL DEFAULT 'PENDIENTE'
                              CHECK (estado IN ('PENDIENTE', 'EJECUTADA', 'CANCELADA', 'RECHAZADA')),
    -- Por qué se rechazó al dispararse (el código de error de la ejecución).
    motivo                TEXT CHECK (motivo ~ '^[A-Z][A-Z0-9_]{0,63}$'),
    -- Movimientos COMPROMISO_ORDEN y LIBERACION_ORDEN (compras; sin FK por aislamiento).
    tx_compromiso_id      UUID UNIQUE,
    tx_liberacion_id      UUID UNIQUE,
    -- La orden del ledger del mercado que la ejecutó. Diferida: se liga antes de insertarla.
    orden_ejecutada_id    UUID UNIQUE REFERENCES mercado.ordenes (id) ON DELETE RESTRICT DEFERRABLE INITIALLY DEFERRED,
    clave_idempotencia    TEXT NOT NULL UNIQUE,
    creado_en             TIMESTAMPTZ NOT NULL DEFAULT now(),
    cerrado_en            TIMESTAMPTZ,

    CONSTRAINT ck_pendiente_precio CHECK (
        (tipo = 'LIMITE') = (precio_limite IS NOT NULL) AND (tipo = 'STOP') = (precio_stop IS NOT NULL)),
    CONSTRAINT ck_pendiente_compromiso CHECK (
        (lado = 'COMPRA' AND efectivo_comprometido = monto AND titulos_comprometidos = 0
                         AND tx_compromiso_id IS NOT NULL AND (estado = 'PENDIENTE') = (tx_liberacion_id IS NULL))
     OR (lado = 'VENTA'  AND efectivo_comprometido = 0 AND titulos_comprometidos > 0
                         AND tx_compromiso_id IS NULL AND tx_liberacion_id IS NULL)),
    CONSTRAINT ck_pendiente_cierre CHECK (
        (estado = 'PENDIENTE') = (cerrado_en IS NULL)
        AND (estado = 'EJECUTADA') = (orden_ejecutada_id IS NOT NULL)
        AND (estado = 'RECHAZADA') = (motivo IS NOT NULL))
);
CREATE INDEX ix_mercado_pendientes_cliente ON mercado.ordenes_pendientes (cliente_id, creado_en DESC);
CREATE INDEX ix_mercado_pendientes_abiertas ON mercado.ordenes_pendientes (ticker) WHERE estado = 'PENDIENTE';

COMMENT ON TABLE mercado.ordenes_pendientes IS
    'Órdenes límite y stop en espera (0015): términos inmutables, estado que solo avanza.';

-- Al nacer: pendiente, de la bóveda del cliente, sobre un activo abierto, con
-- un stop que el precio aún no alcanza y, si vende, títulos libres.
CREATE FUNCTION mercado.fn_validar_pendiente() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    c ifpe.cuentas%ROWTYPE;
    a mercado.activos%ROWTYPE;
    p mercado.posiciones%ROWTYPE;
BEGIN
    IF NEW.estado <> 'PENDIENTE' OR NEW.cerrado_en IS NOT NULL OR NEW.orden_ejecutada_id IS NOT NULL
       OR NEW.tx_liberacion_id IS NOT NULL OR NEW.motivo IS NOT NULL THEN
        RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: una orden pendiente nace pendiente';
    END IF;
    NEW.creado_en := clock_timestamp();

    SELECT * INTO c FROM ifpe.cuentas WHERE id = NEW.cuenta_ifpe_id;
    IF c.id IS NULL OR c.cliente_id <> NEW.cliente_id OR c.moneda <> 'MXN' THEN
        RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: la cuenta % no es la bóveda MXN del cliente %',
            NEW.cuenta_ifpe_id, NEW.cliente_id;
    END IF;
    -- Congelamiento preventivo: el error constante, sin aviso (como 0013).
    IF c.estado = 'CONGELADO_PREVENTIVO' OR aml.cliente_congelado(NEW.cliente_id) THEN
        RAISE EXCEPTION 'SC_OPERACION_NO_DISPONIBLE: la operación no está disponible en este momento'
            USING ERRCODE = 'check_violation';
    ELSIF c.estado = 'CONGELADA' THEN
        RAISE EXCEPTION 'SC_CUENTA_CONGELADA: la cuenta IFPE está congelada' USING ERRCODE = 'check_violation';
    ELSIF c.estado <> 'ACTIVA' THEN
        RAISE EXCEPTION 'SC_CUENTA_CERRADA: la cuenta IFPE está cerrada' USING ERRCODE = 'check_violation';
    END IF;

    SELECT * INTO a FROM mercado.activos WHERE ticker = NEW.ticker FOR SHARE;
    IF a.ticker IS NULL THEN
        RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: el activo % no existe', NEW.ticker;
    END IF;
    IF NOT a.operable THEN
        RAISE EXCEPTION 'SC_MERCADO_CERRADO: % no opera en este momento', NEW.ticker USING ERRCODE = 'check_violation';
    END IF;
    -- Un stop que el precio ya alcanzó se dispararía al instante.
    IF NEW.tipo = 'STOP' AND ((NEW.lado = 'COMPRA' AND NEW.precio_stop <= a.precio)
                           OR (NEW.lado = 'VENTA' AND NEW.precio_stop >= a.precio)) THEN
        RAISE EXCEPTION 'SC_PRECIO_STOP_INVALIDO: el stop % de % ya está alcanzado (precio %)',
            NEW.precio_stop, NEW.ticker, a.precio
            USING ERRCODE = 'check_violation';
    END IF;

    IF NEW.lado = 'VENTA' THEN
        IF NEW.titulos_comprometidos <> trunc(NEW.titulos_comprometidos, a.decimales_cantidad) THEN
            RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: % no es múltiplo del incremento de %',
                NEW.titulos_comprometidos, NEW.ticker;
        END IF;
        SELECT * INTO p FROM mercado.posiciones WHERE cliente_id = NEW.cliente_id AND ticker = NEW.ticker FOR UPDATE;
        IF p.cliente_id IS NULL OR p.cantidad - p.cantidad_comprometida < NEW.titulos_comprometidos THEN
            RAISE EXCEPTION 'SC_POSICION_INSUFICIENTE: no hay % títulos libres de %',
                NEW.titulos_comprometidos, NEW.ticker
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    RETURN NEW;
END
$$;

-- Después: solo el cierre, una vez, sin tocar los términos.
CREATE FUNCTION mercado.fn_transicion_pendiente() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF OLD.estado <> 'PENDIENTE' THEN
        RAISE EXCEPTION 'SC_ORDEN_NO_PENDIENTE: la orden % ya está %', OLD.id, lower(OLD.estado)
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.estado = 'PENDIENTE' THEN
        RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: una orden pendiente solo cambia para cerrarse';
    END IF;
    IF (NEW.id, NEW.cliente_id, NEW.cuenta_ifpe_id, NEW.ticker, NEW.lado, NEW.tipo, NEW.monto, NEW.precio_limite,
        NEW.precio_stop, NEW.efectivo_comprometido, NEW.titulos_comprometidos, NEW.tx_compromiso_id,
        NEW.clave_idempotencia, NEW.creado_en)
       IS DISTINCT FROM
       (OLD.id, OLD.cliente_id, OLD.cuenta_ifpe_id, OLD.ticker, OLD.lado, OLD.tipo, OLD.monto, OLD.precio_limite,
        OLD.precio_stop, OLD.efectivo_comprometido, OLD.titulos_comprometidos, OLD.tx_compromiso_id,
        OLD.clave_idempotencia, OLD.creado_en) THEN
        RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: los términos de la orden % no cambian', OLD.id;
    END IF;
    NEW.cerrado_en := clock_timestamp();
    RETURN NEW;
END
$$;

-- Compromete o libera los títulos de una venta, y sube la versión de la bóveda.
CREATE FUNCTION mercado.fn_aplicar_pendiente() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF NEW.lado = 'VENTA' THEN
        UPDATE mercado.posiciones
           SET cantidad_comprometida = cantidad_comprometida
                   + CASE WHEN TG_OP = 'INSERT' THEN NEW.titulos_comprometidos ELSE -NEW.titulos_comprometidos END,
               actualizado_en = now()
         WHERE cliente_id = NEW.cliente_id AND ticker = NEW.ticker;
    END IF;
    UPDATE ifpe.cuentas SET version_ordenes = version_ordenes + 1 WHERE id = NEW.cuenta_ifpe_id;
    RETURN NULL;
END
$$;

CREATE TRIGGER trg_pendientes_a_validar BEFORE INSERT ON mercado.ordenes_pendientes
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_validar_pendiente();
CREATE TRIGGER trg_pendientes_transicion BEFORE UPDATE ON mercado.ordenes_pendientes
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_transicion_pendiente();
CREATE TRIGGER trg_pendientes_aplicar AFTER INSERT OR UPDATE ON mercado.ordenes_pendientes
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_aplicar_pendiente();
CREATE TRIGGER trg_pendientes_sin_borrado BEFORE DELETE ON mercado.ordenes_pendientes
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_pendientes_no_truncate BEFORE TRUNCATE ON mercado.ordenes_pendientes
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- -----------------------------------------------------------------------------
-- 5. Al COMMIT: compromisos = órdenes pendientes, y cada movimiento con su orden
-- -----------------------------------------------------------------------------
-- El efectivo comprometido de la cuenta es la suma de sus compras pendientes.
CREATE FUNCTION mercado.fn_verificar_efectivo_comprometido() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_cuenta UUID;
    vivo     NUMERIC;
    ordenes  NUMERIC;
BEGIN
    IF TG_TABLE_NAME = 'cuentas' THEN v_cuenta := NEW.id; ELSE v_cuenta := NEW.cuenta_ifpe_id; END IF;
    SELECT saldo_comprometido INTO vivo FROM ifpe.cuentas WHERE id = v_cuenta;
    SELECT coalesce(sum(efectivo_comprometido), 0) INTO ordenes
      FROM mercado.ordenes_pendientes WHERE cuenta_ifpe_id = v_cuenta AND estado = 'PENDIENTE';
    IF coalesce(vivo, 0) <> ordenes THEN
        RAISE EXCEPTION 'SC_COMPROMISO_DESCUADRE: la cuenta % tiene % comprometido pero sus compras pendientes suman %',
            v_cuenta, coalesce(vivo, 0), ordenes
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_ifpe_cuentas_comprometido_ordenes
    AFTER UPDATE OF saldo_comprometido ON ifpe.cuentas
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION mercado.fn_verificar_efectivo_comprometido();
CREATE CONSTRAINT TRIGGER trg_pendientes_efectivo_comprometido
    AFTER INSERT OR UPDATE ON mercado.ordenes_pendientes
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION mercado.fn_verificar_efectivo_comprometido();

-- Los títulos comprometidos de la posición son la suma de sus ventas pendientes.
CREATE FUNCTION mercado.fn_verificar_titulos_comprometidos() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    vivo    NUMERIC;
    ordenes NUMERIC;
BEGIN
    SELECT cantidad_comprometida INTO vivo
      FROM mercado.posiciones WHERE cliente_id = NEW.cliente_id AND ticker = NEW.ticker;
    SELECT coalesce(sum(titulos_comprometidos), 0) INTO ordenes
      FROM mercado.ordenes_pendientes
     WHERE cliente_id = NEW.cliente_id AND ticker = NEW.ticker AND estado = 'PENDIENTE';
    IF coalesce(vivo, 0) <> ordenes THEN
        RAISE EXCEPTION 'SC_COMPROMISO_DESCUADRE: la posición de % en % tiene % títulos comprometidos pero sus ventas pendientes suman %',
            NEW.cliente_id, NEW.ticker, coalesce(vivo, 0), ordenes
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_posiciones_titulos_comprometidos
    AFTER INSERT OR UPDATE OF cantidad_comprometida ON mercado.posiciones
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION mercado.fn_verificar_titulos_comprometidos();
CREATE CONSTRAINT TRIGGER trg_pendientes_titulos_comprometidos
    AFTER INSERT OR UPDATE ON mercado.ordenes_pendientes
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION mercado.fn_verificar_titulos_comprometidos();

-- Cada COMPROMISO_ORDEN / LIBERACION_ORDEN es el de su compra pendiente, en la
-- transacción que la creó o la cerró, por el mismo monto y la misma cuenta.
CREATE FUNCTION mercado.fn_compromiso_con_orden() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    t      ifpe.transacciones;
    t_xmin xid;
    p      mercado.ordenes_pendientes;
    p_xmin xid;
BEGIN
    SELECT * INTO t FROM ifpe.transacciones WHERE id = NEW.id;
    SELECT x.xmin INTO t_xmin FROM ifpe.transacciones x WHERE x.id = NEW.id;
    SELECT * INTO p FROM mercado.ordenes_pendientes WHERE id = t.ref_movimiento_espejo;
    SELECT x.xmin INTO p_xmin FROM mercado.ordenes_pendientes x WHERE x.id = t.ref_movimiento_espejo;
    IF p.id IS NULL OR p_xmin IS DISTINCT FROM t_xmin OR p.lado <> 'COMPRA'
       OR p.cuenta_ifpe_id <> t.cuenta_origen_id OR p.efectivo_comprometido <> t.monto
       OR NOT ((t.tipo = 'COMPROMISO_ORDEN' AND p.tx_compromiso_id = t.id)
            OR (t.tipo = 'LIBERACION_ORDEN' AND p.tx_liberacion_id = t.id AND p.estado <> 'PENDIENTE')) THEN
        RAISE EXCEPTION 'SC_COMPROMISO_SIN_ORDEN: el movimiento % no corresponde a su orden pendiente en esta transacción',
            NEW.id;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_ifpe_tx_compromiso_ligado
    AFTER INSERT ON ifpe.transacciones
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW WHEN (NEW.tipo IN ('COMPROMISO_ORDEN', 'LIBERACION_ORDEN'))
    EXECUTE FUNCTION mercado.fn_compromiso_con_orden();

-- Y cada compra pendiente tiene su COMPROMISO_ORDEN al nacer y su
-- LIBERACION_ORDEN al cerrarse, en esa misma transacción.
CREATE FUNCTION mercado.fn_pendiente_con_compromiso() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    p        mercado.ordenes_pendientes;
    p_xmin   xid;
    t        ifpe.transacciones;
    t_xmin   xid;
    esperado UUID;
    tipo_tx  TEXT;
BEGIN
    SELECT * INTO p FROM mercado.ordenes_pendientes WHERE id = NEW.id;
    IF p.lado <> 'COMPRA' THEN
        RETURN NULL;
    END IF;
    SELECT x.xmin INTO p_xmin FROM mercado.ordenes_pendientes x WHERE x.id = NEW.id;
    IF TG_OP = 'INSERT' THEN
        esperado := p.tx_compromiso_id;
        tipo_tx := 'COMPROMISO_ORDEN';
    ELSE
        esperado := p.tx_liberacion_id;
        tipo_tx := 'LIBERACION_ORDEN';
    END IF;
    SELECT * INTO t FROM ifpe.transacciones WHERE id = esperado;
    SELECT x.xmin INTO t_xmin FROM ifpe.transacciones x WHERE x.id = esperado;
    IF t.id IS NULL OR t_xmin IS DISTINCT FROM p_xmin OR t.tipo <> tipo_tx OR t.ref_movimiento_espejo <> p.id
       OR t.cuenta_origen_id <> p.cuenta_ifpe_id OR t.monto <> p.efectivo_comprometido THEN
        RAISE EXCEPTION 'SC_ORDEN_SIN_COMPROMISO: la orden % no tiene su % en esta transacción', NEW.id, tipo_tx;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_pendientes_compromiso_ligado
    AFTER INSERT OR UPDATE ON mercado.ordenes_pendientes
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION mercado.fn_pendiente_con_compromiso();

-- -----------------------------------------------------------------------------
-- 6. Órdenes del ledger del mercado: títulos libres y ejecución de pendientes
-- -----------------------------------------------------------------------------
-- Como 0013, más: una venta de mercado vende solo títulos libres; y la orden
-- que ejecuta una pendiente cumple sus términos (precio y títulos).
CREATE OR REPLACE FUNCTION mercado.fn_validar_orden() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    c          ifpe.cuentas%ROWTYPE;
    a          mercado.activos%ROWTYPE;
    p          mercado.posiciones%ROWTYPE;
    pend       mercado.ordenes_pendientes%ROWTYPE;
    paso       NUMERIC;
    libre      NUMERIC;
    valor      NUMERIC;
    esperado   NUMERIC;
    unidad     CONSTANT NUMERIC := 100000000;
BEGIN
    SELECT * INTO c FROM ifpe.cuentas WHERE id = NEW.cuenta_ifpe_id;
    IF c.id IS NULL OR c.cliente_id <> NEW.cliente_id OR c.moneda <> 'MXN' THEN
        RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: la cuenta % no es la bóveda MXN del cliente %',
            NEW.cuenta_ifpe_id, NEW.cliente_id;
    END IF;
    -- Congelamiento preventivo: el mismo error constante que la guardia de débitos (sin aviso).
    IF c.estado = 'CONGELADO_PREVENTIVO' OR aml.cliente_congelado(NEW.cliente_id) THEN
        RAISE EXCEPTION 'SC_OPERACION_NO_DISPONIBLE: la operación no está disponible en este momento'
            USING ERRCODE = 'check_violation';
    ELSIF c.estado = 'CONGELADA' THEN
        RAISE EXCEPTION 'SC_CUENTA_CONGELADA: la cuenta IFPE está congelada' USING ERRCODE = 'check_violation';
    ELSIF c.estado <> 'ACTIVA' THEN
        RAISE EXCEPTION 'SC_CUENTA_CERRADA: la cuenta IFPE está cerrada' USING ERRCODE = 'check_violation';
    END IF;

    -- El activo como está ahora: abierto, con este precio y esta fuente. FOR
    -- SHARE: el precio no cambia hasta el COMMIT de la orden.
    SELECT * INTO a FROM mercado.activos WHERE ticker = NEW.ticker FOR SHARE;
    IF a.ticker IS NULL THEN
        RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: el activo % no existe', NEW.ticker;
    END IF;
    IF NOT a.operable THEN
        RAISE EXCEPTION 'SC_MERCADO_CERRADO: % no opera en este momento', NEW.ticker USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.precio <> a.precio OR NEW.fuente_precio <> a.fuente_precio THEN
        -- El backend reintenta la operación completa con el precio nuevo.
        RAISE EXCEPTION 'SC_MERCADO_PRECIO_CAMBIO: el precio de % cambió de % a %', NEW.ticker, NEW.precio, a.precio
            USING ERRCODE = 'check_violation';
    END IF;

    paso := power(10::numeric, -a.decimales_cantidad);
    IF NEW.cantidad <> trunc(NEW.cantidad, a.decimales_cantidad) THEN
        RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: % no es múltiplo del incremento % de %', NEW.cantidad, paso, NEW.ticker;
    END IF;

    -- ¿Ejecuta una orden pendiente? Esa la cerró en esta transacción y la apunta.
    SELECT * INTO pend FROM mercado.ordenes_pendientes WHERE orden_ejecutada_id = NEW.id;
    IF pend.id IS NOT NULL THEN
        IF pend.estado <> 'EJECUTADA' OR pend.cliente_id <> NEW.cliente_id OR pend.cuenta_ifpe_id <> NEW.cuenta_ifpe_id
           OR pend.ticker <> NEW.ticker OR pend.lado <> NEW.lado OR pend.monto <> NEW.monto_solicitado THEN
            RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: la orden % no es la ejecución de la pendiente %', NEW.id, pend.id;
        END IF;
        -- Nunca a un precio peor que el límite; un stop, solo ya alcanzado.
        IF NOT ((pend.tipo = 'LIMITE' AND ((pend.lado = 'COMPRA' AND NEW.precio <= pend.precio_limite)
                                       OR (pend.lado = 'VENTA' AND NEW.precio >= pend.precio_limite)))
             OR (pend.tipo = 'STOP' AND ((pend.lado = 'COMPRA' AND NEW.precio >= pend.precio_stop)
                                      OR (pend.lado = 'VENTA' AND NEW.precio <= pend.precio_stop)))) THEN
            RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: % a % no cumple la orden pendiente %', NEW.ticker, NEW.precio, pend.id;
        END IF;
    END IF;

    IF NEW.lado = 'COMPRA' THEN
        -- Todo lo que el monto compra: un incremento más costaría más de lo pedido.
        -- (Una compra pendiente compra con todo su efectivo comprometido.)
        IF NOT (NEW.cantidad * NEW.precio <= NEW.monto_solicitado
                AND (NEW.cantidad + paso) * NEW.precio > NEW.monto_solicitado) THEN
            RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: % % a % no es lo que compran % MXN',
                NEW.cantidad, NEW.ticker, NEW.precio, NEW.monto_solicitado;
        END IF;
        RETURN NEW;
    END IF;

    -- Venta: la posición del cliente (bloqueada hasta el COMMIT) y sus títulos libres.
    SELECT * INTO p FROM mercado.posiciones WHERE cliente_id = NEW.cliente_id AND ticker = NEW.ticker FOR UPDATE;
    IF p.cliente_id IS NULL OR p.cantidad = 0 THEN
        RAISE EXCEPTION 'SC_POSICION_INSUFICIENTE: el cliente no tiene % que vender', NEW.ticker
            USING ERRCODE = 'check_violation';
    END IF;
    libre := p.cantidad - p.cantidad_comprometida;
    IF pend.id IS NOT NULL THEN
        -- Exactamente los títulos que comprometió (su cierre ya los liberó).
        IF NEW.cantidad <> pend.titulos_comprometidos OR NEW.cantidad > libre THEN
            RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: la pendiente % vende % títulos, no %',
                pend.id, pend.titulos_comprometidos, NEW.cantidad;
        END IF;
    ELSE
        IF libre = 0 THEN
            RAISE EXCEPTION 'SC_POSICION_INSUFICIENTE: tus títulos de % están comprometidos en órdenes pendientes', NEW.ticker
                USING ERRCODE = 'check_violation';
        END IF;
        valor := libre * NEW.precio;
        IF NEW.monto_solicitado > ceil(valor * 100) / 100 THEN
            RAISE EXCEPTION 'SC_POSICION_INSUFICIENTE: % MXN excede lo que valen tus títulos libres de % (%)',
                NEW.monto_solicitado, NEW.ticker, valor
                USING ERRCODE = 'check_violation';
        END IF;
        IF NEW.monto_solicitado >= trunc(valor, 2) THEN
            -- Lo que valen todos los títulos libres, al centavo: se venden todos (sin residuos).
            IF NEW.cantidad <> libre THEN
                RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: % MXN vende todos los títulos libres (%), no %',
                    NEW.monto_solicitado, libre, NEW.cantidad;
            END IF;
        ELSIF NOT (NEW.cantidad * NEW.precio <= NEW.monto_solicitado
                   AND (NEW.cantidad + paso) * NEW.precio > NEW.monto_solicitado) THEN
            RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: % % a % no es lo que cubren % MXN',
                NEW.cantidad, NEW.ticker, NEW.precio, NEW.monto_solicitado;
        END IF;
    END IF;
    -- Costo: todo, si se vende la posición completa; si no, el promedio
    -- (costo × vendido / tenido al 10^-8, mitad hacia arriba, en enteros exactos).
    IF NEW.cantidad = p.cantidad THEN
        esperado := p.costo;
    ELSE
        esperado := div(2 * (p.costo * unidad) * (NEW.cantidad * unidad) + p.cantidad * unidad,
                        2 * (p.cantidad * unidad)) / unidad;
    END IF;
    IF NEW.costo_liberado <> esperado THEN
        RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: la venta libera % de costo, no %', esperado, NEW.costo_liberado;
    END IF;
    RETURN NEW;
END
$$;

-- -----------------------------------------------------------------------------
-- 7. Privilegios y RLS
-- -----------------------------------------------------------------------------
GRANT SELECT, INSERT, UPDATE (estado, motivo, tx_liberacion_id, orden_ejecutada_id)
    ON mercado.ordenes_pendientes TO sc_mercado;
GRANT SELECT ON mercado.ordenes_pendientes TO sc_cliente;

-- Coloca quien tiene KYC verificado y está fuera de la Lista de Personas
-- Bloqueadas; cancelar (cerrar) no lo exige: devolver lo comprometido siempre se puede.
ALTER TABLE mercado.ordenes_pendientes ENABLE ROW LEVEL SECURITY;
CREATE POLICY pendientes_lectura ON mercado.ordenes_pendientes FOR SELECT USING (true);
CREATE POLICY pendientes_alta_con_kyc ON mercado.ordenes_pendientes FOR INSERT WITH CHECK (kyc.puede_operar(cliente_id));
CREATE POLICY pendientes_cierre ON mercado.ordenes_pendientes FOR UPDATE USING (true) WITH CHECK (true);
CREATE POLICY solo_del_cliente ON mercado.ordenes_pendientes AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual())
    WITH CHECK (cliente_id = nucleo.cliente_actual());
