-- =============================================================================
--  Synthetic Capital — 0008: endurecimiento SPEI
-- =============================================================================
--
--  Correcciones de la revisión de 0005 (0001-0007 no se modifican):
--
--  1. `intentos_envio` nunca disminuye: el despachador lo usa como testigo de
--     su reclamo (un rechazo de registro de un reclamo vencido no reembolsa).
--  2. `ifpe.spei_resoluciones`: el operador reembolsa una orden en revisión
--     manual que el proveedor nunca recibió. Ligada al reverso de la misma
--     transacción; solo órdenes nunca aceptadas por el proveedor.
--  3. `ifpe.spei_incidentes`: bitácora inmutable de resultados del proveedor
--     incompatibles con el estado final de una orden (p. ej. "liquidada" de
--     una orden ya reembolsada), que antes solo quedaban en el log.
--  4. `ifpe.v_spei_conciliacion`: cuenta también las órdenes LIQUIDADA en
--     revisión y agrega las órdenes con incidente.
--  5. Una devolución (de cualquier tipo de pago de devolución, o con clave de
--     rastreo de devolución) nunca se registra como abono nuevo.
--  6. Notificaciones retenidas para revisión (posible duplicado, identidad
--     reutilizada) en `ifpe.spei_cuarentena`, con su identidad; y la
--     autorización del operador para abonar un pago distinto de un abono
--     previo parecido (`distinto_de_tx`).
--  7. Abonos anteriores a 0005: se registran en `ifpe.spei_inbound` con origen
--     LEGADO, para que una nueva notificación del mismo pago sea una
--     repetición y no un segundo abono (ni un 409).
--  8. `ifpe.spei_anulaciones`: el operador anula una captura manual errónea
--     (corrección total) y libera su identidad: el pago real se registra como
--     reemplazo (`reemplaza_a`) y puede abonarse a la cuenta correcta.
--  9. Registro con el proveedor: `registro_intentado_en` (inicio de la última
--     llamada a `registrar`, para consultar todas las fechas de operación
--     posibles) y `registro_incierto_desde` (inicio de la primera llamada que
--     pudo registrar la orden: base de la regla del corte).
-- 10. Devoluciones de orden ligadas a su orden (CLABE ordenante, cuenta,
--     monto); la del operador solo sobre una orden ya liquidada y nunca de
--     sus propias órdenes.
-- 11. `ifpe.spei_descartes`: el operador descarta como duplicado una
--     notificación retenida y la saca de la cola de revisión.
--
--  La reserva del 100%, la sombra del ledger, la lista blanca de 0005 (todo
--  DEPOSITO es un SPEI registrado o el reverso de una orden; todo RETIRO una
--  orden o una corrección) y el RLS de 0006 y 0007 no cambian: ningún camino nuevo
--  mueve dinero fuera de `spei:`, `spei-reverso:` y `spei-correccion:`.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. intentos_envio monótono
-- -----------------------------------------------------------------------------
CREATE FUNCTION ifpe.fn_spei_outbound_intentos_monotonos() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.intentos_envio < OLD.intentos_envio THEN
        RAISE EXCEPTION 'SC_SPEI_INMUTABLE: intentos_envio de la orden % no puede disminuir', OLD.clave_rastreo;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_spei_outbound_intentos BEFORE UPDATE OF intentos_envio ON ifpe.spei_outbound
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_spei_outbound_intentos_monotonos();

-- -----------------------------------------------------------------------------
-- 2. Resolución del operador de una orden en revisión manual
-- -----------------------------------------------------------------------------
CREATE TABLE ifpe.spei_resoluciones (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    spei_outbound_id   UUID NOT NULL UNIQUE REFERENCES ifpe.spei_outbound (id) ON DELETE RESTRICT,
    accion             TEXT NOT NULL CHECK (accion IN ('REEMBOLSO')),
    motivo             TEXT NOT NULL CHECK (length(btrim(motivo)) BETWEEN 10 AND 300),
    -- ultimo_error de la orden al resolverla: la causa original de la revisión.
    error_previo       TEXT NOT NULL,
    autorizado_por     UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    clave_idempotencia TEXT NOT NULL UNIQUE,
    creado_en          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TRIGGER trg_spei_resoluciones_inmutable BEFORE UPDATE OR DELETE ON ifpe.spei_resoluciones
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_spei_resoluciones_no_truncate BEFORE TRUNCATE ON ifpe.spei_resoluciones
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- Una resolución es el reembolso, en esta misma transacción, de una orden
-- marcada para revisión que el proveedor nunca aceptó.
CREATE FUNCTION ifpe.fn_verificar_spei_resolucion() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    o      ifpe.spei_outbound;
    o_xmin xid;
    r_xmin xid;
    v_xmin xid;
BEGIN
    SELECT * INTO o FROM ifpe.spei_outbound WHERE id = NEW.spei_outbound_id;
    SELECT x.xmin INTO o_xmin FROM ifpe.spei_outbound x WHERE x.id = NEW.spei_outbound_id;
    SELECT x.xmin INTO r_xmin FROM ifpe.spei_resoluciones x WHERE x.id = NEW.id;
    SELECT x.xmin INTO v_xmin FROM ifpe.transacciones x WHERE x.id = o.tx_reverso_id;
    IF o.id IS NULL OR o.estado <> 'RECHAZADA' OR o.tx_reverso_id IS NULL
       OR o.enviada_en IS NOT NULL OR o.id_proveedor IS NOT NULL OR o.fecha_operacion IS NOT NULL
       OR o.ultimo_error IS NULL OR o.ultimo_error NOT LIKE 'REVISION_MANUAL:%'
       OR NEW.error_previo IS DISTINCT FROM o.ultimo_error
       OR o_xmin IS DISTINCT FROM r_xmin OR v_xmin IS DISTINCT FROM r_xmin THEN
        RAISE EXCEPTION 'SC_SPEI_DESCUADRE: la resolución de la orden % no corresponde a un reembolso de revisión manual en esta transacción',
            coalesce(o.clave_rastreo, NEW.spei_outbound_id::text);
    END IF;
    IF NEW.autorizado_por = o.cliente_id THEN
        RAISE EXCEPTION 'SC_SPEI_AUTORESOLUCION: un operador no resuelve sus propias órdenes';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_spei_resoluciones_ledger
    AFTER INSERT ON ifpe.spei_resoluciones
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_verificar_spei_resolucion();

-- -----------------------------------------------------------------------------
-- 3. Incidentes de conciliación
-- -----------------------------------------------------------------------------
CREATE TABLE ifpe.spei_incidentes (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    spei_outbound_id UUID NOT NULL REFERENCES ifpe.spei_outbound (id) ON DELETE RESTRICT,
    -- Estado de la orden cuando el proveedor informó lo incompatible: final,
    -- o ENVIADA/LIQUIDADA con un segundo registro aceptado (otro id u otra
    -- fecha de operación: posible pago doble).
    estado_orden     TEXT NOT NULL CHECK (estado_orden IN ('ENVIADA', 'LIQUIDADA', 'RECHAZADA', 'DEVUELTA')),
    evento           TEXT NOT NULL CHECK (evento IN ('ACEPTADA', 'LIQUIDADA', 'RECHAZADA', 'DEVUELTA', 'EN_PROCESO')),
    detalle          TEXT NOT NULL CHECK (length(detalle) BETWEEN 1 AND 500),
    registrado_en    TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Los reintentos del proveedor no duplican la evidencia.
    CONSTRAINT ux_spei_incidentes UNIQUE (spei_outbound_id, estado_orden, evento)
);
CREATE TRIGGER trg_spei_incidentes_inmutable BEFORE UPDATE OR DELETE ON ifpe.spei_incidentes
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_spei_incidentes_no_truncate BEFORE TRUNCATE ON ifpe.spei_incidentes
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- -----------------------------------------------------------------------------
-- 4. Conciliación con el custodio
-- -----------------------------------------------------------------------------
-- Mismas columnas, mismo orden y tipos que 0005 (CREATE OR REPLACE conserva
-- dueño y permisos), más una columna al final. El JOIN incluye ahora las
-- órdenes LIQUIDADA marcadas para revisión: ordenes_en_revision las cuenta;
-- las columnas de dinero ya filtran por estado y no cambian.
CREATE OR REPLACE VIEW ifpe.v_spei_conciliacion AS
SELECT r.id AS reserva_id,
       r.institucion,
       r.tipo,
       r.saldo AS reserva_contable,
       coalesce(sum(o.monto) FILTER (WHERE o.estado = 'PENDIENTE'), 0) AS pendiente_de_envio,
       coalesce(sum(o.monto) FILTER (WHERE o.estado = 'ENVIADA'), 0)   AS enviado_sin_confirmar,
       count(o.id) FILTER (WHERE o.estado IN ('PENDIENTE', 'ENVIADA'))  AS ordenes_en_transito,
       count(o.id) FILTER (WHERE o.revision_manual)                     AS ordenes_en_revision,
       r.saldo + coalesce(sum(o.monto) FILTER (WHERE o.estado IN ('PENDIENTE', 'ENVIADA')), 0)
                                                                        AS saldo_esperado_custodio,
       -- Órdenes de esta reserva con un incidente registrado (evidencia
       -- permanente: el custodio pudo pagar algo que los libros reembolsaron).
       (SELECT count(DISTINCT i.spei_outbound_id)
          FROM ifpe.spei_incidentes i
          JOIN ifpe.spei_outbound oi ON oi.id = i.spei_outbound_id
          JOIN ifpe.transacciones ti ON ti.id = oi.tx_retiro_id
         WHERE ti.reserva_id = r.id)                                    AS incidentes_conciliacion
FROM ifpe.reserva_custodia r
LEFT JOIN ifpe.transacciones t ON t.reserva_id = r.id AND t.tipo = 'RETIRO'
LEFT JOIN ifpe.spei_outbound o ON o.tx_retiro_id = t.id
                              AND (o.estado IN ('PENDIENTE', 'ENVIADA') OR o.revision_manual)
GROUP BY r.id, r.institucion, r.tipo, r.saldo;

-- -----------------------------------------------------------------------------
-- 5. Una devolución nunca se abona como depósito nuevo
-- -----------------------------------------------------------------------------
-- Tipos de pago SPEI de devolución (catálogo Banxico): 0 no acreditada, 16
-- extemporánea no acreditada, 17 acreditada, 18 extemporánea acreditada, 23
-- especial acreditada, 24 extemporánea especial acreditada. Una notificación
-- con clave de rastreo de devolución bien formada también es una devolución.
-- Lo que aquí cuenta como devolución es un subconjunto de lo que el backend
-- trata como tal (`Abono::es_devolucion`), así que un abono que el backend
-- registra nunca choca con esta regla. NOT VALID: se exige a toda fila nueva.
ALTER TABLE ifpe.spei_inbound
    ADD CONSTRAINT ck_spei_inbound_abonado_no_es_devolucion CHECK (
        estado <> 'ABONADO' OR (
            (tipo_pago IS NULL OR tipo_pago NOT IN (0, 16, 17, 18, 23, 24))
            AND coalesce(btrim(raw_payload ->> 'claveRastreoDevolucion'), '') !~ '^[A-Za-z0-9]{1,30}$'
        )
    ) NOT VALID;

-- Avisa si el defecto ya ocurrió (spei_inbound es inmutable: se corrige con
-- ifpe.spei_correcciones, no editando la fila).
DO $$
DECLARE
    n bigint;
BEGIN
    SELECT count(*) INTO n FROM ifpe.spei_inbound
     WHERE estado = 'ABONADO'
       AND (tipo_pago IN (0, 16, 17, 18, 23, 24)
            OR btrim(raw_payload ->> 'claveRastreoDevolucion') ~ '^[A-Za-z0-9]{1,30}$');
    IF n > 0 THEN
        RAISE WARNING 'SC_SPEI_REVISION: % SPEI recibidos ABONADO son devoluciones; conciliar contra ifpe.spei_outbound', n;
    END IF;
END
$$;

-- -----------------------------------------------------------------------------
-- 6. Notificaciones retenidas y pagos distintos autorizados por el operador
-- -----------------------------------------------------------------------------
-- POSIBLE_DUPLICADO / IDENTIDAD_REUTILIZADA: notificación autenticada que se
-- confirmó al proveedor y se retuvo sin abonar (el dinero queda en el
-- custodio, de más) hasta que el operador la resuelva. Una fila por cuerpo e
-- identidad: los reintentos no se acumulan.
ALTER TABLE ifpe.spei_cuarentena DROP CONSTRAINT spei_cuarentena_tipo_check;
ALTER TABLE ifpe.spei_cuarentena
    ADD CONSTRAINT spei_cuarentena_tipo_check
        CHECK (tipo IN ('ABONO', 'ESTADO', 'POSIBLE_DUPLICADO', 'IDENTIDAD_REUTILIZADA')),
    ADD COLUMN identidad TEXT CHECK (identidad ~ '^[0-9]{5}:[0-9]{8}:[A-Z0-9]{1,30}$'),
    ADD CONSTRAINT ck_spei_cuarentena_identidad
        CHECK ((tipo IN ('POSIBLE_DUPLICADO', 'IDENTIDAD_REUTILIZADA')) = (identidad IS NOT NULL));
CREATE UNIQUE INDEX ux_spei_cuarentena_retenida
    ON ifpe.spei_cuarentena (tipo, identidad, md5(cuerpo)) WHERE identidad IS NOT NULL;
CREATE TRIGGER trg_spei_cuarentena_no_truncate BEFORE TRUNCATE ON ifpe.spei_cuarentena
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- El operador confirma que un pago es distinto de un abono previo parecido
-- (misma cuenta, monto y clave de rastreo): auditado e inmutable.
ALTER TABLE ifpe.spei_inbound
    ADD COLUMN distinto_de_tx UUID REFERENCES ifpe.transacciones (id) ON DELETE RESTRICT,
    ADD CONSTRAINT ck_spei_inbound_distinto CHECK (
        distinto_de_tx IS NULL OR (
            origen = 'OPERADOR' AND estado = 'ABONADO'
            AND distinto_de_tx IS DISTINCT FROM transaccion_id
            AND motivo IS NOT NULL AND length(btrim(motivo)) BETWEEN 5 AND 200));

CREATE FUNCTION ifpe.fn_verificar_spei_distinto() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    p       ifpe.transacciones;
    rastreo TEXT;
BEGIN
    SELECT * INTO p FROM ifpe.transacciones WHERE id = NEW.distinto_de_tx;
    -- Referencia canónica (<emisor>:<AAAAMMDD>:<RASTREO>[/<reemplazo>]) o,
    -- en abonos anteriores a 0005, la clave de rastreo sola.
    rastreo := CASE WHEN p.referencia_externa ~ '^[0-9]{3,5}:[0-9]{8}:'
                    THEN upper(split_part(split_part(p.referencia_externa, '/', 1), ':', 3))
                    ELSE upper(btrim(p.referencia_externa)) END;
    IF p.id IS NULL OR p.tipo <> 'DEPOSITO' OR p.clave_idempotencia LIKE 'spei-reverso:%'
       OR p.cuenta_destino_id IS DISTINCT FROM NEW.cuenta_id OR p.monto <> NEW.monto
       OR rastreo IS DISTINCT FROM NEW.clave_rastreo THEN
        RAISE EXCEPTION 'SC_SPEI_DISTINTO_INVALIDO: el SPEI % solo puede declararse distinto de un abono de la misma cuenta, monto y clave de rastreo',
            ifpe.identidad_spei(NEW.institucion_emisora, NEW.fecha_operacion, NEW.clave_rastreo);
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_spei_inbound_distinto AFTER INSERT ON ifpe.spei_inbound
    FOR EACH ROW WHEN (NEW.distinto_de_tx IS NOT NULL) EXECUTE FUNCTION ifpe.fn_verificar_spei_distinto();

-- -----------------------------------------------------------------------------
-- 7. Anulación de una captura manual errónea y reemplazo de su identidad
-- -----------------------------------------------------------------------------
CREATE TABLE ifpe.spei_anulaciones (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    spei_inbound_id    UUID NOT NULL UNIQUE REFERENCES ifpe.spei_inbound (id) ON DELETE RESTRICT,
    -- Corrección que completó la reversión en esta misma transacción (NULL si
    -- el abono ya estaba corregido por completo).
    correccion_id      UUID UNIQUE REFERENCES ifpe.spei_correcciones (id) ON DELETE RESTRICT,
    motivo             TEXT NOT NULL CHECK (length(btrim(motivo)) BETWEEN 5 AND 200),
    autorizado_por     UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    clave_idempotencia TEXT NOT NULL UNIQUE,
    creado_en          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TRIGGER trg_spei_anulaciones_inmutable BEFORE UPDATE OR DELETE ON ifpe.spei_anulaciones
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_spei_anulaciones_no_truncate BEFORE TRUNCATE ON ifpe.spei_anulaciones
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- Solo se anula una captura (del operador o anterior a 0005, nunca lo que
-- informó el proveedor) totalmente corregida al COMMIT, por alguien que no es
-- el titular de la cuenta.
CREATE FUNCTION ifpe.fn_verificar_spei_anulacion() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    abono  ifpe.spei_inbound;
    c      ifpe.spei_correcciones;
    c_xmin xid;
    a_xmin xid;
    total  NUMERIC;
BEGIN
    SELECT * INTO abono FROM ifpe.spei_inbound WHERE id = NEW.spei_inbound_id;
    IF abono.id IS NULL OR abono.estado <> 'ABONADO' OR abono.origen NOT IN ('OPERADOR', 'LEGADO') THEN
        RAISE EXCEPTION 'SC_SPEI_NO_ANULABLE: solo se anula un abono capturado por el operador (o anterior a 0005)';
    END IF;
    SELECT coalesce(sum(monto), 0) INTO total FROM ifpe.spei_correcciones WHERE spei_inbound_id = abono.id;
    IF total <> abono.monto THEN
        RAISE EXCEPTION 'SC_SPEI_DESCUADRE: la anulación del SPEI % exige corregir el abono completo (% de %)',
            abono.id, total, abono.monto;
    END IF;
    IF NEW.correccion_id IS NOT NULL THEN
        SELECT * INTO c FROM ifpe.spei_correcciones WHERE id = NEW.correccion_id;
        SELECT x.xmin INTO c_xmin FROM ifpe.spei_correcciones x WHERE x.id = NEW.correccion_id;
        SELECT x.xmin INTO a_xmin FROM ifpe.spei_anulaciones x WHERE x.id = NEW.id;
        IF c.spei_inbound_id IS DISTINCT FROM abono.id OR c_xmin IS DISTINCT FROM a_xmin THEN
            RAISE EXCEPTION 'SC_SPEI_DESCUADRE: la corrección de la anulación no es de este abono ni de esta transacción';
        END IF;
    END IF;
    IF EXISTS (SELECT 1 FROM ifpe.cuentas WHERE id = abono.cuenta_id AND cliente_id = NEW.autorizado_por) THEN
        RAISE EXCEPTION 'SC_SPEI_AUTORESOLUCION: un operador no anula abonos a su propia cuenta';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_spei_anulaciones_ledger
    AFTER INSERT ON ifpe.spei_anulaciones
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_verificar_spei_anulacion();

-- El registro que reemplaza a uno anulado lleva su id. La identidad del pago
-- sigue siendo única entre los registros originales, cada anulado tiene a lo
-- sumo un reemplazo y solo se reemplaza un registro anulado de la misma
-- identidad: por identidad hay a lo sumo un registro vigente.
ALTER TABLE ifpe.spei_inbound
    ADD COLUMN reemplaza_a UUID UNIQUE REFERENCES ifpe.spei_inbound (id) ON DELETE RESTRICT;
ALTER TABLE ifpe.spei_inbound DROP CONSTRAINT ux_spei_inbound_identidad;
CREATE UNIQUE INDEX ux_spei_inbound_identidad
    ON ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion) WHERE reemplaza_a IS NULL;

CREATE FUNCTION ifpe.fn_verificar_spei_reemplazo() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    p ifpe.spei_inbound;
BEGIN
    SELECT * INTO p FROM ifpe.spei_inbound WHERE id = NEW.reemplaza_a;
    IF p.id IS NULL
       OR (p.institucion_emisora, p.clave_rastreo, p.fecha_operacion)
          IS DISTINCT FROM (NEW.institucion_emisora, NEW.clave_rastreo, NEW.fecha_operacion)
       OR NOT EXISTS (SELECT 1 FROM ifpe.spei_anulaciones a WHERE a.spei_inbound_id = p.id) THEN
        RAISE EXCEPTION 'SC_SPEI_DESCUADRE: el SPEI % solo puede reemplazar un registro anulado de la misma identidad',
            ifpe.identidad_spei(NEW.institucion_emisora, NEW.fecha_operacion, NEW.clave_rastreo);
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_spei_inbound_reemplazo BEFORE INSERT ON ifpe.spei_inbound
    FOR EACH ROW WHEN (NEW.reemplaza_a IS NOT NULL) EXECUTE FUNCTION ifpe.fn_verificar_spei_reemplazo();

-- Referencia del DEPOSITO de un abono SPEI: la identidad canónica; la de un
-- reemplazo lleva además el id del registro anulado (el ledger ya tiene un
-- DEPOSITO con la identidad sola, el anulado, que no se modifica).
CREATE FUNCTION ifpe.referencia_abono_spei(p_emisor TEXT, p_fecha DATE, p_rastreo TEXT, p_reemplaza_a UUID)
RETURNS TEXT
LANGUAGE sql IMMUTABLE PARALLEL SAFE
AS $$ SELECT ifpe.identidad_spei(p_emisor, p_fecha, p_rastreo) || coalesce('/' || p_reemplaza_a::text, '') $$;

-- Igual que en 0005, con la referencia del reemplazo.
CREATE OR REPLACE FUNCTION ifpe.fn_verificar_spei_inbound() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    t ifpe.transacciones;
    t_xmin xid;
    s_xmin xid;
    identidad TEXT := ifpe.identidad_spei(NEW.institucion_emisora, NEW.fecha_operacion, NEW.clave_rastreo);
    referencia TEXT := ifpe.referencia_abono_spei(NEW.institucion_emisora, NEW.fecha_operacion, NEW.clave_rastreo,
                                                  NEW.reemplaza_a);
BEGIN
    IF NEW.estado <> 'ABONADO' THEN
        RETURN NULL;
    END IF;
    SELECT * INTO t FROM ifpe.transacciones WHERE id = NEW.transaccion_id;
    SELECT x.xmin INTO t_xmin FROM ifpe.transacciones x WHERE x.id = NEW.transaccion_id;
    SELECT x.xmin INTO s_xmin FROM ifpe.spei_inbound x WHERE x.id = NEW.id;
    IF t.id IS NULL OR t.tipo <> 'DEPOSITO' OR t.cuenta_destino_id <> NEW.cuenta_id OR t.monto <> NEW.monto
       OR t.referencia_externa IS DISTINCT FROM referencia OR t.clave_idempotencia <> 'spei:' || referencia
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

-- -----------------------------------------------------------------------------
-- 8. Abonos anteriores a 0005 (origen LEGADO)
-- -----------------------------------------------------------------------------
ALTER TABLE ifpe.spei_inbound DROP CONSTRAINT spei_inbound_origen_check;
ALTER TABLE ifpe.spei_inbound
    ADD CONSTRAINT spei_inbound_origen_check CHECK (origen IN ('WEBHOOK', 'OPERADOR', 'DEMO', 'LEGADO'));

-- Nadie registra un abono LEGADO: solo `ifpe.registrar_abonos_legados`.
CREATE FUNCTION ifpe.fn_spei_inbound_sin_legado() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION 'SC_SPEI_LEGADO: los abonos anteriores a 0005 solo los registra la migración';
END
$$;
CREATE TRIGGER trg_spei_inbound_sin_legado BEFORE INSERT ON ifpe.spei_inbound
    FOR EACH ROW WHEN (NEW.origen = 'LEGADO') EXECUTE FUNCTION ifpe.fn_spei_inbound_sin_legado();

-- Registra los DEPOSITO 'spei:' con identidad canónica de 5 dígitos que no
-- tienen registro (anteriores a 0005: desde 0005 la lista blanca exige el
-- registro en la misma transacción). Los de otro formato (clave de rastreo
-- sola, emisor de 3 dígitos, claves 'op:') no tienen identidad completa: el
-- backend los trata como posibles duplicados. Idempotente; solo el dueño del
-- esquema puede ejecutarla.
CREATE FUNCTION ifpe.registrar_abonos_legados() RETURNS integer
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    d     record;
    fecha DATE;
    n     integer := 0;
    filas integer;
BEGIN
    -- La liga con el ledger exige que el registro y su DEPOSITO sean de la
    -- misma transacción, cosa imposible para un abono anterior: se desactiva
    -- solo mientras se registran (sin eventos pendientes, el ALTER es seguro).
    ALTER TABLE ifpe.spei_inbound DISABLE TRIGGER trg_spei_inbound_ledger;
    ALTER TABLE ifpe.spei_inbound DISABLE TRIGGER trg_spei_inbound_sin_legado;
    FOR d IN
        SELECT t.id, t.cuenta_destino_id, t.monto, t.referencia_externa, t.clave_idempotencia, t.creado_en, c.clabe
          FROM ifpe.transacciones t
          JOIN ifpe.cuentas c ON c.id = t.cuenta_destino_id
         WHERE t.tipo = 'DEPOSITO' AND t.clave_idempotencia LIKE 'spei:%'
           AND t.referencia_externa ~ '^[0-9]{5}:[0-9]{8}:[A-Za-z0-9]{1,30}$'
           AND NOT EXISTS (SELECT 1 FROM ifpe.spei_inbound s WHERE s.transaccion_id = t.id)
         ORDER BY t.secuencia
    LOOP
        BEGIN
            fecha := to_date(split_part(d.referencia_externa, ':', 2), 'YYYYMMDD');
        EXCEPTION WHEN others THEN
            CONTINUE;
        END;
        IF to_char(fecha, 'YYYYMMDD') <> split_part(d.referencia_externa, ':', 2) THEN
            CONTINUE;
        END IF;
        INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id,
                                       monto, estado, transaccion_id, origen, raw_payload, recibido_en)
        VALUES (split_part(d.referencia_externa, ':', 1), upper(split_part(d.referencia_externa, ':', 3)), fecha,
                d.clabe, d.cuenta_destino_id, d.monto, 'ABONADO', d.id, 'LEGADO',
                jsonb_build_object('legado', true, 'referencia_externa', d.referencia_externa,
                                   'clave_idempotencia', d.clave_idempotencia),
                d.creado_en)
        ON CONFLICT DO NOTHING;
        GET DIAGNOSTICS filas = ROW_COUNT;
        n := n + filas;
    END LOOP;
    ALTER TABLE ifpe.spei_inbound ENABLE TRIGGER trg_spei_inbound_sin_legado;
    ALTER TABLE ifpe.spei_inbound ENABLE TRIGGER trg_spei_inbound_ledger;
    RETURN n;
END
$$;
REVOKE ALL ON FUNCTION ifpe.registrar_abonos_legados() FROM PUBLIC;

DO $$
DECLARE
    n integer := ifpe.registrar_abonos_legados();
BEGIN
    IF n > 0 THEN
        RAISE NOTICE 'SPEI: % abonos anteriores a 0005 registrados con origen LEGADO', n;
    END IF;
END
$$;

-- -----------------------------------------------------------------------------
-- 9. Cola de revisión
-- -----------------------------------------------------------------------------
-- El operador descarta una notificación retenida que confirmó como duplicado
-- de un pago ya abonado (p. ej. de un abono anterior a 0005 sin registro que
-- anular). No mueve dinero: el excedente queda en el custodio para conciliar.
CREATE TABLE ifpe.spei_descartes (
    id                 UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cuarentena_id      UUID NOT NULL UNIQUE REFERENCES ifpe.spei_cuarentena (id) ON DELETE RESTRICT,
    motivo             TEXT NOT NULL CHECK (length(btrim(motivo)) BETWEEN 5 AND 200),
    autorizado_por     UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    clave_idempotencia TEXT NOT NULL UNIQUE,
    creado_en          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TRIGGER trg_spei_descartes_inmutable BEFORE UPDATE OR DELETE ON ifpe.spei_descartes
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_spei_descartes_no_truncate BEFORE TRUNCATE ON ifpe.spei_descartes
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- Solo se descarta una notificación retenida (con identidad).
CREATE FUNCTION ifpe.fn_verificar_spei_descarte() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM ifpe.spei_cuarentena WHERE id = NEW.cuarentena_id AND identidad IS NOT NULL) THEN
        RAISE EXCEPTION 'SC_SPEI_NO_DESCARTABLE: solo se descarta una notificación retenida para revisión';
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_spei_descartes_retenida AFTER INSERT ON ifpe.spei_descartes
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_verificar_spei_descarte();

-- Notificaciones retenidas que aún no se resuelven con un registro vigente de
-- esa identidad posterior a la retención (captura del operador con
-- `distinto_de`, o reemplazo tras anular una captura errónea) ni con un
-- descarte del operador.
CREATE VIEW ifpe.v_spei_en_revision AS
SELECT q.id, q.tipo, q.identidad, q.error, q.recibido_en
FROM ifpe.spei_cuarentena q
WHERE q.identidad IS NOT NULL
  AND NOT EXISTS (SELECT 1 FROM ifpe.spei_descartes d WHERE d.cuarentena_id = q.id)
  AND NOT EXISTS (
      SELECT 1 FROM ifpe.spei_inbound s
       WHERE ifpe.identidad_spei(s.institucion_emisora, s.fecha_operacion, s.clave_rastreo) = q.identidad
         AND s.recibido_en >= q.recibido_en
         AND NOT EXISTS (SELECT 1 FROM ifpe.spei_anulaciones a WHERE a.spei_inbound_id = s.id));

-- -----------------------------------------------------------------------------
-- 10. Registro con el proveedor
-- -----------------------------------------------------------------------------
-- registro_intentado_en: inicio de la última llamada a `registrar` (se fija
-- antes de llamar, con el reclamo renovado). Toda fecha de operación que el
-- proveedor pudo asignar cae entre fecha_solicitud y esa fecha + 5 días: la
-- consulta las revisa todas.
-- registro_incierto_desde: inicio de la primera llamada a `registrar` que
-- pudo registrar la orden (se fija antes de llamar y solo se borra si esa
-- misma llamada prueba que no llegó al proveedor). La regla del corte
-- (no reenviar si el día de operación pudo cambiar) cuenta desde aquí.
ALTER TABLE ifpe.spei_outbound
    ADD COLUMN registro_intentado_en   TIMESTAMPTZ,
    ADD COLUMN registro_incierto_desde TIMESTAMPTZ,
    ADD CONSTRAINT ck_spei_outbound_registro CHECK (
        registro_incierto_desde IS NULL
        OR (registro_intentado_en IS NOT NULL AND registro_incierto_desde <= registro_intentado_en));

CREATE FUNCTION ifpe.fn_spei_outbound_registro_monotono() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF OLD.registro_intentado_en IS NOT NULL
       AND (NEW.registro_intentado_en IS NULL OR NEW.registro_intentado_en < OLD.registro_intentado_en) THEN
        RAISE EXCEPTION 'SC_SPEI_INMUTABLE: registro_intentado_en de la orden % no puede retroceder', OLD.clave_rastreo;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_spei_outbound_registro BEFORE UPDATE OF registro_intentado_en ON ifpe.spei_outbound
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_spei_outbound_registro_monotono();

-- Órdenes vivas que ya hablaron con el proveedor antes de 0008: conservador,
-- como la regla anterior (desde la creación; consulta hasta hoy + 5 días).
-- Las RECHAZADA/DEVUELTA no se tocan: ya no se registran ni se consultan, su
-- estado final no admite transición y su reverso está ligado a la versión de
-- la fila que lo creó (`trg_spei_outbound_ledger`).
UPDATE ifpe.spei_outbound
   SET registro_intentado_en = greatest(now(), creado_en), registro_incierto_desde = creado_en
 WHERE intentos_envio > 0 AND estado IN ('PENDIENTE', 'ENVIADA', 'LIQUIDADA');

-- -----------------------------------------------------------------------------
-- 11. Devoluciones de orden
-- -----------------------------------------------------------------------------
-- Un registro DEVOLUCION_DE_ORDEN corresponde a su orden: regresa a la CLABE
-- ordenante (la cuenta del cliente), por el monto enviado, y la orden quedó
-- reembolsada. Si lo capturó el operador: solo sobre una orden liquidada en
-- una transacción anterior (nunca PENDIENTE/ENVIADA, que el proveedor pudo
-- pagar después) y nunca de sus propias órdenes.
CREATE FUNCTION ifpe.fn_verificar_spei_devolucion_de_orden() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    o ifpe.spei_outbound;
    identidad TEXT := ifpe.identidad_spei(NEW.institucion_emisora, NEW.fecha_operacion, NEW.clave_rastreo);
BEGIN
    SELECT * INTO o FROM ifpe.spei_outbound WHERE id = NEW.spei_outbound_id;
    IF o.id IS NULL OR o.estado NOT IN ('DEVUELTA', 'RECHAZADA') OR NEW.cuenta_id IS DISTINCT FROM o.cuenta_id
       OR NEW.monto <> o.monto OR NEW.receptor_clabe IS DISTINCT FROM o.ordenante_clabe THEN
        RAISE EXCEPTION 'SC_SPEI_DESCUADRE: la devolución % no corresponde a la orden % (CLABE ordenante, cuenta o monto)',
            identidad, coalesce(o.clave_rastreo, NEW.spei_outbound_id::text);
    END IF;
    IF NEW.registrado_por IS NOT NULL THEN
        IF NEW.registrado_por = o.cliente_id THEN
            RAISE EXCEPTION 'SC_SPEI_AUTORESOLUCION: un operador no captura devoluciones de sus propias órdenes';
        END IF;
        IF o.estado <> 'DEVUELTA' OR o.liquidada_en IS NULL OR o.liquidada_en >= now() THEN
            RAISE EXCEPTION 'SC_SPEI_DESCUADRE: la devolución % capturada por el operador exige una orden ya liquidada',
                identidad;
        END IF;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_spei_inbound_devolucion_de_orden
    AFTER INSERT ON ifpe.spei_inbound
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW WHEN (NEW.estado = 'DEVOLUCION_DE_ORDEN')
    EXECUTE FUNCTION ifpe.fn_verificar_spei_devolucion_de_orden();

-- -----------------------------------------------------------------------------
-- 12. Permisos
-- -----------------------------------------------------------------------------
GRANT SELECT ON ifpe.spei_resoluciones, ifpe.spei_incidentes, ifpe.spei_anulaciones, ifpe.v_spei_en_revision,
                ifpe.spei_descartes
    TO sc_spei, sc_auditor;
GRANT UPDATE (registro_intentado_en, registro_incierto_desde) ON ifpe.spei_outbound TO sc_spei;
GRANT INSERT (cuarentena_id, motivo, autorizado_por, clave_idempotencia) ON ifpe.spei_descartes TO sc_spei;
GRANT INSERT (spei_outbound_id, accion, motivo, error_previo, autorizado_por, clave_idempotencia)
    ON ifpe.spei_resoluciones TO sc_spei;
GRANT INSERT (spei_outbound_id, estado_orden, evento, detalle) ON ifpe.spei_incidentes TO sc_spei;
GRANT INSERT (identidad) ON ifpe.spei_cuarentena TO sc_spei;
GRANT INSERT (distinto_de_tx, reemplaza_a) ON ifpe.spei_inbound TO sc_spei;
GRANT INSERT (spei_inbound_id, correccion_id, motivo, autorizado_por, clave_idempotencia)
    ON ifpe.spei_anulaciones TO sc_spei;
