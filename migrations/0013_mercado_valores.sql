-- =============================================================================
--  Synthetic Capital — 0013: mercado de valores (activos, órdenes y posiciones)
-- =============================================================================
--
--  El cliente compra y vende activos (oro y plata tokenizados, certificados)
--  con el efectivo de su bóveda. El mercado es una entidad aparte de la IFPE y
--  de la SOFOM, como ellas lo son entre sí:
--
--  1. Esquema `mercado` y rol `sc_mercado`, el único que coloca órdenes. Mueve
--     efectivo entre la bóveda y el mercado (como `sc_puente` con la SOFOM),
--     no toca la SOFOM y en el ledger IFPE solo escribe sus dos movimientos.
--  2. `mercado.activos`: el catálogo y su precio. Hasta que llegue el oráculo
--     de precios todos son SIMULADO, y el backend solo los cotiza y los opera
--     con MODO_DEMO (backend/src/mercado). Ninguna ruta los modifica.
--  3. `mercado.ordenes`: el ledger del mercado. Una orden de mercado se ejecuta
--     completa al colocarse: cada una es un renglón inmutable con secuencia y
--     `tx_hash = SHA-256(payload canónico SC-MERCADO-v1)` calculados por la
--     base de datos, como en los ledgers IFPE y SOFOM. La base de datos revisa
--     cada renglón contra el catálogo y la posición: el precio vigente, la
--     cantidad en el incremento del activo, la compra máxima que cubre el monto,
--     el importe al centavo, la posición que alcanza para vender y el costo que
--     libera la venta. Comisión: cero, por restricción.
--  4. `mercado.posiciones`: lo que tiene cada cliente y lo que le costó. Solo
--     la mueve el trigger de las órdenes, y al COMMIT debe coincidir con la
--     reproducción de su ledger (SC_POSICION_SIN_LEDGER).
--  5. Las patas de efectivo en el ledger IFPE: COMPRA_MERCADO (-cliente,
--     -reserva: el efectivo sale a liquidar la compra) y VENTA_MERCADO
--     (+cliente, +reserva). Cada una con su orden en la misma transacción y
--     cada orden con la suya (SC_MERCADO_SIN_ORDEN, SC_ORDEN_SIN_MOVIMIENTO).
--     La reserva del 100%, las sombras y la cadena de hashes no cambian.
--  6. KYC y PLD: solo opera quien tiene KYC verificado (RLS, como en 0006). La
--     compra es un canal PLD nuevo, COMPRA_MERCADO (SALIDA), con la evaluación
--     en línea, la guardia de congelamiento, la cobertura y la velocidad de la
--     inversión en mutuo. La venta, como RETORNO_DE_SOFOM, es dinero del propio
--     cliente que regresa: no se evalúa. Un cliente congelado no compra ni
--     vende (SC_OPERACION_NO_DISPONIBLE, el error constante, sin aviso).
--  7. RLS: sc_cliente lee el catálogo completo y solo sus órdenes y posiciones.
--
--  El WebSocket de la bóveda no necesita nada nuevo: una orden cambia el saldo
--  de la cuenta IFPE dentro de su transacción, así que `seq` sube y
--  `sc_boveda` avisa al confirmar (0012).
--
--  Orden global de candados (el del encabezado de backend/src/ledger) para una
--  orden: cuenta IFPE del cliente → candado PLD del cliente (solo compras) →
--  reserva → [estado_reserva → cabeza IFPE → activo (FOR SHARE) → posición →
--  cabeza del mercado], que toman los triggers. La SOFOM no participa.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. Rol y esquema
-- -----------------------------------------------------------------------------
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sc_mercado') THEN
        CREATE ROLE sc_mercado NOLOGIN;
    END IF;
    EXECUTE format('GRANT sc_mercado TO %I', current_user);
END
$$;

CREATE SCHEMA mercado;
REVOKE ALL ON SCHEMA mercado FROM PUBLIC;
COMMENT ON SCHEMA mercado IS
    'Mercado de valores: catálogo de activos, órdenes ejecutadas (ledger encadenado) y posiciones. Sin acceso a la SOFOM.';

-- -----------------------------------------------------------------------------
-- 2. Catálogo de activos
-- -----------------------------------------------------------------------------
CREATE TABLE mercado.activos (
    ticker               TEXT PRIMARY KEY CHECK (ticker ~ '^[A-Z][A-Z0-9]{0,11}$'),
    nombre               TEXT NOT NULL CHECK (length(btrim(nombre)) BETWEEN 1 AND 120),
    clase                TEXT NOT NULL CHECK (clase IN ('MATERIA_PRIMA', 'RENTA_FIJA', 'RENTA_VARIABLE', 'FONDO')),
    -- Incremento mínimo de la cantidad: 10^-decimales_cantidad unidades.
    decimales_cantidad   SMALLINT NOT NULL CHECK (decimales_cantidad BETWEEN 0 AND 8),
    precio               NUMERIC(18, 8) NOT NULL CHECK (precio > 0),
    precio_cierre_previo NUMERIC(18, 8) NOT NULL CHECK (precio_cierre_previo > 0),
    capitalizacion       NUMERIC(20, 2) CHECK (capitalizacion >= 0),
    volumen_24h          NUMERIC(20, 2) CHECK (volumen_24h >= 0),
    -- Rendimiento anual en por ciento (NULL si el activo no paga).
    rendimiento_anual    NUMERIC(9, 4) CHECK (rendimiento_anual > -100 AND rendimiento_anual < 1000),
    -- SIMULADO hasta el oráculo de precios: el backend no lo opera sin MODO_DEMO.
    fuente_precio        TEXT NOT NULL CHECK (fuente_precio IN ('SIMULADO', 'ORACULO')),
    precio_en            TIMESTAMPTZ NOT NULL DEFAULT now(),
    operable             BOOLEAN NOT NULL DEFAULT TRUE,
    creado_en            TIMESTAMPTZ NOT NULL DEFAULT now(),
    actualizado_en       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TRIGGER trg_activos_actualizado_en BEFORE UPDATE ON mercado.activos
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_tocar_actualizado_en();

-- Las órdenes y las posiciones existentes se miden en el incremento del activo.
CREATE FUNCTION mercado.fn_proteger_activo() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF (NEW.ticker, NEW.decimales_cantidad) IS DISTINCT FROM (OLD.ticker, OLD.decimales_cantidad) THEN
        RAISE EXCEPTION 'SC_ACTIVO_INMUTABLE: el ticker y el incremento de % no cambian', OLD.ticker
            USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_activos_proteger BEFORE UPDATE ON mercado.activos
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_proteger_activo();

-- -----------------------------------------------------------------------------
-- 3. Posiciones: lo que tiene cada cliente. Solo las mueve el ledger del mercado
-- -----------------------------------------------------------------------------
CREATE TABLE mercado.posiciones (
    cliente_id     UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    ticker         TEXT NOT NULL REFERENCES mercado.activos (ticker) ON DELETE RESTRICT,
    cantidad       NUMERIC(18, 8) NOT NULL DEFAULT 0
                       CONSTRAINT ck_mercado_posicion_no_negativa CHECK (cantidad >= 0),
    -- Lo que costaron las unidades que se tienen (costo promedio).
    costo          NUMERIC(18, 8) NOT NULL DEFAULT 0 CHECK (costo >= 0),
    actualizado_en TIMESTAMPTZ NOT NULL DEFAULT now(),

    PRIMARY KEY (cliente_id, ticker),
    CONSTRAINT ck_mercado_posicion_cerrada_sin_costo CHECK (cantidad > 0 OR costo = 0)
);

-- -----------------------------------------------------------------------------
-- 4. Ledger del mercado: órdenes ejecutadas (inmutable, encadenado por hash)
-- -----------------------------------------------------------------------------
CREATE TABLE mercado.ledger_cabeza (
    id               BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    ultima_secuencia BIGINT NOT NULL DEFAULT 0,
    ultimo_hash      CHAR(64) NOT NULL DEFAULT nucleo.hash_genesis()
);
INSERT INTO mercado.ledger_cabeza DEFAULT VALUES;

CREATE TABLE mercado.ordenes (
    id                 UUID PRIMARY KEY,
    secuencia          BIGINT NOT NULL UNIQUE CONSTRAINT ck_mercado_ordenes_secuencia_positiva CHECK (secuencia > 0),
    cliente_id         UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    -- Bóveda que paga la compra o cobra la venta (sin FK por aislamiento).
    cuenta_ifpe_id     UUID NOT NULL,
    ticker             TEXT NOT NULL REFERENCES mercado.activos (ticker) ON DELETE RESTRICT,
    lado               TEXT NOT NULL CHECK (lado IN ('COMPRA', 'VENTA')),
    -- Lo que pidió el cliente, en pesos y centavos.
    monto_solicitado   NUMERIC(18, 8) NOT NULL
                           CHECK (monto_solicitado > 0 AND monto_solicitado = trunc(monto_solicitado, 2)),
    cantidad           NUMERIC(18, 8) NOT NULL CHECK (cantidad > 0),
    precio             NUMERIC(18, 8) NOT NULL CHECK (precio > 0),
    -- Lo que se cargó o abonó en la bóveda: cantidad × precio al centavo.
    monto              NUMERIC(18, 8) NOT NULL CHECK (monto > 0),
    comision           NUMERIC(18, 8) NOT NULL DEFAULT 0 CONSTRAINT ck_mercado_ordenes_sin_comision CHECK (comision = 0),
    -- Venta: costo de las unidades vendidas (costo promedio). NULL en compras.
    costo_liberado     NUMERIC(18, 8) CHECK (costo_liberado >= 0),
    fuente_precio      TEXT NOT NULL CHECK (fuente_precio IN ('SIMULADO', 'ORACULO')),
    -- Movimiento COMPRA_MERCADO / VENTA_MERCADO del ledger IFPE (sin FK por aislamiento).
    tx_ifpe_id         UUID NOT NULL UNIQUE,
    clave_idempotencia TEXT NOT NULL UNIQUE,
    hash_anterior      CHAR(64) NOT NULL,
    tx_hash            CHAR(64) NOT NULL UNIQUE CHECK (tx_hash ~ '^[0-9a-f]{64}$'),
    creado_en          TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ck_mercado_ordenes_importe CHECK (monto = round(cantidad * precio, 2)),
    CONSTRAINT ck_mercado_ordenes_compra_cubierta CHECK (lado = 'VENTA' OR monto <= monto_solicitado),
    CONSTRAINT ck_mercado_ordenes_costo CHECK ((lado = 'COMPRA') = (costo_liberado IS NULL))
);
CREATE INDEX ix_mercado_ordenes_cliente ON mercado.ordenes (cliente_id, ticker, secuencia);

COMMENT ON TABLE mercado.ordenes IS
    'Ledger del mercado: una orden ejecutada por renglón, inmutable y encadenada (SC-MERCADO-v1).';

-- Payload canónico v1 (backend/src/ledger/hash.rs produce exactamente lo mismo):
--   'SC-MERCADO-v1' | secuencia | id | cliente_id | cuenta_ifpe_id | ticker | lado
--   | monto_solicitado | cantidad | precio | monto | comision | costo_liberado
--   | fuente_precio | tx_ifpe_id | clave_idempotencia | creado_en(epoch µs) | hash_anterior
CREATE FUNCTION mercado.payload_canonico(o mercado.ordenes) RETURNS TEXT
LANGUAGE sql IMMUTABLE
AS $$
    SELECT concat_ws('|',
        nucleo.campo('SC-MERCADO-v1'),
        nucleo.campo(o.secuencia::text),
        nucleo.campo(o.id::text),
        nucleo.campo(o.cliente_id::text),
        nucleo.campo(o.cuenta_ifpe_id::text),
        nucleo.campo(o.ticker),
        nucleo.campo(o.lado),
        nucleo.campo(o.monto_solicitado::text),
        nucleo.campo(o.cantidad::text),
        nucleo.campo(o.precio::text),
        nucleo.campo(o.monto::text),
        nucleo.campo(o.comision::text),
        nucleo.campo(o.costo_liberado::text),
        nucleo.campo(o.fuente_precio),
        nucleo.campo(o.tx_ifpe_id::text),
        nucleo.campo(o.clave_idempotencia),
        nucleo.campo(nucleo.epoch_micros(o.creado_en)),
        nucleo.campo(o.hash_anterior::text)
    )
$$;

-- Revisión de la orden contra la cuenta, el catálogo y la posición. Su nombre
-- la ordena antes de trg_ordenes_encadenar: rechaza antes del candado de la cabeza.
CREATE FUNCTION mercado.fn_validar_orden() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    c          ifpe.cuentas%ROWTYPE;
    a          mercado.activos%ROWTYPE;
    p          mercado.posiciones%ROWTYPE;
    paso       NUMERIC;
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

    IF NEW.lado = 'COMPRA' THEN
        -- Todo lo que el monto compra: un incremento más costaría más de lo pedido.
        IF NOT (NEW.cantidad * NEW.precio <= NEW.monto_solicitado
                AND (NEW.cantidad + paso) * NEW.precio > NEW.monto_solicitado) THEN
            RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: % % a % no es lo que compran % MXN',
                NEW.cantidad, NEW.ticker, NEW.precio, NEW.monto_solicitado;
        END IF;
        RETURN NEW;
    END IF;

    -- Venta: la posición del cliente (bloqueada hasta el COMMIT).
    SELECT * INTO p FROM mercado.posiciones WHERE cliente_id = NEW.cliente_id AND ticker = NEW.ticker FOR UPDATE;
    IF p.cliente_id IS NULL OR p.cantidad = 0 THEN
        RAISE EXCEPTION 'SC_POSICION_INSUFICIENTE: el cliente no tiene % que vender', NEW.ticker
            USING ERRCODE = 'check_violation';
    END IF;
    valor := p.cantidad * NEW.precio;
    IF NEW.monto_solicitado > ceil(valor * 100) / 100 THEN
        RAISE EXCEPTION 'SC_POSICION_INSUFICIENTE: % MXN excede lo que vale la posición en % (%)',
            NEW.monto_solicitado, NEW.ticker, valor
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.monto_solicitado >= trunc(valor, 2) THEN
        -- Lo que vale toda la posición, al centavo: se vende completa (sin residuos).
        esperado := p.cantidad;
        IF NEW.cantidad <> esperado THEN
            RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: % MXN vende toda la posición (%), no %',
                NEW.monto_solicitado, esperado, NEW.cantidad;
        END IF;
        IF NEW.costo_liberado <> p.costo THEN
            RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: vender toda la posición libera su costo (%), no %',
                p.costo, NEW.costo_liberado;
        END IF;
        RETURN NEW;
    END IF;
    IF NOT (NEW.cantidad * NEW.precio <= NEW.monto_solicitado
            AND (NEW.cantidad + paso) * NEW.precio > NEW.monto_solicitado) THEN
        RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: % % a % no es lo que cubren % MXN',
            NEW.cantidad, NEW.ticker, NEW.precio, NEW.monto_solicitado;
    END IF;
    -- Costo promedio: costo × vendido / tenido, al 10^-8 (mitad hacia arriba),
    -- en enteros exactos de 10^-8.
    esperado := div(2 * (p.costo * unidad) * (NEW.cantidad * unidad) + p.cantidad * unidad,
                    2 * (p.cantidad * unidad)) / unidad;
    IF NEW.costo_liberado <> esperado THEN
        RAISE EXCEPTION 'SC_ORDEN_INCOHERENTE: la venta libera % de costo, no %', esperado, NEW.costo_liberado;
    END IF;
    RETURN NEW;
END
$$;

-- Asigna secuencia, hash_anterior, creado_en y tx_hash; bloquea la cabeza (orden total).
CREATE FUNCTION mercado.fn_encadenar_orden() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    cabeza mercado.ledger_cabeza%ROWTYPE;
BEGIN
    SELECT * INTO cabeza FROM mercado.ledger_cabeza WHERE id FOR UPDATE;

    NEW.secuencia     := cabeza.ultima_secuencia + 1;
    NEW.hash_anterior := cabeza.ultimo_hash;
    NEW.creado_en     := clock_timestamp();
    NEW.tx_hash       := nucleo.sha256_hex(mercado.payload_canonico(NEW));

    UPDATE mercado.ledger_cabeza
       SET ultima_secuencia = NEW.secuencia, ultimo_hash = NEW.tx_hash
     WHERE id;
    RETURN NEW;
END
$$;

-- Las órdenes mueven las posiciones (solo este trigger las escribe).
CREATE FUNCTION mercado.fn_aplicar_orden() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF NEW.lado = 'COMPRA' THEN
        INSERT INTO mercado.posiciones AS p (cliente_id, ticker, cantidad, costo)
        VALUES (NEW.cliente_id, NEW.ticker, NEW.cantidad, NEW.monto)
        ON CONFLICT (cliente_id, ticker) DO UPDATE
           SET cantidad = p.cantidad + EXCLUDED.cantidad,
               costo = p.costo + EXCLUDED.costo,
               actualizado_en = now();
    ELSE
        UPDATE mercado.posiciones
           SET cantidad = cantidad - NEW.cantidad,
               costo = costo - NEW.costo_liberado,
               actualizado_en = now()
         WHERE cliente_id = NEW.cliente_id AND ticker = NEW.ticker;
    END IF;
    RETURN NULL;
END
$$;

CREATE TRIGGER trg_ordenes_a_validar BEFORE INSERT ON mercado.ordenes
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_validar_orden();
CREATE TRIGGER trg_ordenes_encadenar BEFORE INSERT ON mercado.ordenes
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_encadenar_orden();
CREATE TRIGGER trg_ordenes_posicion AFTER INSERT ON mercado.ordenes
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_aplicar_orden();
CREATE TRIGGER trg_ordenes_inmutable BEFORE UPDATE OR DELETE ON mercado.ordenes
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_ordenes_no_truncate BEFORE TRUNCATE ON mercado.ordenes
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- Al COMMIT, cada posición modificada es exactamente la reproducción de su
-- ledger: cantidad = compras - ventas; costo = importes de compra - costos liberados.
CREATE FUNCTION mercado.fn_verificar_posicion() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    viva     mercado.posiciones%ROWTYPE;
    cantidad NUMERIC;
    costo    NUMERIC;
BEGIN
    SELECT * INTO viva FROM mercado.posiciones WHERE cliente_id = NEW.cliente_id AND ticker = NEW.ticker;
    SELECT coalesce(sum(CASE o.lado WHEN 'COMPRA' THEN o.cantidad ELSE -o.cantidad END), 0),
           coalesce(sum(CASE o.lado WHEN 'COMPRA' THEN o.monto ELSE -o.costo_liberado END), 0)
      INTO cantidad, costo
      FROM mercado.ordenes o
     WHERE o.cliente_id = NEW.cliente_id AND o.ticker = NEW.ticker;
    IF coalesce(viva.cantidad, 0) <> cantidad OR coalesce(viva.costo, 0) <> costo THEN
        RAISE EXCEPTION 'SC_POSICION_SIN_LEDGER: la posición de % en % es (%, %) pero el ledger justifica (%, %)',
            NEW.cliente_id, NEW.ticker, coalesce(viva.cantidad, 0), coalesce(viva.costo, 0), cantidad, costo
            USING ERRCODE = 'check_violation',
                  HINT = 'Toda posición se mueve con una orden del ledger del mercado, en la misma transacción.';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_posiciones_ledger
    AFTER INSERT OR UPDATE ON mercado.posiciones
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_verificar_posicion();

-- -----------------------------------------------------------------------------
-- 5. Patas de efectivo en el ledger IFPE
-- -----------------------------------------------------------------------------
-- NOT VALID: las restricciones nuevas admiten todo lo que admitían las de
-- 0001, así que los renglones existentes ya las cumplen. Sin recorrer el ledger
-- con un candado exclusivo; los renglones nuevos se revisan igual.
ALTER TABLE ifpe.transacciones DROP CONSTRAINT transacciones_tipo_check;
ALTER TABLE ifpe.transacciones ADD CONSTRAINT transacciones_tipo_check CHECK (tipo IN (
    'DEPOSITO', 'RETIRO', 'TRANSFERENCIA', 'TRASPASO_A_SOFOM', 'RETORNO_DE_SOFOM', 'RETENCION', 'LIBERACION_RETENCION',
    'COMPRA_MERCADO',  -- compra de valores: -cliente, -reserva (el efectivo sale a liquidar la compra)
    'VENTA_MERCADO'    -- venta de valores: +cliente, +reserva
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
                               AND ref_movimiento_espejo IS NOT NULL)
) NOT VALID;

-- Saldos sombra (0004) y puerta KYC (0006) con los dos tipos nuevos.
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
    END CASE;
    RETURN NULL;
END
$$;

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
        -- RETORNO_DE_SOFOM, RETENCION, LIBERACION_RETENCION.
        ELSE TRUE
    END
$$;

-- Solo sc_mercado escribe los movimientos del mercado, y sc_mercado no
-- escribe otros (RESTRICTIVE: se suman a la política de KYC de 0006).
CREATE POLICY tx_mercado_exclusivo ON ifpe.transacciones AS RESTRICTIVE FOR INSERT
    WITH CHECK (tipo NOT IN ('COMPRA_MERCADO', 'VENTA_MERCADO') OR pg_has_role(current_user, 'sc_mercado', 'MEMBER'));
CREATE POLICY tx_mercado_solo_sus_movimientos ON ifpe.transacciones AS RESTRICTIVE FOR INSERT TO sc_mercado
    WITH CHECK (tipo IN ('COMPRA_MERCADO', 'VENTA_MERCADO'));

-- Cada movimiento del mercado tiene su orden en la misma transacción, por el
-- mismo monto, lado y cuenta, y se referencian mutuamente; y cada orden, su
-- movimiento. Diferidos: el movimiento IFPE se inserta antes que la orden.
CREATE FUNCTION mercado.par_ligado(t ifpe.transacciones, t_xmin xid, o mercado.ordenes, o_xmin xid) RETURNS BOOLEAN
LANGUAGE sql IMMUTABLE
AS $$
    SELECT t.id IS NOT NULL AND o.id IS NOT NULL
       AND t_xmin = o_xmin
       AND t.ref_movimiento_espejo = o.id AND o.tx_ifpe_id = t.id
       AND t.monto = o.monto
       AND ((t.tipo = 'COMPRA_MERCADO' AND o.lado = 'COMPRA' AND t.cuenta_origen_id = o.cuenta_ifpe_id)
         OR (t.tipo = 'VENTA_MERCADO' AND o.lado = 'VENTA' AND t.cuenta_destino_id = o.cuenta_ifpe_id))
$$;

CREATE FUNCTION mercado.fn_movimiento_con_orden() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    t      ifpe.transacciones;
    t_xmin xid;
    o      mercado.ordenes;
    o_xmin xid;
BEGIN
    SELECT * INTO t FROM ifpe.transacciones WHERE id = NEW.id;
    SELECT x.xmin INTO t_xmin FROM ifpe.transacciones x WHERE x.id = NEW.id;
    SELECT * INTO o FROM mercado.ordenes WHERE id = t.ref_movimiento_espejo;
    SELECT x.xmin INTO o_xmin FROM mercado.ordenes x WHERE x.id = t.ref_movimiento_espejo;
    IF NOT coalesce(mercado.par_ligado(t, t_xmin, o, o_xmin), FALSE) THEN
        RAISE EXCEPTION 'SC_MERCADO_SIN_ORDEN: el movimiento % no tiene su orden en esta transacción', NEW.id;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_ifpe_tx_mercado_ligado
    AFTER INSERT ON ifpe.transacciones
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW WHEN (NEW.tipo IN ('COMPRA_MERCADO', 'VENTA_MERCADO'))
    EXECUTE FUNCTION mercado.fn_movimiento_con_orden();

CREATE FUNCTION mercado.fn_orden_con_movimiento() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    t      ifpe.transacciones;
    t_xmin xid;
    o      mercado.ordenes;
    o_xmin xid;
BEGIN
    SELECT * INTO o FROM mercado.ordenes WHERE id = NEW.id;
    SELECT x.xmin INTO o_xmin FROM mercado.ordenes x WHERE x.id = NEW.id;
    SELECT * INTO t FROM ifpe.transacciones WHERE id = o.tx_ifpe_id;
    SELECT x.xmin INTO t_xmin FROM ifpe.transacciones x WHERE x.id = o.tx_ifpe_id;
    IF NOT coalesce(mercado.par_ligado(t, t_xmin, o, o_xmin), FALSE) THEN
        RAISE EXCEPTION 'SC_ORDEN_SIN_MOVIMIENTO: la orden % no tiene su movimiento IFPE en esta transacción', NEW.id;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_ordenes_ligada
    AFTER INSERT ON mercado.ordenes
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION mercado.fn_orden_con_movimiento();

-- -----------------------------------------------------------------------------
-- 6. PLD: la compra es el canal COMPRA_MERCADO (SALIDA), como la inversión en mutuo
-- -----------------------------------------------------------------------------
ALTER TABLE aml.evaluaciones DROP CONSTRAINT evaluaciones_canal_check;
ALTER TABLE aml.evaluaciones ADD CONSTRAINT evaluaciones_canal_check CHECK (canal IN (
    'SPEI_WEBHOOK', 'SPEI_OPERADOR', 'SPEI_DEMO', 'TRANSFERENCIA_RECIBIDA', 'TRANSFERENCIA_ENVIADA', 'SPEI_ENVIADO',
    'INVERSION_MUTUO', 'COMPRA_MERCADO', 'DESEMBOLSO_CREDITO', 'COBRO_CREDITO'
)) NOT VALID;

-- Funciones de 0009 con el canal nuevo (cada cambio, marcado en su sitio):
-- calificador, velocidad, guardia de débitos, cobertura, evaluación ligada a su
-- renglón, candidatos del monitoreo por lotes y envoltura de la IFPE. Ninguna
-- banda de reglas cambia: como la inversión en mutuo, la compra no cuenta para
-- la estructuración ni para el paso rápido de fondos, solo para la velocidad.
CREATE OR REPLACE FUNCTION aml.evaluar_operacion(p_entidad TEXT, p_cliente UUID, p_sentido TEXT, p_canal TEXT, p_monto NUMERIC,
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
            AND p_canal IN ('TRANSFERENCIA_ENVIADA', 'SPEI_ENVIADO', 'INVERSION_MUTUO', 'COMPRA_MERCADO'))
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

CREATE OR REPLACE FUNCTION aml.movimientos_cliente(p_cliente UUID, p_desde TIMESTAMPTZ, p_hasta TIMESTAMPTZ, p_excluir UUID)
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
    -- Compra en el mercado de valores (0013). La venta, como RETORNO_DE_SOFOM, no cuenta.
    SELECT 'IFPE', t.id, 'SALIDA', 'COMPRA_MERCADO', t.monto::numeric, NULL::text, t.creado_en
      FROM cuenta
      JOIN ifpe.transacciones t ON t.cuenta_origen_id = cuenta.id
     WHERE t.tipo = 'COMPRA_MERCADO'
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

CREATE OR REPLACE FUNCTION aml.fn_guardia_tx_ifpe() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_estado  TEXT;
    v_cliente UUID;
BEGIN
    IF NEW.tipo NOT IN ('RETIRO', 'TRANSFERENCIA', 'TRASPASO_A_SOFOM', 'COMPRA_MERCADO') THEN
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

CREATE OR REPLACE FUNCTION aml.fn_cobertura_ifpe() RETURNS trigger
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
        WHEN 'COMPRA_MERCADO' THEN
            PERFORM aml.fn_cubrir('IFPE', NEW.id, v_xmin, 'SALIDA', v_origen, NEW.monto, 'COMPRA_MERCADO',
                                  ARRAY['COMPRA_MERCADO'], NEW.clave_idempotencia);
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

CREATE OR REPLACE FUNCTION aml.fn_evaluacion_ligada() RETURNS trigger
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
        ELSIF t.tipo = 'COMPRA_MERCADO' AND NEW.sentido = 'SALIDA' THEN
            v_canal := 'COMPRA_MERCADO';
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

CREATE OR REPLACE FUNCTION aml.monitoreo_candidatos(p_turno UUID) RETURNS TABLE (cliente_id UUID, anclas TIMESTAMPTZ[])
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
           AND t.tipo IN ('DEPOSITO', 'TRANSFERENCIA', 'RETIRO', 'TRASPASO_A_SOFOM', 'COMPRA_MERCADO')
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

CREATE OR REPLACE FUNCTION ifpe.aml_evaluar(p_cliente UUID, p_sentido TEXT, p_canal TEXT, p_monto NUMERIC, p_transaccion_id UUID,
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
            OR (v_rol = 'sc_puente' AND p_sentido = 'SALIDA' AND p_canal = 'INVERSION_MUTUO')
            OR (v_rol = 'sc_mercado' AND p_sentido = 'SALIDA' AND p_canal = 'COMPRA_MERCADO')) THEN
        RAISE EXCEPTION 'SC_AML_PARAMETRO_INVALIDO: el rol % no evalúa operaciones (%, %)', v_rol, p_sentido, p_canal;
    END IF;
    RETURN QUERY SELECT * FROM aml.evaluar_operacion('IFPE', p_cliente, p_sentido, p_canal, p_monto, p_transaccion_id,
                                                     p_clave, p_contraparte);
END
$$;

DROP TRIGGER trg_ifpe_tx_zz_aml_cobertura ON ifpe.transacciones;
CREATE CONSTRAINT TRIGGER trg_ifpe_tx_zz_aml_cobertura
    AFTER INSERT ON ifpe.transacciones
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW
    WHEN ((NEW.tipo = 'DEPOSITO' AND NEW.clave_idempotencia LIKE 'spei:%')
          OR NEW.tipo IN ('TRANSFERENCIA', 'TRASPASO_A_SOFOM', 'COMPRA_MERCADO')
          OR (NEW.tipo = 'RETIRO' AND NEW.clave_idempotencia NOT LIKE 'spei-correccion:%'))
    EXECUTE FUNCTION aml.fn_cobertura_ifpe();

-- La rama nueva de aml.movimientos_cliente (IF NOT EXISTS: puede crearse
-- antes con CONCURRENTLY en un ledger grande de producción).
CREATE INDEX IF NOT EXISTS ix_ifpe_tx_aml_mercado ON ifpe.transacciones (cuenta_origen_id, creado_en)
    INCLUDE (monto, tipo, clave_idempotencia) WHERE tipo = 'COMPRA_MERCADO';

-- -----------------------------------------------------------------------------
-- 7. Privilegios y RLS
-- -----------------------------------------------------------------------------
-- sc_mercado: el camino de las órdenes. En la IFPE, lo mismo que el puente
-- salvo que solo escribe saldo_disponible y la reserva; en el mercado, lee
-- todo e inserta órdenes (las posiciones las mueve el trigger).
GRANT USAGE ON SCHEMA nucleo, kyc, ifpe, mercado TO sc_mercado;
GRANT SELECT ON kyc.clientes TO sc_mercado;
GRANT SELECT, UPDATE (saldo_disponible) ON ifpe.cuentas TO sc_mercado;
GRANT SELECT, UPDATE (saldo) ON ifpe.reserva_custodia TO sc_mercado;
GRANT SELECT, INSERT ON ifpe.transacciones TO sc_mercado;
GRANT SELECT ON mercado.activos, mercado.posiciones, mercado.ledger_cabeza TO sc_mercado;
GRANT SELECT, INSERT ON mercado.ordenes TO sc_mercado;
GRANT EXECUTE ON FUNCTION ifpe.aml_evaluar(UUID, TEXT, TEXT, NUMERIC, UUID, TEXT, JSONB) TO sc_mercado;

-- sc_cliente (0011): el catálogo y sus propias órdenes y posiciones.
GRANT USAGE ON SCHEMA mercado TO sc_cliente;
GRANT SELECT ON mercado.activos, mercado.ordenes, mercado.posiciones TO sc_cliente;

-- Solo opera quien tiene KYC verificado y está fuera de la Lista de Personas Bloqueadas.
ALTER TABLE mercado.ordenes ENABLE ROW LEVEL SECURITY;
CREATE POLICY ordenes_lectura ON mercado.ordenes FOR SELECT USING (true);
CREATE POLICY ordenes_alta_con_kyc ON mercado.ordenes FOR INSERT WITH CHECK (kyc.puede_operar(cliente_id));
CREATE POLICY solo_del_cliente ON mercado.ordenes AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual())
    WITH CHECK (cliente_id = nucleo.cliente_actual());

ALTER TABLE mercado.posiciones ENABLE ROW LEVEL SECURITY;
CREATE POLICY por_privilegios ON mercado.posiciones USING (true) WITH CHECK (true);
CREATE POLICY solo_del_cliente ON mercado.posiciones AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual())
    WITH CHECK (cliente_id = nucleo.cliente_actual());

-- -----------------------------------------------------------------------------
-- 8. Catálogo inicial (precios SIMULADO)
-- -----------------------------------------------------------------------------
-- Datos de demostración hasta el oráculo: el backend no cotiza ni opera un
-- precio SIMULADO sin MODO_DEMO.
INSERT INTO mercado.activos (ticker, nombre, clase, decimales_cantidad, precio, precio_cierre_previo,
                             capitalizacion, volumen_24h, rendimiento_anual, fuente_precio) VALUES
    ('AUG1',  'Tokenized gold, 1 g',       'MATERIA_PRIMA', 4, 1612.40, 1598.97, 1250000000.00, 84300000.00, NULL,    'SIMULADO'),
    ('AGT1',  'Tokenized silver, 1 oz',    'MATERIA_PRIMA', 4,  548.20,  551.06,  310000000.00, 12800000.00, NULL,    'SIMULADO'),
    ('SCT28', 'Synthetic T-bill, 28 days', 'RENTA_FIJA',    0,    9.92,    9.92,           NULL,  5200000.00, 10.2500, 'SIMULADO');
