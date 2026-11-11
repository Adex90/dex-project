-- =============================================================================
--  Pruebas de la migración 0015 (órdenes límite y stop). Corren en una
--  transacción que termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0015_ordenes_avanzadas.sql
--
--  Lo que la base de datos garantiza por sí sola: una compra pendiente
--  compromete su efectivo con un movimiento del ledger IFPE y lo libera al
--  cerrarse, sin cambiar el pasivo ni la reserva del 100 %; una venta
--  pendiente compromete títulos que una venta de mercado ya no puede vender;
--  al COMMIT, lo comprometido es exactamente la suma de las órdenes
--  pendientes; los términos de una orden no cambian y su estado solo avanza;
--  y la ejecución de una orden pendiente cumple su precio límite o su stop y
--  vende exactamente los títulos que comprometió. backend/tests/ordenes_avanzadas.rs
--  cubre el camino completo desde la API y el motor de disparo.
-- =============================================================================
\set ON_ERROR_STOP on
\set QUIET on
\o /dev/null
SET client_min_messages = notice;

BEGIN;

-- SET CONSTRAINTS ALL IMMEDIATE dispara los triggers diferidos (los que
-- revisan al COMMIT) dentro de esta transacción, que nunca confirma.
CREATE FUNCTION pg_temp.debe_fallar(prueba TEXT, sentencias TEXT[], patron TEXT) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    s TEXT;
BEGIN
    BEGIN
        FOREACH s IN ARRAY sentencias LOOP
            EXECUTE s;
        END LOOP;
        SET CONSTRAINTS ALL IMMEDIATE;
    EXCEPTION WHEN OTHERS THEN
        SET CONSTRAINTS ALL DEFERRED;
        IF strpos(SQLERRM, patron) = 0 THEN
            RAISE EXCEPTION 'FALLO [%]: error inesperado "%" (se esperaba "%")', prueba, SQLERRM, patron;
        END IF;
        RAISE NOTICE 'OK  rechazado  %', prueba;
        RETURN;
    END;
    RAISE EXCEPTION 'FALLO [%]: se esperaba "%" pero las sentencias se aceptaron', prueba, patron;
END
$$;

-- Como debe_fallar, pero revisa solo una restricción diferida (SET CONSTRAINTS
-- <nombre> IMMEDIATE): cuando las sentencias rompen varias reglas a la vez,
-- prueba que esta en particular las detecta. Las demás revisiones pendientes
-- se descartan con la subtransacción.
CREATE FUNCTION pg_temp.debe_fallar_en(prueba TEXT, sentencias TEXT[], restriccion TEXT, patron TEXT) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    s TEXT;
BEGIN
    BEGIN
        FOREACH s IN ARRAY sentencias LOOP
            EXECUTE s;
        END LOOP;
        EXECUTE format('SET CONSTRAINTS %s IMMEDIATE', restriccion);
        RAISE EXCEPTION 'SC_PRUEBA_ACEPTADA';
    EXCEPTION WHEN OTHERS THEN
        SET CONSTRAINTS ALL DEFERRED;
        IF SQLERRM = 'SC_PRUEBA_ACEPTADA' THEN
            RAISE EXCEPTION 'FALLO [%]: se esperaba "%" de % pero las sentencias se aceptaron', prueba, patron, restriccion;
        END IF;
        IF strpos(SQLERRM, patron) = 0 THEN
            RAISE EXCEPTION 'FALLO [%]: error inesperado "%" (se esperaba "%")', prueba, SQLERRM, patron;
        END IF;
        RAISE NOTICE 'OK  rechazado  %', prueba;
    END;
END
$$;

-- Sin bloque EXCEPTION: las sentencias corren en la transacción principal (las
-- revisiones de par exigen el mismo xmin que la transacción).
CREATE FUNCTION pg_temp.debe_pasar(prueba TEXT, sentencias TEXT[]) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    s TEXT;
BEGIN
    FOREACH s IN ARRAY sentencias LOOP
        EXECUTE s;
    END LOOP;
    SET CONSTRAINTS ALL IMMEDIATE;
    SET CONSTRAINTS ALL DEFERRED;
    RAISE NOTICE 'OK  aceptado   %', prueba;
END
$$;

CREATE FUNCTION pg_temp.exigir(prueba TEXT, condicion BOOLEAN) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
    IF condicion IS NOT TRUE THEN
        RAISE EXCEPTION 'FALLO [%]', prueba;
    END IF;
    RAISE NOTICE 'OK  estado     %', prueba;
END
$$;

-- Las sentencias de una orden de mercado, como las escribe el backend
-- (backend/src/mercado): PLD (solo compras), saldos, movimiento IFPE y orden.
-- La ejecución de una pendiente es una de estas con la clave 'ord:<id>:ejecucion'.
CREATE FUNCTION pg_temp.orden(lado TEXT, cliente TEXT, cuenta TEXT, orden_id TEXT, tx_id TEXT, clave TEXT,
                              ticker TEXT, solicitado TEXT, cantidad TEXT, precio TEXT, monto TEXT,
                              costo_liberado TEXT DEFAULT NULL) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT CASE lado
      WHEN 'COMPRA' THEN ARRAY[
        'SET LOCAL ROLE sc_mercado',
        format($q$SELECT ifpe.aml_evaluar(%L, 'SALIDA', 'COMPRA_MERCADO', %s, %L, %L, '{}'::jsonb)$q$,
               cliente, monto, tx_id, clave),
        format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - %s WHERE id = %L$q$, monto, cuenta),
        format($q$UPDATE ifpe.reserva_custodia SET saldo = saldo - %s WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
               monto),
        format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, concepto,
                                                   clave_idempotencia, ref_movimiento_espejo)
                  VALUES (%L, 'COMPRA_MERCADO', %L, 'e0000000-0000-0000-0000-00000000000e', %s, %L, %L, %L)$q$,
               tx_id, cuenta, monto, 'Compra de ' || ticker, clave, orden_id),
        format($q$INSERT INTO mercado.ordenes (id, cliente_id, cuenta_ifpe_id, ticker, lado, monto_solicitado, cantidad,
                                                precio, monto, fuente_precio, tx_ifpe_id, clave_idempotencia)
                  VALUES (%L, %L, %L, %L, 'COMPRA', %s, %s, %s, %s, 'SIMULADO', %L, %L)$q$,
               orden_id, cliente, cuenta, ticker, solicitado, cantidad, precio, monto, tx_id, clave),
        'RESET ROLE']
      ELSE ARRAY[
        'SET LOCAL ROLE sc_mercado',
        format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + %s WHERE id = %L$q$, monto, cuenta),
        format($q$UPDATE ifpe.reserva_custodia SET saldo = saldo + %s WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
               monto),
        format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, concepto,
                                                   clave_idempotencia, ref_movimiento_espejo)
                  VALUES (%L, 'VENTA_MERCADO', %L, 'e0000000-0000-0000-0000-00000000000e', %s, %L, %L, %L)$q$,
               tx_id, cuenta, monto, 'Venta de ' || ticker, clave, orden_id),
        format($q$INSERT INTO mercado.ordenes (id, cliente_id, cuenta_ifpe_id, ticker, lado, monto_solicitado, cantidad,
                                                precio, monto, costo_liberado, fuente_precio, tx_ifpe_id, clave_idempotencia)
                  VALUES (%L, %L, %L, %L, 'VENTA', %s, %s, %s, %s, %s, 'SIMULADO', %L, %L)$q$,
               orden_id, cliente, cuenta, ticker, solicitado, cantidad, precio, monto, coalesce(costo_liberado, 'NULL'),
               tx_id, clave),
        'RESET ROLE']
    END
$$;

-- Una compra pendiente, como la coloca el backend: la orden (que la base de
-- datos valida), el efectivo de disponible a comprometido y su COMPROMISO_ORDEN.
CREATE FUNCTION pg_temp.compra_pendiente(pend TEXT, tx_id TEXT, cliente TEXT, cuenta TEXT, ticker TEXT, tipo TEXT,
                                         monto TEXT, limite TEXT, stop TEXT, clave TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
        'SET LOCAL ROLE sc_mercado',
        format($q$INSERT INTO mercado.ordenes_pendientes (id, cliente_id, cuenta_ifpe_id, ticker, lado, tipo, monto,
                                                           precio_limite, precio_stop, efectivo_comprometido,
                                                           tx_compromiso_id, clave_idempotencia)
                  VALUES (%L, %L, %L, %L, 'COMPRA', %L, %s, %s, %s, %s, %L, %L)$q$,
               pend, cliente, cuenta, ticker, tipo, monto, coalesce(limite, 'NULL'), coalesce(stop, 'NULL'), monto,
               tx_id, clave),
        format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - %s,
                                          saldo_comprometido = saldo_comprometido + %s WHERE id = %L$q$,
               monto, monto, cuenta),
        format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, cuenta_destino_id, monto, concepto,
                                                   clave_idempotencia, ref_movimiento_espejo)
                  VALUES (%L, 'COMPROMISO_ORDEN', %L, %L, %s, %L, %L, %L)$q$,
               tx_id, cuenta, cuenta, monto, 'Orden de compra de ' || ticker, 'ord:' || pend || ':compromiso', pend),
        'RESET ROLE']
$$;

-- Una venta pendiente: solo la orden (sus títulos se comprometen por trigger).
CREATE FUNCTION pg_temp.venta_pendiente(pend TEXT, cliente TEXT, cuenta TEXT, ticker TEXT, tipo TEXT, monto TEXT,
                                        limite TEXT, stop TEXT, titulos TEXT, clave TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
        'SET LOCAL ROLE sc_mercado',
        format($q$INSERT INTO mercado.ordenes_pendientes (id, cliente_id, cuenta_ifpe_id, ticker, lado, tipo, monto,
                                                           precio_limite, precio_stop, titulos_comprometidos,
                                                           clave_idempotencia)
                  VALUES (%L, %L, %L, %L, 'VENTA', %L, %s, %s, %s, %s, %L)$q$,
               pend, cliente, cuenta, ticker, tipo, monto, coalesce(limite, 'NULL'), coalesce(stop, 'NULL'), titulos,
               clave),
        'RESET ROLE']
$$;

-- Cierra una compra pendiente (cancelada, rechazada o ejecutada): el estado,
-- y el efectivo de vuelta al disponible con su LIBERACION_ORDEN.
CREATE FUNCTION pg_temp.cerrar_compra(pend TEXT, tx_id TEXT, cuenta TEXT, monto TEXT, estado TEXT,
                                      motivo TEXT DEFAULT NULL, ejecutada TEXT DEFAULT NULL) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
        'SET LOCAL ROLE sc_mercado',
        format($q$UPDATE mercado.ordenes_pendientes SET estado = %L, motivo = %L, tx_liberacion_id = %L,
                                                         orden_ejecutada_id = %L WHERE id = %L$q$,
               estado, motivo, tx_id, ejecutada, pend),
        format($q$UPDATE ifpe.cuentas SET saldo_comprometido = saldo_comprometido - %s,
                                          saldo_disponible = saldo_disponible + %s WHERE id = %L$q$,
               monto, monto, cuenta),
        format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, cuenta_destino_id, monto, concepto,
                                                   clave_idempotencia, ref_movimiento_espejo)
                  VALUES (%L, 'LIBERACION_ORDEN', %L, %L, %s, 'Orden cerrada', %L, %L)$q$,
               tx_id, cuenta, cuenta, monto, 'ord:' || pend || ':liberacion', pend),
        'RESET ROLE']
$$;

-- Cierra una venta pendiente: el estado (sus títulos se liberan por trigger).
CREATE FUNCTION pg_temp.cerrar_venta(pend TEXT, estado TEXT, motivo TEXT DEFAULT NULL, ejecutada TEXT DEFAULT NULL)
RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
        'SET LOCAL ROLE sc_mercado',
        format($q$UPDATE mercado.ordenes_pendientes SET estado = %L, motivo = %L, orden_ejecutada_id = %L
                   WHERE id = %L$q$, estado, motivo, ejecutada, pend),
        'RESET ROLE']
$$;

-- Como dueño (fuera de los roles de servicio): el oráculo mueve el precio.
CREATE FUNCTION pg_temp.precio(activo TEXT, nuevo TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[format($q$UPDATE mercado.activos SET precio = %s WHERE ticker = %L$q$, nuevo, activo)]
$$;

-- (disponible, comprometido) de una cuenta, como texto para comparar.
CREATE FUNCTION pg_temp.saldos(cuenta UUID) RETURNS TEXT
LANGUAGE sql AS $$
    SELECT saldo_disponible::text || ' / ' || saldo_comprometido::text FROM ifpe.cuentas WHERE id = cuenta
$$;

-- (cantidad, comprometida, costo) de una posición, como texto para comparar.
CREATE FUNCTION pg_temp.posicion(cliente UUID, activo TEXT) RETURNS TEXT
LANGUAGE sql AS $$
    SELECT coalesce((SELECT cantidad::text || ' / ' || cantidad_comprometida::text || ' / ' || costo::text
                       FROM mercado.posiciones WHERE cliente_id = cliente AND ticker = activo), 'sin posición')
$$;

CREATE FUNCTION pg_temp.estado(pend UUID) RETURNS TEXT
LANGUAGE sql AS $$ SELECT estado FROM mercado.ordenes_pendientes WHERE id = pend $$;

-- -----------------------------------------------------------------------------
-- Datos: Ana y Beto verificados, Carla sin KYC; la reserva; 20 000 para Ana y
-- 100 para Beto; Ana compra 1 250 MXN de oro (0.7752 g) para tener qué vender.
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ana',   'Prueba', 'ana@example.mx',   'VERIFICADO', now(), 'BAJO'),
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Beto',  'Prueba', 'beto@example.mx',  'VERIFICADO', now(), 'BAJO');
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, nivel_riesgo_pld) VALUES
  ('c0000000-0000-0000-0000-00000000000c', 'FISICA', 'Carla', 'Prueba', 'carla@example.mx', 'BAJO');
INSERT INTO ifpe.reserva_custodia (id, institucion, tipo, numero_cuenta) VALUES
  ('e0000000-0000-0000-0000-00000000000e', 'Banco de México', 'BANXICO', 'SC-RES-001');
INSERT INTO ifpe.cuentas (id, cliente_id, clabe) VALUES
  ('1a000000-0000-0000-0000-00000000001a', 'a0000000-0000-0000-0000-00000000000a', '646180157000000017'),
  ('1b000000-0000-0000-0000-00000000001b', 'b0000000-0000-0000-0000-00000000000b', '646180157000000020'),
  ('1c000000-0000-0000-0000-00000000001c', 'c0000000-0000-0000-0000-00000000000c', '646180157000000033');

SELECT pg_temp.debe_pasar('depósitos SPEI: 20 000 para Ana y 100 para Beto, como sc_spei', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 20000 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 100 WHERE id = '1b000000-0000-0000-0000-00000000001b'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 20100 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
     VALUES ('7d000000-0000-0000-0000-000000000001', 'DEPOSITO', '1a000000-0000-0000-0000-00000000001a',
             'e0000000-0000-0000-0000-00000000000e', 20000, 'spei:40012:20261101:ORD0001', '40012:20261101:ORD0001'),
            ('7d000000-0000-0000-0000-000000000002', 'DEPOSITO', '1b000000-0000-0000-0000-00000000001b',
             'e0000000-0000-0000-0000-00000000000e', 100, 'spei:40012:20261101:ORD0002', '40012:20261101:ORD0002')$q$,
  $q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                    estado, transaccion_id, origen, raw_payload)
     VALUES ('40012', 'ORD0001', '2026-11-01', '646180157000000017', '1a000000-0000-0000-0000-00000000001a', 20000,
             'ABONADO', '7d000000-0000-0000-0000-000000000001', 'WEBHOOK', '{}'),
            ('40012', 'ORD0002', '2026-11-01', '646180157000000020', '1b000000-0000-0000-0000-00000000001b', 100,
             'ABONADO', '7d000000-0000-0000-0000-000000000002', 'WEBHOOK', '{}')$q$,
  'RESET ROLE'
]);

SELECT pg_temp.debe_pasar('Ana compra 1 250 MXN de AUG1 a mercado (0.7752 g por 1 249.93)', pg_temp.orden(
  'COMPRA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-000000000001', '7e000000-0000-0000-0000-000000000001',
  'cli:a0000000-0000-0000-0000-00000000000a:mercado-1', 'AUG1', '1250', '0.7752', '1612.40', '1249.93'));

-- -----------------------------------------------------------------------------
-- Estructura, privilegios y RLS
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('las cuentas y posiciones existentes nacen sin nada comprometido', (
  SELECT bool_and(saldo_comprometido = 0 AND version_ordenes = 0) FROM ifpe.cuentas)
  AND (SELECT bool_and(cantidad_comprometida = 0) FROM mercado.posiciones));

SELECT pg_temp.exigir('RLS en órdenes pendientes, con la política RESTRICTIVE de sc_cliente (0011)', (
  SELECT relrowsecurity FROM pg_class WHERE oid = 'mercado.ordenes_pendientes'::regclass)
  AND (SELECT count(*) = 1 FROM pg_policies
        WHERE schemaname = 'mercado' AND tablename = 'ordenes_pendientes' AND policyname = 'solo_del_cliente'
          AND permissive = 'RESTRICTIVE' AND cmd = 'ALL' AND roles = '{sc_cliente}'));

SELECT pg_temp.exigir('sc_cliente solo lee sus órdenes pendientes', (
  SELECT array_agg(privilege_type::text ORDER BY privilege_type) = ARRAY['SELECT']
    FROM information_schema.role_table_grants
   WHERE grantee = 'sc_cliente' AND table_schema = 'mercado' AND table_name = 'ordenes_pendientes'));

SELECT pg_temp.exigir('sc_mercado coloca órdenes y solo cierra las que esperan (estado, motivo y ligas)', (
  has_table_privilege('sc_mercado', 'mercado.ordenes_pendientes', 'SELECT')
  AND has_table_privilege('sc_mercado', 'mercado.ordenes_pendientes', 'INSERT')
  AND NOT has_table_privilege('sc_mercado', 'mercado.ordenes_pendientes', 'DELETE')
  AND has_column_privilege('sc_mercado', 'mercado.ordenes_pendientes', 'estado', 'UPDATE')
  AND has_column_privilege('sc_mercado', 'mercado.ordenes_pendientes', 'tx_liberacion_id', 'UPDATE')
  AND NOT has_column_privilege('sc_mercado', 'mercado.ordenes_pendientes', 'monto', 'UPDATE')
  AND NOT has_column_privilege('sc_mercado', 'mercado.ordenes_pendientes', 'precio_limite', 'UPDATE')
  AND NOT has_column_privilege('sc_mercado', 'mercado.ordenes_pendientes', 'titulos_comprometidos', 'UPDATE')));

SELECT pg_temp.exigir('sc_mercado compromete efectivo, pero no toca lo retenido por PLD', (
  has_column_privilege('sc_mercado', 'ifpe.cuentas', 'saldo_comprometido', 'UPDATE')
  AND NOT has_column_privilege('sc_mercado', 'ifpe.cuentas', 'saldo_bloqueado', 'UPDATE')
  AND NOT has_column_privilege('sc_mercado', 'ifpe.cuentas', 'version_ordenes', 'UPDATE')));

SELECT pg_temp.debe_fallar('sc_cliente no coloca órdenes pendientes', ARRAY[
  'SET LOCAL ROLE sc_cliente',
  $q$INSERT INTO mercado.ordenes_pendientes (id, cliente_id, cuenta_ifpe_id, ticker, lado, tipo, monto, precio_limite,
                                             titulos_comprometidos, clave_idempotencia)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
             'AUG1', 'VENTA', 'LIMITE', 100, 1700, 0.05, 'x')$q$
], 'permission denied');

-- -----------------------------------------------------------------------------
-- Una compra límite compromete su efectivo
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana: compra límite de 1 250 MXN de AUG1 a 1 600 o menos (el precio es 1 612.40)',
  pg_temp.compra_pendiente('f1000000-0000-0000-0000-000000000001', '7c000000-0000-0000-0000-000000000001',
                           'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a', 'AUG1',
                           'LIMITE', '1250', '1600', NULL, 'cli:a0000000-0000-0000-0000-00000000000a:limite-1'));

SELECT pg_temp.exigir('los 1 250 pasan de disponible a comprometido (18 750.07 -> 17 500.07 / 1 250)',
  pg_temp.saldos('1a000000-0000-0000-0000-00000000001a') = '17500.07000000 / 1250.00000000');
SELECT pg_temp.exigir('el saldo sombra lo reproduce desde el ledger', (
  SELECT disponible = 17500.07 AND comprometido = 1250 AND bloqueado = 0
    FROM ifpe.saldos_ledger WHERE cuenta_id = '1a000000-0000-0000-0000-00000000001a'));
SELECT pg_temp.exigir('el pasivo no cambia y la reserva sigue al 100 % (18 850.07 en custodia)', (
  SELECT pasivo_clientes = activo_reserva AND activo_reserva = 18850.07 FROM ifpe.estado_reserva));
SELECT pg_temp.exigir('la hoja Merkle de Ana cuenta lo comprometido (18 750.07)', (
  SELECT saldo_total = 18750.07 FROM ifpe.v_hojas_reserva WHERE cuenta_id = '1a000000-0000-0000-0000-00000000001a'));
SELECT pg_temp.exigir('el compromiso es un movimiento encadenado del ledger IFPE, dentro de la cuenta y sin reserva', (
  SELECT tipo = 'COMPROMISO_ORDEN' AND cuenta_origen_id = cuenta_destino_id AND reserva_id IS NULL
         AND ref_movimiento_espejo = 'f1000000-0000-0000-0000-000000000001'
         AND clave_idempotencia = 'ord:f1000000-0000-0000-0000-000000000001:compromiso'
         AND tx_hash = nucleo.sha256_hex(ifpe.payload_canonico(t))
    FROM ifpe.transacciones t WHERE id = '7c000000-0000-0000-0000-000000000001'));
SELECT pg_temp.exigir('la orden espera con sus términos', (
  SELECT estado = 'PENDIENTE' AND tipo = 'LIMITE' AND lado = 'COMPRA' AND precio_limite = 1600
         AND precio_stop IS NULL AND efectivo_comprometido = 1250 AND titulos_comprometidos = 0
         AND cerrado_en IS NULL AND motivo IS NULL AND orden_ejecutada_id IS NULL
    FROM mercado.ordenes_pendientes WHERE id = 'f1000000-0000-0000-0000-000000000001'));
SELECT pg_temp.exigir('colocarla sube la versión de órdenes de la bóveda (el WebSocket avisa al confirmar)', (
  SELECT version_ordenes = 1 FROM ifpe.cuentas WHERE id = '1a000000-0000-0000-0000-00000000001a'));
SELECT pg_temp.exigir('comprometer no es una salida: PLD no lo evalúa (sí la compra, al ejecutarse)', (
  SELECT count(*) = 0 FROM aml.evaluaciones WHERE transaccion_id = '7c000000-0000-0000-0000-000000000001'));

SELECT pg_temp.debe_pasar('Ana: otra compra límite, 500 MXN a 1 500 o menos',
  pg_temp.compra_pendiente('f1000000-0000-0000-0000-000000000002', '7c000000-0000-0000-0000-000000000002',
                           'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a', 'AUG1',
                           'LIMITE', '500', '1500', NULL, 'cli:a0000000-0000-0000-0000-00000000000a:limite-2'));
SELECT pg_temp.exigir('comprometido: la suma de sus compras pendientes (1 750)',
  pg_temp.saldos('1a000000-0000-0000-0000-00000000001a') = '17000.07000000 / 1750.00000000');

SELECT pg_temp.debe_fallar('no compromete más de lo disponible', pg_temp.compra_pendiente(
  'f1000000-0000-0000-0000-0000000000a1', '7c000000-0000-0000-0000-0000000000a1',
  'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a', 'AUG1', 'LIMITE', '17000.08',
  '1500', NULL, 'cli:a0000000-0000-0000-0000-00000000000a:sin-saldo'),
  'ck_ifpe_cuentas_saldo_disponible_no_negativo');

-- -----------------------------------------------------------------------------
-- Cada compromiso con su orden, y lo comprometido = las órdenes pendientes
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar_en('un COMPROMISO_ORDEN sin su orden pendiente', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100, saldo_comprometido = saldo_comprometido + 100
      WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia,
                                     ref_movimiento_espejo)
     VALUES ('7c000000-0000-0000-0000-0000000000a2', 'COMPROMISO_ORDEN', '1a000000-0000-0000-0000-00000000001a',
             '1a000000-0000-0000-0000-00000000001a', 100, 'ord:sin-orden:compromiso',
             'f1000000-0000-0000-0000-0000000000a2')$q$
], 'ifpe.trg_ifpe_tx_compromiso_ligado', 'SC_COMPROMISO_SIN_ORDEN');

SELECT pg_temp.debe_fallar_en('… ni con efectivo comprometido de más', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100, saldo_comprometido = saldo_comprometido + 100
      WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia,
                                     ref_movimiento_espejo)
     VALUES ('7c000000-0000-0000-0000-0000000000a3', 'COMPROMISO_ORDEN', '1a000000-0000-0000-0000-00000000001a',
             '1a000000-0000-0000-0000-00000000001a', 100, 'ord:sin-orden-2:compromiso',
             'f1000000-0000-0000-0000-0000000000a3')$q$
], 'ifpe.trg_ifpe_cuentas_comprometido_ordenes', 'SC_COMPROMISO_DESCUADRE');

SELECT pg_temp.debe_fallar_en('una compra pendiente sin su COMPROMISO_ORDEN', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$INSERT INTO mercado.ordenes_pendientes (id, cliente_id, cuenta_ifpe_id, ticker, lado, tipo, monto, precio_limite,
                                             efectivo_comprometido, tx_compromiso_id, clave_idempotencia)
     VALUES ('f1000000-0000-0000-0000-0000000000a4', 'a0000000-0000-0000-0000-00000000000a',
             '1a000000-0000-0000-0000-00000000001a', 'AUG1', 'COMPRA', 'LIMITE', 100, 1500, 100,
             '7c000000-0000-0000-0000-0000000000a4', 'cli:a0000000-0000-0000-0000-00000000000a:sin-compromiso')$q$
], 'mercado.trg_pendientes_compromiso_ligado', 'SC_ORDEN_SIN_COMPROMISO');

SELECT pg_temp.debe_fallar_en('… ni sin el efectivo comprometido en la cuenta', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$INSERT INTO mercado.ordenes_pendientes (id, cliente_id, cuenta_ifpe_id, ticker, lado, tipo, monto, precio_limite,
                                             efectivo_comprometido, tx_compromiso_id, clave_idempotencia)
     VALUES ('f1000000-0000-0000-0000-0000000000a5', 'a0000000-0000-0000-0000-00000000000a',
             '1a000000-0000-0000-0000-00000000001a', 'AUG1', 'COMPRA', 'LIMITE', 100, 1500, 100,
             '7c000000-0000-0000-0000-0000000000a5', 'cli:a0000000-0000-0000-0000-00000000000a:sin-efectivo')$q$
], 'mercado.trg_pendientes_efectivo_comprometido', 'SC_COMPROMISO_DESCUADRE');

SELECT pg_temp.debe_fallar_en('un compromiso por otro monto que el de su orden', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$INSERT INTO mercado.ordenes_pendientes (id, cliente_id, cuenta_ifpe_id, ticker, lado, tipo, monto, precio_limite,
                                             efectivo_comprometido, tx_compromiso_id, clave_idempotencia)
     VALUES ('f1000000-0000-0000-0000-0000000000a6', 'a0000000-0000-0000-0000-00000000000a',
             '1a000000-0000-0000-0000-00000000001a', 'AUG1', 'COMPRA', 'LIMITE', 200, 1500, 200,
             '7c000000-0000-0000-0000-0000000000a6', 'cli:a0000000-0000-0000-0000-00000000000a:otro-monto')$q$,
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100, saldo_comprometido = saldo_comprometido + 100
      WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia,
                                     ref_movimiento_espejo)
     VALUES ('7c000000-0000-0000-0000-0000000000a6', 'COMPROMISO_ORDEN', '1a000000-0000-0000-0000-00000000001a',
             '1a000000-0000-0000-0000-00000000001a', 100, 'ord:otro-monto:compromiso',
             'f1000000-0000-0000-0000-0000000000a6')$q$
], 'mercado.trg_pendientes_compromiso_ligado', 'SC_ORDEN_SIN_COMPROMISO');

SELECT pg_temp.debe_fallar_en('efectivo comprometido sin movimiento del ledger', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100, saldo_comprometido = saldo_comprometido + 100
      WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$
], 'ifpe.trg_ifpe_cuentas_comprometido_saldo_ledger', 'SC_SALDO_SIN_LEDGER');

SELECT pg_temp.debe_fallar_en('liberar el efectivo de una orden que sigue pendiente', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE ifpe.cuentas SET saldo_comprometido = saldo_comprometido - 500, saldo_disponible = saldo_disponible + 500
      WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia,
                                     ref_movimiento_espejo)
     VALUES ('7c000000-0000-0000-0000-0000000000a7', 'LIBERACION_ORDEN', '1a000000-0000-0000-0000-00000000001a',
             '1a000000-0000-0000-0000-00000000001a', 500, 'ord:f1000000-0000-0000-0000-000000000002:liberacion',
             'f1000000-0000-0000-0000-000000000002')$q$
], 'ifpe.trg_ifpe_tx_compromiso_ligado', 'SC_COMPROMISO_SIN_ORDEN');

SELECT pg_temp.debe_fallar_en('… ni el comprometido queda por debajo de sus órdenes', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE ifpe.cuentas SET saldo_comprometido = saldo_comprometido - 500, saldo_disponible = saldo_disponible + 500
      WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia,
                                     ref_movimiento_espejo)
     VALUES ('7c000000-0000-0000-0000-0000000000a8', 'LIBERACION_ORDEN', '1a000000-0000-0000-0000-00000000001a',
             '1a000000-0000-0000-0000-00000000001a', 500, 'ord:sigue-pendiente:liberacion',
             'f1000000-0000-0000-0000-000000000002')$q$
], 'ifpe.trg_ifpe_cuentas_comprometido_ordenes', 'SC_COMPROMISO_DESCUADRE');

SELECT pg_temp.debe_fallar('un compromiso entre dos cuentas no existe', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia,
                                     ref_movimiento_espejo)
     VALUES (gen_random_uuid(), 'COMPROMISO_ORDEN', '1a000000-0000-0000-0000-00000000001a',
             '1b000000-0000-0000-0000-00000000001b', 100, 'ord:dos-cuentas:compromiso', gen_random_uuid())$q$
], 'ck_ifpe_tx_forma');

SELECT pg_temp.debe_fallar('solo sc_mercado escribe compromisos', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia,
                                     ref_movimiento_espejo)
     VALUES (gen_random_uuid(), 'COMPROMISO_ORDEN', '1a000000-0000-0000-0000-00000000001a',
             '1a000000-0000-0000-0000-00000000001a', 100, 'ord:ifpe:compromiso', gen_random_uuid())$q$
], 'row-level security');

SELECT pg_temp.debe_fallar('una cuenta cerrada no guarda efectivo comprometido', ARRAY[
  $q$UPDATE ifpe.cuentas SET estado = 'CERRADA', saldo_disponible = 0 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$
], 'ck_ifpe_cuentas_cerrada_sin_saldo');

-- -----------------------------------------------------------------------------
-- Quién coloca y qué: KYC, cuenta activa, activo abierto, stop sin alcanzar
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('Carla, sin KYC, no coloca órdenes', pg_temp.compra_pendiente(
  'f1000000-0000-0000-0000-0000000000b1', '7c000000-0000-0000-0000-0000000000b1',
  'c0000000-0000-0000-0000-00000000000c', '1c000000-0000-0000-0000-00000000001c', 'AUG1', 'LIMITE', '10', '1500',
  NULL, 'cli:c0000000-0000-0000-0000-00000000000c:sin-kyc'),
  'row-level security');

SELECT pg_temp.debe_fallar('… ni compromete efectivo', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia,
                                     ref_movimiento_espejo)
     VALUES (gen_random_uuid(), 'COMPROMISO_ORDEN', '1c000000-0000-0000-0000-00000000001c',
             '1c000000-0000-0000-0000-00000000001c', 10, 'ord:sin-kyc:compromiso', gen_random_uuid())$q$
], 'row-level security');

SELECT pg_temp.debe_fallar('una orden con la cuenta de otro cliente', pg_temp.compra_pendiente(
  'f1000000-0000-0000-0000-0000000000b2', '7c000000-0000-0000-0000-0000000000b2',
  'a0000000-0000-0000-0000-00000000000a', '1b000000-0000-0000-0000-00000000001b', 'AUG1', 'LIMITE', '10', '1500',
  NULL, 'cli:a0000000-0000-0000-0000-00000000000a:otra-cuenta'),
  'SC_ORDEN_INCOHERENTE');

SELECT pg_temp.debe_fallar('una cuenta congelada no coloca órdenes', ARRAY[
  $q$UPDATE ifpe.cuentas SET estado = 'CONGELADA' WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$
] || pg_temp.compra_pendiente(
  'f1000000-0000-0000-0000-0000000000b3', '7c000000-0000-0000-0000-0000000000b3',
  'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a', 'AUG1', 'LIMITE', '10', '1500',
  NULL, 'cli:a0000000-0000-0000-0000-00000000000a:congelada'),
  'SC_CUENTA_CONGELADA');

SELECT pg_temp.debe_fallar('un activo suspendido no recibe órdenes', ARRAY[
  $q$UPDATE mercado.activos SET operable = FALSE WHERE ticker = 'AUG1'$q$
] || pg_temp.compra_pendiente(
  'f1000000-0000-0000-0000-0000000000b4', '7c000000-0000-0000-0000-0000000000b4',
  'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a', 'AUG1', 'LIMITE', '10', '1500',
  NULL, 'cli:a0000000-0000-0000-0000-00000000000a:suspendido'),
  'SC_MERCADO_CERRADO');

SELECT pg_temp.debe_fallar('un stop de compra ya alcanzado (1 600 con el precio en 1 612.40)', pg_temp.compra_pendiente(
  'f1000000-0000-0000-0000-0000000000b5', '7c000000-0000-0000-0000-0000000000b5',
  'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a', 'AUG1', 'STOP', '100', NULL,
  '1600', 'cli:a0000000-0000-0000-0000-00000000000a:stop-alcanzado'),
  'SC_PRECIO_STOP_INVALIDO');

SELECT pg_temp.debe_fallar('un stop de venta ya alcanzado (1 700 con el precio en 1 612.40)', pg_temp.venta_pendiente(
  'f1000000-0000-0000-0000-0000000000b6', 'a0000000-0000-0000-0000-00000000000a',
  '1a000000-0000-0000-0000-00000000001a', 'AUG1', 'STOP', '100', NULL, '1700', '0.0588',
  'cli:a0000000-0000-0000-0000-00000000000a:stop-venta-alcanzado'),
  'SC_PRECIO_STOP_INVALIDO');

SELECT pg_temp.debe_fallar('una orden límite con precio stop', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$INSERT INTO mercado.ordenes_pendientes (id, cliente_id, cuenta_ifpe_id, ticker, lado, tipo, monto, precio_limite,
                                             precio_stop, titulos_comprometidos, clave_idempotencia)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
             'AUG1', 'VENTA', 'LIMITE', 100, 1700, 1500, 0.0588, 'cli:a0000000-0000-0000-0000-00000000000a:dos-precios')$q$
], 'ck_pendiente_precio');

SELECT pg_temp.debe_fallar('una orden stop sin precio stop', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$INSERT INTO mercado.ordenes_pendientes (id, cliente_id, cuenta_ifpe_id, ticker, lado, tipo, monto,
                                             titulos_comprometidos, clave_idempotencia)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
             'AUG1', 'VENTA', 'STOP', 100, 0.0588, 'cli:a0000000-0000-0000-0000-00000000000a:sin-stop')$q$
], 'ck_pendiente_precio');

SELECT pg_temp.debe_fallar('un monto con fracciones de centavo', pg_temp.compra_pendiente(
  'f1000000-0000-0000-0000-0000000000b7', '7c000000-0000-0000-0000-0000000000b7',
  'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a', 'AUG1', 'LIMITE', '10.005', '1500',
  NULL, 'cli:a0000000-0000-0000-0000-00000000000a:centavos'),
  'ordenes_pendientes_monto_check');

SELECT pg_temp.debe_fallar('una orden pendiente nace pendiente', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$INSERT INTO mercado.ordenes_pendientes (id, cliente_id, cuenta_ifpe_id, ticker, lado, tipo, monto, precio_limite,
                                             titulos_comprometidos, estado, cerrado_en, clave_idempotencia)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
             'AUG1', 'VENTA', 'LIMITE', 100, 1700, 0.0588, 'CANCELADA', now(),
             'cli:a0000000-0000-0000-0000-00000000000a:nace-cerrada')$q$
], 'SC_ORDEN_INCOHERENTE');

-- -----------------------------------------------------------------------------
-- Términos inmutables; el estado solo avanza
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('sc_mercado no cambia el monto de una orden', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE mercado.ordenes_pendientes SET monto = 1 WHERE id = 'f1000000-0000-0000-0000-000000000001'$q$
], 'permission denied');

SELECT pg_temp.debe_fallar('los términos no cambian al cerrar (ni el dueño)', ARRAY[
  $q$UPDATE mercado.ordenes_pendientes SET estado = 'CANCELADA', precio_limite = 1700
      WHERE id = 'f1000000-0000-0000-0000-000000000001'$q$
], 'SC_ORDEN_INCOHERENTE');

SELECT pg_temp.debe_fallar('una orden pendiente solo cambia para cerrarse', ARRAY[
  $q$UPDATE mercado.ordenes_pendientes SET motivo = 'X' WHERE id = 'f1000000-0000-0000-0000-000000000001'$q$
], 'SC_ORDEN_INCOHERENTE');

SELECT pg_temp.debe_fallar('ejecutada sin la orden que la ejecutó', pg_temp.cerrar_compra(
  'f1000000-0000-0000-0000-000000000002', '7c000000-0000-0000-0000-0000000000c1',
  '1a000000-0000-0000-0000-00000000001a', '500', 'EJECUTADA'),
  'ck_pendiente_cierre');

SELECT pg_temp.debe_fallar('rechazada sin motivo', pg_temp.cerrar_compra(
  'f1000000-0000-0000-0000-000000000002', '7c000000-0000-0000-0000-0000000000c2',
  '1a000000-0000-0000-0000-00000000001a', '500', 'RECHAZADA'),
  'ck_pendiente_cierre');

SELECT pg_temp.debe_fallar('el motivo es un código (MAYÚSCULAS_Y_GUIONES)', pg_temp.cerrar_compra(
  'f1000000-0000-0000-0000-000000000002', '7c000000-0000-0000-0000-0000000000c3',
  '1a000000-0000-0000-0000-00000000001a', '500', 'RECHAZADA', 'sin fondos'),
  'ordenes_pendientes_motivo_check');

SELECT pg_temp.debe_fallar('una orden pendiente no se borra', ARRAY[
  $q$DELETE FROM mercado.ordenes_pendientes WHERE id = 'f1000000-0000-0000-0000-000000000001'$q$
], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('… ni se trunca', ARRAY[
  'TRUNCATE mercado.ordenes_pendientes'
], 'SC_LEDGER_INMUTABLE');

-- -----------------------------------------------------------------------------
-- Cancelar una compra devuelve el efectivo
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana cancela la compra de 500: CANCELADA y LIBERACION_ORDEN', pg_temp.cerrar_compra(
  'f1000000-0000-0000-0000-000000000002', '7c000000-0000-0000-0000-000000000003',
  '1a000000-0000-0000-0000-00000000001a', '500', 'CANCELADA'));

SELECT pg_temp.exigir('los 500 regresan al disponible',
  pg_temp.saldos('1a000000-0000-0000-0000-00000000001a') = '17500.07000000 / 1250.00000000');
SELECT pg_temp.exigir('la orden queda cancelada y cerrada, ligada a su liberación', (
  SELECT estado = 'CANCELADA' AND cerrado_en IS NOT NULL AND tx_liberacion_id = '7c000000-0000-0000-0000-000000000003'
    FROM mercado.ordenes_pendientes WHERE id = 'f1000000-0000-0000-0000-000000000002'));
SELECT pg_temp.exigir('tres cambios de órdenes en la bóveda: dos altas y una cancelación', (
  SELECT version_ordenes = 3 FROM ifpe.cuentas WHERE id = '1a000000-0000-0000-0000-00000000001a'));

SELECT pg_temp.debe_fallar('una orden cerrada no se vuelve a cerrar', pg_temp.cerrar_compra(
  'f1000000-0000-0000-0000-000000000002', '7c000000-0000-0000-0000-0000000000c4',
  '1a000000-0000-0000-0000-00000000001a', '500', 'CANCELADA'),
  'SC_ORDEN_NO_PENDIENTE');

-- -----------------------------------------------------------------------------
-- Una venta límite compromete títulos
-- -----------------------------------------------------------------------------
-- 500 MXN a 1 700 son 0.2941 g (0.2942 costarían 500.14).
SELECT pg_temp.debe_pasar('Ana: venta límite de 500 MXN de AUG1 a 1 700 o más (0.2941 g)', pg_temp.venta_pendiente(
  'f1000000-0000-0000-0000-000000000003', 'a0000000-0000-0000-0000-00000000000a',
  '1a000000-0000-0000-0000-00000000001a', 'AUG1', 'LIMITE', '500', '1700', NULL, '0.2941',
  'cli:a0000000-0000-0000-0000-00000000000a:venta-limite'));

SELECT pg_temp.exigir('0.2941 de sus 0.7752 g quedan comprometidos',
  pg_temp.posicion('a0000000-0000-0000-0000-00000000000a', 'AUG1') = '0.77520000 / 0.29410000 / 1249.93000000');
SELECT pg_temp.exigir('una venta pendiente no mueve efectivo ni escribe en el ledger IFPE',
  pg_temp.saldos('1a000000-0000-0000-0000-00000000001a') = '17500.07000000 / 1250.00000000'
  AND (SELECT count(*) = 0 FROM ifpe.transacciones WHERE ref_movimiento_espejo = 'f1000000-0000-0000-0000-000000000003'));

SELECT pg_temp.debe_pasar('Ana: stop de compra, 100 MXN cuando AUG1 llegue a 1 650',
  pg_temp.compra_pendiente('f1000000-0000-0000-0000-000000000004', '7c000000-0000-0000-0000-000000000004',
                           'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a', 'AUG1',
                           'STOP', '100', NULL, '1650', 'cli:a0000000-0000-0000-0000-00000000000a:stop-compra'));

-- 200 MXN a 1 550 son 0.1290 g.
SELECT pg_temp.debe_pasar('Ana: stop de venta, 200 MXN si AUG1 baja a 1 550 (0.1290 g)', pg_temp.venta_pendiente(
  'f1000000-0000-0000-0000-000000000005', 'a0000000-0000-0000-0000-00000000000a',
  '1a000000-0000-0000-0000-00000000001a', 'AUG1', 'STOP', '200', NULL, '1550', '0.1290',
  'cli:a0000000-0000-0000-0000-00000000000a:stop-venta'));

SELECT pg_temp.exigir('comprometidos: 1 350 en efectivo y 0.4231 g (quedan 0.3521 libres)',
  pg_temp.saldos('1a000000-0000-0000-0000-00000000001a') = '17400.07000000 / 1350.00000000'
  AND pg_temp.posicion('a0000000-0000-0000-0000-00000000000a', 'AUG1') = '0.77520000 / 0.42310000 / 1249.93000000');

SELECT pg_temp.debe_fallar('no compromete títulos que no están libres (0.5 de 0.3521)', pg_temp.venta_pendiente(
  'f1000000-0000-0000-0000-0000000000d1', 'a0000000-0000-0000-0000-00000000000a',
  '1a000000-0000-0000-0000-00000000001a', 'AUG1', 'LIMITE', '900', '1800', NULL, '0.5',
  'cli:a0000000-0000-0000-0000-00000000000a:sin-titulos'),
  'SC_POSICION_INSUFICIENTE');

SELECT pg_temp.debe_fallar('títulos fuera del incremento del activo', pg_temp.venta_pendiente(
  'f1000000-0000-0000-0000-0000000000d2', 'a0000000-0000-0000-0000-00000000000a',
  '1a000000-0000-0000-0000-00000000001a', 'AUG1', 'LIMITE', '100', '1800', NULL, '0.05555',
  'cli:a0000000-0000-0000-0000-00000000000a:incremento'),
  'SC_ORDEN_INCOHERENTE');

SELECT pg_temp.debe_fallar('Beto no compromete lo que no tiene', pg_temp.venta_pendiente(
  'f1000000-0000-0000-0000-0000000000d3', 'b0000000-0000-0000-0000-00000000000b',
  '1b000000-0000-0000-0000-00000000001b', 'AUG1', 'LIMITE', '100', '1800', NULL, '0.0555',
  'cli:b0000000-0000-0000-0000-00000000000b:sin-posicion'),
  'SC_POSICION_INSUFICIENTE');

SELECT pg_temp.debe_fallar_en('títulos comprometidos sin su orden pendiente (el dueño los edita)', ARRAY[
  $q$UPDATE mercado.posiciones SET cantidad_comprometida = 0.5 WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a'$q$
], 'mercado.trg_posiciones_titulos_comprometidos', 'SC_COMPROMISO_DESCUADRE');

-- -----------------------------------------------------------------------------
-- Una venta de mercado solo vende títulos libres
-- -----------------------------------------------------------------------------
-- Los 0.3521 g libres valen 567.73 a 1 612.40; antes de 0015 se podían vender 1 249.93.
SELECT pg_temp.debe_fallar('vender a mercado más de lo que valen los títulos libres', pg_temp.orden(
  'VENTA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-0000000000e1', '7e000000-0000-0000-0000-0000000000e1',
  'cli:a0000000-0000-0000-0000-00000000000a:comprometidos', 'AUG1', '600', '0.3721', '1612.40', '599.97',
  '599.97186145'),
  'SC_POSICION_INSUFICIENTE');

SELECT pg_temp.debe_fallar('lo que valen todos los títulos libres los vende todos (0.3521), no una parte', pg_temp.orden(
  'VENTA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-0000000000e2', '7e000000-0000-0000-0000-0000000000e2',
  'cli:a0000000-0000-0000-0000-00000000000a:libres-parcial', 'AUG1', '567.72', '0.3520', '1612.40', '567.56',
  '567.54966465'),
  'SC_ORDEN_INCOHERENTE');

-- 100 MXN son 0.0620 g por 99.97; el costo promedio es sobre los 0.7752 g que tiene.
SELECT pg_temp.debe_pasar('Ana vende 100 MXN de sus títulos libres: 0.0620 g, libera 99.96860165 de costo', pg_temp.orden(
  'VENTA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-000000000002', '7e000000-0000-0000-0000-000000000002',
  'cli:a0000000-0000-0000-0000-00000000000a:libres', 'AUG1', '100', '0.0620', '1612.40', '99.97', '99.96860165'));

SELECT pg_temp.exigir('quedan 0.7132 g, los mismos 0.4231 comprometidos',
  pg_temp.posicion('a0000000-0000-0000-0000-00000000000a', 'AUG1') = '0.71320000 / 0.42310000 / 1149.96139835'
  AND pg_temp.saldos('1a000000-0000-0000-0000-00000000001a') = '17500.04000000 / 1350.00000000');

-- -----------------------------------------------------------------------------
-- La ejecución cumple los términos de la pendiente
-- -----------------------------------------------------------------------------
-- A 1 605 la compra límite (1 600 o menos) no se ejecuta: 0.7788 g por 1 249.97.
SELECT pg_temp.debe_fallar('una compra límite no se ejecuta por encima de su límite', pg_temp.precio('AUG1', '1605')
  || pg_temp.cerrar_compra('f1000000-0000-0000-0000-000000000001', '7c000000-0000-0000-0000-0000000000e3',
                           '1a000000-0000-0000-0000-00000000001a', '1250', 'EJECUTADA', NULL,
                           '0d000000-0000-0000-0000-0000000000e3')
  || pg_temp.orden('COMPRA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
                   '0d000000-0000-0000-0000-0000000000e3', '7e000000-0000-0000-0000-0000000000e3',
                   'ord:f1000000-0000-0000-0000-000000000001:ejecucion', 'AUG1', '1250', '0.7788', '1605', '1249.97'),
  'no cumple la orden pendiente');

SELECT pg_temp.debe_fallar('la ejecución compra con todo el monto de la pendiente, no con otro', pg_temp.precio('AUG1', '1590')
  || pg_temp.cerrar_compra('f1000000-0000-0000-0000-000000000001', '7c000000-0000-0000-0000-0000000000e4',
                           '1a000000-0000-0000-0000-00000000001a', '1250', 'EJECUTADA', NULL,
                           '0d000000-0000-0000-0000-0000000000e4')
  || pg_temp.orden('COMPRA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
                   '0d000000-0000-0000-0000-0000000000e4', '7e000000-0000-0000-0000-0000000000e4',
                   'ord:f1000000-0000-0000-0000-000000000001:ejecucion', 'AUG1', '1000', '0.6289', '1590', '999.95'),
  'no es la ejecución de la pendiente');

SELECT pg_temp.debe_fallar('una stop de venta (1 550) no se ejecuta antes de alcanzarse (1 590)', pg_temp.precio('AUG1', '1590')
  || pg_temp.cerrar_venta('f1000000-0000-0000-0000-000000000005', 'EJECUTADA', NULL, '0d000000-0000-0000-0000-0000000000e5')
  || pg_temp.orden('VENTA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
                   '0d000000-0000-0000-0000-0000000000e5', '7e000000-0000-0000-0000-0000000000e5',
                   'ord:f1000000-0000-0000-0000-000000000005:ejecucion', 'AUG1', '200', '0.1290', '1590', '205.11',
                   '208.00099719'),
  'no cumple la orden pendiente');

SELECT pg_temp.debe_fallar_en('una orden marcada como ejecutada sin su orden en el ledger del mercado',
  pg_temp.cerrar_venta('f1000000-0000-0000-0000-000000000005', 'EJECUTADA', NULL, '0d000000-0000-0000-0000-0000000000e6'),
  'mercado.ordenes_pendientes_orden_ejecutada_id_fkey', 'violates foreign key constraint');

-- El oráculo baja AUG1 a 1 590: la compra límite de 1 250 (1 600 o menos) se ejecuta.
SELECT pg_temp.debe_pasar('AUG1 baja a 1 590', pg_temp.precio('AUG1', '1590'));

-- 1 250 MXN a 1 590 son 0.7861 g por 1 249.90.
SELECT pg_temp.debe_pasar('se ejecuta la compra límite: libera los 1 250 y compra 0.7861 g por 1 249.90',
  pg_temp.cerrar_compra('f1000000-0000-0000-0000-000000000001', '7c000000-0000-0000-0000-000000000011',
                        '1a000000-0000-0000-0000-00000000001a', '1250', 'EJECUTADA', NULL,
                        '0d000000-0000-0000-0000-000000000011')
  || pg_temp.orden('COMPRA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
                   '0d000000-0000-0000-0000-000000000011', '7e000000-0000-0000-0000-000000000011',
                   'ord:f1000000-0000-0000-0000-000000000001:ejecucion', 'AUG1', '1250', '0.7861', '1590', '1249.90'));

SELECT pg_temp.exigir('el resto (0.10) se queda disponible y lo comprometido es el stop de 100',
  pg_temp.saldos('1a000000-0000-0000-0000-00000000001a') = '17500.14000000 / 100.00000000');
SELECT pg_temp.exigir('Ana tiene 1.4993 g que le costaron 2 399.86139835',
  pg_temp.posicion('a0000000-0000-0000-0000-00000000000a', 'AUG1') = '1.49930000 / 0.42310000 / 2399.86139835');
SELECT pg_temp.exigir('la pendiente queda ejecutada, ligada a su orden del ledger del mercado', (
  SELECT p.estado = 'EJECUTADA' AND p.cerrado_en IS NOT NULL AND o.monto_solicitado = p.monto AND o.precio = 1590
    FROM mercado.ordenes_pendientes p JOIN mercado.ordenes o ON o.id = p.orden_ejecutada_id
   WHERE p.id = 'f1000000-0000-0000-0000-000000000001'));
SELECT pg_temp.exigir('PLD evaluó la compra al ejecutarse, con la clave de la ejecución', (
  SELECT count(*) = 1 FROM aml.evaluaciones
   WHERE transaccion_id = '7e000000-0000-0000-0000-000000000011' AND canal = 'COMPRA_MERCADO' AND monto = 1249.90
     AND clave_operacion = 'ord:f1000000-0000-0000-0000-000000000001:ejecucion'));

SELECT pg_temp.debe_fallar('una pendiente ejecutada no se cancela', pg_temp.cerrar_compra(
  'f1000000-0000-0000-0000-000000000001', '7c000000-0000-0000-0000-0000000000e7',
  '1a000000-0000-0000-0000-00000000001a', '1250', 'CANCELADA'),
  'SC_ORDEN_NO_PENDIENTE');

-- El oráculo sube AUG1 a 1 710: la venta límite (1 700 o más) y el stop de compra (1 650) se ejecutan.
SELECT pg_temp.debe_pasar('AUG1 sube a 1 710', pg_temp.precio('AUG1', '1710'));

SELECT pg_temp.debe_fallar('la venta de una pendiente vende exactamente sus títulos (0.2941, no 0.2940)',
  pg_temp.cerrar_venta('f1000000-0000-0000-0000-000000000003', 'EJECUTADA', NULL, '0d000000-0000-0000-0000-0000000000e8')
  || pg_temp.orden('VENTA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
                   '0d000000-0000-0000-0000-0000000000e8', '7e000000-0000-0000-0000-0000000000e8',
                   'ord:f1000000-0000-0000-0000-000000000003:ejecucion', 'AUG1', '500', '0.2940', '1710', '502.74',
                   '470.59244255'),
  'vende 0.29410000 títulos');

-- 0.2941 g a 1 710 son 502.91; el costo promedio sobre los 1.4993 g que tiene.
SELECT pg_temp.debe_pasar('se ejecuta la venta límite: 0.2941 g por 502.91, libera 470.75250934 de costo',
  pg_temp.cerrar_venta('f1000000-0000-0000-0000-000000000003', 'EJECUTADA', NULL, '0d000000-0000-0000-0000-000000000012')
  || pg_temp.orden('VENTA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
                   '0d000000-0000-0000-0000-000000000012', '7e000000-0000-0000-0000-000000000012',
                   'ord:f1000000-0000-0000-0000-000000000003:ejecucion', 'AUG1', '500', '0.2941', '1710', '502.91',
                   '470.75250934'));

SELECT pg_temp.exigir('la venta abona 502.91 (más que los 500: el precio superó el límite)',
  pg_temp.saldos('1a000000-0000-0000-0000-00000000001a') = '18003.05000000 / 100.00000000');
SELECT pg_temp.exigir('quedan 1.2052 g; comprometidos solo los 0.1290 del stop de venta',
  pg_temp.posicion('a0000000-0000-0000-0000-00000000000a', 'AUG1') = '1.20520000 / 0.12900000 / 1929.10888901');

-- 100 MXN a 1 710 son 0.0584 g por 99.86.
SELECT pg_temp.debe_pasar('se ejecuta el stop de compra: libera 100 y compra 0.0584 g por 99.86',
  pg_temp.cerrar_compra('f1000000-0000-0000-0000-000000000004', '7c000000-0000-0000-0000-000000000014',
                        '1a000000-0000-0000-0000-00000000001a', '100', 'EJECUTADA', NULL,
                        '0d000000-0000-0000-0000-000000000014')
  || pg_temp.orden('COMPRA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
                   '0d000000-0000-0000-0000-000000000014', '7e000000-0000-0000-0000-000000000014',
                   'ord:f1000000-0000-0000-0000-000000000004:ejecucion', 'AUG1', '100', '0.0584', '1710', '99.86'));

SELECT pg_temp.exigir('sin efectivo comprometido: 18 003.19 disponibles',
  pg_temp.saldos('1a000000-0000-0000-0000-00000000001a') = '18003.19000000 / 0.00000000');

-- El oráculo baja AUG1 a 1 540: el stop de venta (1 550) se ejecuta.
SELECT pg_temp.debe_pasar('AUG1 baja a 1 540', pg_temp.precio('AUG1', '1540'));

-- 0.1290 g a 1 540 son 198.66.
SELECT pg_temp.debe_pasar('se ejecuta el stop de venta: 0.1290 g por 198.66, libera 207.13595021 de costo',
  pg_temp.cerrar_venta('f1000000-0000-0000-0000-000000000005', 'EJECUTADA', NULL, '0d000000-0000-0000-0000-000000000015')
  || pg_temp.orden('VENTA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
                   '0d000000-0000-0000-0000-000000000015', '7e000000-0000-0000-0000-000000000015',
                   'ord:f1000000-0000-0000-0000-000000000005:ejecucion', 'AUG1', '200', '0.1290', '1540', '198.66',
                   '207.13595021'));

SELECT pg_temp.exigir('Ana: 18 201.85 disponibles, 1.1346 g sin nada comprometido',
  pg_temp.saldos('1a000000-0000-0000-0000-00000000001a') = '18201.85000000 / 0.00000000'
  AND pg_temp.posicion('a0000000-0000-0000-0000-00000000000a', 'AUG1') = '1.13460000 / 0.00000000 / 1821.83293880');

-- -----------------------------------------------------------------------------
-- Rechazo: una cuenta congelada aún recupera lo comprometido
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Beto: compra límite de 50 MXN de AUG1 a 1 500 o menos',
  pg_temp.compra_pendiente('f1000000-0000-0000-0000-000000000006', '7c000000-0000-0000-0000-000000000006',
                           'b0000000-0000-0000-0000-00000000000b', '1b000000-0000-0000-0000-00000000001b', 'AUG1',
                           'LIMITE', '50', '1500', NULL, 'cli:b0000000-0000-0000-0000-00000000000b:limite'));

SELECT pg_temp.debe_pasar('el operador congela la cuenta de Beto; la orden se rechaza y el efectivo regresa', ARRAY[
  $q$UPDATE ifpe.cuentas SET estado = 'CONGELADA' WHERE id = '1b000000-0000-0000-0000-00000000001b'$q$
] || pg_temp.cerrar_compra('f1000000-0000-0000-0000-000000000006', '7c000000-0000-0000-0000-000000000016',
                           '1b000000-0000-0000-0000-00000000001b', '50', 'RECHAZADA', 'CUENTA_CONGELADA'));

SELECT pg_temp.exigir('Beto: sus 100 disponibles otra vez, la orden rechazada con su motivo',
  pg_temp.saldos('1b000000-0000-0000-0000-00000000001b') = '100.00000000 / 0.00000000'
  AND (SELECT estado = 'RECHAZADA' AND motivo = 'CUENTA_CONGELADA' AND cerrado_en IS NOT NULL
         FROM mercado.ordenes_pendientes WHERE id = 'f1000000-0000-0000-0000-000000000006'));

-- -----------------------------------------------------------------------------
-- RLS: cada cliente lee solo sus órdenes pendientes
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('como Ana, sc_cliente ve sus cinco órdenes y ninguna de Beto', (
  SELECT set_config('sc.cliente_actual', 'a0000000-0000-0000-0000-00000000000a', true) IS NOT NULL));
SET LOCAL ROLE sc_cliente;
SELECT pg_temp.exigir('… sus cinco órdenes', (
  SELECT count(*) = 5 AND bool_and(cliente_id = 'a0000000-0000-0000-0000-00000000000a')
    FROM mercado.ordenes_pendientes));
SELECT pg_temp.exigir('… aunque pida la de Beto por id', (
  SELECT count(*) = 0 FROM mercado.ordenes_pendientes WHERE id = 'f1000000-0000-0000-0000-000000000006'));
RESET ROLE;
SELECT set_config('sc.cliente_actual', '', true);
SET LOCAL ROLE sc_cliente;
SELECT pg_temp.exigir('sin cliente en la transacción, sc_cliente no ve órdenes pendientes', (
  SELECT count(*) = 0 FROM mercado.ordenes_pendientes));
RESET ROLE;

-- -----------------------------------------------------------------------------
-- Invariantes globales al final
-- -----------------------------------------------------------------------------
SET CONSTRAINTS ALL IMMEDIATE;
SELECT pg_temp.exigir('reserva del 100 %: pasivo = custodia = suma de los tres saldos = suma de las hojas', (
  SELECT e.pasivo_clientes = e.activo_reserva
         AND e.activo_reserva = (SELECT sum(saldo) FROM ifpe.reserva_custodia)
         AND e.pasivo_clientes = (SELECT sum(saldo_disponible + saldo_bloqueado + saldo_comprometido) FROM ifpe.cuentas)
         AND e.pasivo_clientes = (SELECT sum(saldo_total) FROM ifpe.v_hojas_reserva)
    FROM ifpe.estado_reserva e));
SELECT pg_temp.exigir('lo comprometido es lo de las órdenes pendientes (hoy, nada)', (
  SELECT sum(saldo_comprometido) = 0 FROM ifpe.cuentas)
  AND (SELECT sum(cantidad_comprometida) = 0 FROM mercado.posiciones)
  AND (SELECT count(*) = 0 FROM mercado.ordenes_pendientes WHERE estado = 'PENDIENTE'));
SELECT pg_temp.exigir('cada compra pendiente tuvo su compromiso y su liberación, por el mismo monto', (
  SELECT count(*) = 4 AND bool_and(c.monto = p.monto AND l.monto = p.monto)
    FROM mercado.ordenes_pendientes p
    JOIN ifpe.transacciones c ON c.id = p.tx_compromiso_id AND c.tipo = 'COMPROMISO_ORDEN'
    JOIN ifpe.transacciones l ON l.id = p.tx_liberacion_id AND l.tipo = 'LIBERACION_ORDEN'
   WHERE p.lado = 'COMPRA'));
SELECT pg_temp.exigir('el ledger IFPE sigue encadenado con los movimientos nuevos', (
  SELECT bool_and(hash_propio AND enlace)
    FROM (SELECT tx_hash = nucleo.sha256_hex(ifpe.payload_canonico(t)) AS hash_propio,
                 hash_anterior = coalesce(lag(tx_hash) OVER (ORDER BY secuencia), nucleo.hash_genesis()) AS enlace
            FROM ifpe.transacciones t) cadena));

\o
\echo 'tests/db/0015_ordenes_avanzadas.sql: todas las pruebas pasaron (ROLLBACK)'
ROLLBACK;
