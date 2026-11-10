-- =============================================================================
--  Pruebas de la migración 0013 (mercado de valores). Corren en una
--  transacción que termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0013_mercado_valores.sql
--
--  Lo que la base de datos garantiza por sí sola: una orden y su movimiento de
--  efectivo nacen juntos o no nacen; la orden ejecuta al precio del catálogo
--  la cantidad exacta que cubre el monto, sin comisión; la posición es la
--  reproducción de su ledger; el ledger del mercado es inmutable y encadenado;
--  solo sc_mercado lo escribe, solo con KYC y nunca a un cliente congelado; la
--  compra pasa por PLD como la inversión en mutuo; y cada cliente lee solo lo
--  suyo. backend/tests/mercado.rs cubre el camino completo desde la API.
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

-- Las sentencias de una orden completa, como las escribe el backend
-- (backend/src/mercado): PLD (solo compras), saldos, movimiento IFPE y orden.
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

CREATE FUNCTION pg_temp.saldo(cuenta UUID) RETURNS NUMERIC
LANGUAGE sql AS $$ SELECT saldo_disponible FROM ifpe.cuentas WHERE id = cuenta $$;

CREATE FUNCTION pg_temp.posicion(cliente UUID, activo TEXT) RETURNS TEXT
LANGUAGE sql AS $$
    SELECT coalesce((SELECT cantidad::text || ' / ' || costo::text FROM mercado.posiciones
                      WHERE cliente_id = cliente AND ticker = activo), 'sin posición')
$$;

-- -----------------------------------------------------------------------------
-- Datos: Ana y Beto verificados, Carla sin KYC; la reserva; 20 000 para Ana
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

SELECT pg_temp.debe_pasar('depósito SPEI de Beto (100), como sc_spei', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 100 WHERE id = '1b000000-0000-0000-0000-00000000001b'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 100 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
     VALUES ('7d000000-0000-0000-0000-000000000002', 'DEPOSITO', '1b000000-0000-0000-0000-00000000001b',
             'e0000000-0000-0000-0000-00000000000e', 100, 'spei:40012:20261101:MER0002', '40012:20261101:MER0002')$q$,
  $q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                    estado, transaccion_id, origen, raw_payload)
     VALUES ('40012', 'MER0002', '2026-11-01', '646180157000000020', '1b000000-0000-0000-0000-00000000001b', 100,
             'ABONADO', '7d000000-0000-0000-0000-000000000002', 'WEBHOOK', '{}')$q$,
  'RESET ROLE'
]);

SELECT pg_temp.debe_pasar('depósito SPEI de Ana (20 000), como sc_spei', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 20000 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 20000 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
     VALUES ('7d000000-0000-0000-0000-000000000001', 'DEPOSITO', '1a000000-0000-0000-0000-00000000001a',
             'e0000000-0000-0000-0000-00000000000e', 20000, 'spei:40012:20261101:MER0001', '40012:20261101:MER0001')$q$,
  $q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                    estado, transaccion_id, origen, raw_payload)
     VALUES ('40012', 'MER0001', '2026-11-01', '646180157000000017', '1a000000-0000-0000-0000-00000000001a', 20000,
             'ABONADO', '7d000000-0000-0000-0000-000000000001', 'WEBHOOK', '{}')$q$,
  'RESET ROLE'
]);

-- -----------------------------------------------------------------------------
-- Estructura y catálogo
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('rol sc_mercado NOLOGIN y esquema mercado sin PUBLIC', (
  SELECT NOT rolcanlogin FROM pg_roles WHERE rolname = 'sc_mercado')
  AND NOT has_schema_privilege('public', 'mercado', 'USAGE'));

-- Solo el catálogo de esta migración: las siguientes (0014) le añaden activos.
SELECT pg_temp.exigir('el catálogo inicial: AUG1, AGT1 y SCT28, todos con precio SIMULADO', (
  SELECT array_agg(ticker ORDER BY ticker) = ARRAY['AGT1', 'AUG1', 'SCT28'] AND bool_and(fuente_precio = 'SIMULADO')
    FROM mercado.activos WHERE ticker IN ('AGT1', 'AUG1', 'SCT28')));

SELECT pg_temp.exigir('RLS en órdenes y posiciones, con la política RESTRICTIVE de sc_cliente (0011)', (
  SELECT count(*) = 2 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE n.nspname = 'mercado' AND c.relname IN ('ordenes', 'posiciones') AND c.relrowsecurity)
  AND (SELECT count(*) = 2 FROM pg_policies
        WHERE schemaname = 'mercado' AND policyname = 'solo_del_cliente' AND permissive = 'RESTRICTIVE'
          AND cmd = 'ALL' AND roles = '{sc_cliente}'));

SELECT pg_temp.exigir('sc_cliente solo lee el mercado: SELECT en activos, órdenes y posiciones', (
  SELECT count(*) FILTER (WHERE privilege_type = 'SELECT') = 3 AND count(*) FILTER (WHERE privilege_type <> 'SELECT') = 0
    FROM information_schema.role_table_grants WHERE grantee = 'sc_cliente' AND table_schema = 'mercado'));

SELECT pg_temp.exigir('oro tokenizado: 1 612.40 MXN en incrementos de 0.0001', (
  SELECT precio = 1612.40 AND decimales_cantidad = 4 AND operable FROM mercado.activos WHERE ticker = 'AUG1'));

SELECT pg_temp.debe_fallar('sc_mercado no cambia precios', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE mercado.activos SET precio = 1 WHERE ticker = 'AUG1'$q$
], 'permission denied');

SELECT pg_temp.debe_fallar('el incremento de un activo no cambia (ni el dueño)', ARRAY[
  $q$UPDATE mercado.activos SET decimales_cantidad = 2 WHERE ticker = 'AUG1'$q$
], 'SC_ACTIVO_INMUTABLE');

SELECT pg_temp.debe_fallar('sc_mercado no escribe posiciones', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$INSERT INTO mercado.posiciones (cliente_id, ticker, cantidad) VALUES ('a0000000-0000-0000-0000-00000000000a', 'AUG1', 1)$q$
], 'permission denied');

SELECT pg_temp.debe_fallar('sc_cliente no coloca órdenes', ARRAY[
  'SET LOCAL ROLE sc_cliente',
  $q$INSERT INTO mercado.ordenes (id, cliente_id, cuenta_ifpe_id, ticker, lado, monto_solicitado, cantidad, precio, monto,
                                  fuente_precio, tx_ifpe_id, clave_idempotencia)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a', 'AUG1',
             'COMPRA', 1250, 0.7752, 1612.40, 1249.93, 'SIMULADO', gen_random_uuid(), 'x')$q$
], 'permission denied');

-- -----------------------------------------------------------------------------
-- Una compra: 1 250 MXN de oro a 1 612.40 son 0.7752 g por 1 249.93
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana compra 1 250 MXN de AUG1, como sc_mercado',
  pg_temp.orden('COMPRA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
                '0d000000-0000-0000-0000-000000000001', '7e000000-0000-0000-0000-000000000001',
                'cli:a0000000-0000-0000-0000-00000000000a:compra-1', 'AUG1', '1250', '0.7752', '1612.40', '1249.93'));

SELECT pg_temp.exigir('la bóveda de Ana baja 1 249.93: el resto de los 1 250 se queda en efectivo',
  pg_temp.saldo('1a000000-0000-0000-0000-00000000001a') = 18750.07);
SELECT pg_temp.exigir('Ana tiene 0.7752 AUG1 que le costaron 1 249.93',
  pg_temp.posicion('a0000000-0000-0000-0000-00000000000a', 'AUG1') = '0.77520000 / 1249.93000000');
SELECT pg_temp.exigir('la reserva sigue al 100 %: el efectivo de la compra salió de la custodia', (
  SELECT pasivo_clientes = activo_reserva AND activo_reserva = 18850.07 FROM ifpe.estado_reserva));
-- Esta suite es una sola transacción, y seq sube una vez por transacción (0012):
-- la compra queda en la versión del depósito. backend/tests/boveda.rs prueba
-- el aviso de una orden confirmada.
SELECT pg_temp.exigir('la versión de la bóveda de Ana queda sellada con esta transacción', (
  SELECT seq = 1 AND seq_xact = pg_current_xact_id() FROM ifpe.cuentas WHERE id = '1a000000-0000-0000-0000-00000000001a'));
SELECT pg_temp.exigir('la orden es la primera del ledger del mercado y su hash es el de su payload', (
  SELECT secuencia = 1 AND hash_anterior = nucleo.hash_genesis()
         AND tx_hash = nucleo.sha256_hex(mercado.payload_canonico(o)) AND comision = 0
    FROM mercado.ordenes o WHERE id = '0d000000-0000-0000-0000-000000000001'));
SELECT pg_temp.exigir('la cabeza del ledger del mercado apunta a la orden', (
  SELECT c.ultima_secuencia = 1 AND c.ultimo_hash = o.tx_hash
    FROM mercado.ledger_cabeza c, mercado.ordenes o WHERE o.id = '0d000000-0000-0000-0000-000000000001'));
SELECT pg_temp.exigir('PLD evaluó la compra en línea, en el canal COMPRA_MERCADO', (
  SELECT count(*) = 1 FROM aml.evaluaciones
   WHERE transaccion_id = '7e000000-0000-0000-0000-000000000001' AND canal = 'COMPRA_MERCADO'
     AND sentido = 'SALIDA' AND origen = 'EN_LINEA' AND monto = 1249.93));
SELECT pg_temp.exigir('… y la cuenta para la velocidad del cliente', (
  SELECT count(*) = 1 FROM aml.movimientos_cliente('a0000000-0000-0000-0000-00000000000a', '-infinity', 'infinity', NULL)
   WHERE canal = 'COMPRA_MERCADO' AND sentido = 'SALIDA' AND monto = 1249.93));

-- -----------------------------------------------------------------------------
-- Una orden y su movimiento nacen juntos
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('una orden sin su movimiento IFPE', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$INSERT INTO mercado.ordenes (id, cliente_id, cuenta_ifpe_id, ticker, lado, monto_solicitado, cantidad, precio, monto,
                                  fuente_precio, tx_ifpe_id, clave_idempotencia)
     VALUES ('0d000000-0000-0000-0000-0000000000f1', 'a0000000-0000-0000-0000-00000000000a',
             '1a000000-0000-0000-0000-00000000001a', 'AUG1', 'COMPRA', 100, 0.0620, 1612.40, 99.97, 'SIMULADO',
             '7e000000-0000-0000-0000-0000000000f1', 'cli:a0000000-0000-0000-0000-00000000000a:sin-movimiento')$q$
], 'SC_ORDEN_SIN_MOVIMIENTO');

SELECT pg_temp.debe_fallar('un movimiento COMPRA_MERCADO sin su orden', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo - 100 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia, ref_movimiento_espejo)
     VALUES ('7e000000-0000-0000-0000-0000000000f2', 'COMPRA_MERCADO', '1a000000-0000-0000-0000-00000000001a',
             'e0000000-0000-0000-0000-00000000000e', 100, 'cli:a0000000-0000-0000-0000-00000000000a:sin-orden',
             '0d000000-0000-0000-0000-0000000000f2')$q$
], 'SC_MERCADO_SIN_ORDEN');

SELECT pg_temp.debe_fallar('una orden cuyo movimiento IFPE es de otro monto', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo - 100 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia, ref_movimiento_espejo)
     VALUES ('7e000000-0000-0000-0000-0000000000f3', 'COMPRA_MERCADO', '1a000000-0000-0000-0000-00000000001a',
             'e0000000-0000-0000-0000-00000000000e', 100, 'cli:a0000000-0000-0000-0000-00000000000a:otro-monto',
             '0d000000-0000-0000-0000-0000000000f3')$q$,
  $q$INSERT INTO mercado.ordenes (id, cliente_id, cuenta_ifpe_id, ticker, lado, monto_solicitado, cantidad, precio, monto,
                                  fuente_precio, tx_ifpe_id, clave_idempotencia)
     VALUES ('0d000000-0000-0000-0000-0000000000f3', 'a0000000-0000-0000-0000-00000000000a',
             '1a000000-0000-0000-0000-00000000001a', 'AUG1', 'COMPRA', 100, 0.0620, 1612.40, 99.97, 'SIMULADO',
             '7e000000-0000-0000-0000-0000000000f3', 'cli:a0000000-0000-0000-0000-00000000000a:otro-monto')$q$
], 'en esta transacción');

-- -----------------------------------------------------------------------------
-- El precio, la cantidad y el importe son los del catálogo, exactos
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('un precio que no es el del catálogo', pg_temp.orden(
  'COMPRA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-0000000000f4', '7e000000-0000-0000-0000-0000000000f4',
  'cli:a0000000-0000-0000-0000-00000000000a:precio', 'AUG1', '1250', '0.7812', '1600.00', '1249.92'),
  'SC_MERCADO_PRECIO_CAMBIO');

SELECT pg_temp.debe_fallar('menos de lo que compra el monto (0.7751 en vez de 0.7752)', pg_temp.orden(
  'COMPRA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-0000000000f5', '7e000000-0000-0000-0000-0000000000f5',
  'cli:a0000000-0000-0000-0000-00000000000a:menos', 'AUG1', '1250', '0.7751', '1612.40', '1249.77'),
  'SC_ORDEN_INCOHERENTE');

SELECT pg_temp.debe_fallar('una cantidad fuera del incremento del activo', pg_temp.orden(
  'COMPRA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-0000000000f6', '7e000000-0000-0000-0000-0000000000f6',
  'cli:a0000000-0000-0000-0000-00000000000a:incremento', 'AUG1', '1250', '0.77521', '1612.40', '1249.95'),
  'SC_ORDEN_INCOHERENTE');

SELECT pg_temp.debe_fallar('un importe que no es cantidad × precio al centavo', pg_temp.orden(
  'COMPRA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-0000000000f7', '7e000000-0000-0000-0000-0000000000f7',
  'cli:a0000000-0000-0000-0000-00000000000a:importe', 'AUG1', '1250', '0.7752', '1612.40', '1249.94'),
  'ck_mercado_ordenes_importe');

SELECT pg_temp.debe_fallar('un monto pedido con fracciones de centavo', pg_temp.orden(
  'COMPRA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-0000000000f8', '7e000000-0000-0000-0000-0000000000f8',
  'cli:a0000000-0000-0000-0000-00000000000a:centavos', 'AUG1', '1250.005', '0.7752', '1612.40', '1249.93'),
  'monto_solicitado');

SELECT pg_temp.debe_fallar('una comisión distinta de cero', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$INSERT INTO mercado.ordenes (id, cliente_id, cuenta_ifpe_id, ticker, lado, monto_solicitado, cantidad, precio, monto,
                                  comision, fuente_precio, tx_ifpe_id, clave_idempotencia)
     VALUES ('0d000000-0000-0000-0000-0000000000f9', 'a0000000-0000-0000-0000-00000000000a',
             '1a000000-0000-0000-0000-00000000001a', 'AUG1', 'COMPRA', 1250, 0.7752, 1612.40, 1249.93, 1, 'SIMULADO',
             '7e000000-0000-0000-0000-0000000000f9', 'cli:a0000000-0000-0000-0000-00000000000a:comision')$q$
], 'ck_mercado_ordenes_sin_comision');

SELECT pg_temp.debe_fallar('un activo suspendido no opera', ARRAY[
  $q$UPDATE mercado.activos SET operable = FALSE WHERE ticker = 'AUG1'$q$
] || pg_temp.orden(
  'COMPRA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-0000000000fa', '7e000000-0000-0000-0000-0000000000fa',
  'cli:a0000000-0000-0000-0000-00000000000a:suspendido', 'AUG1', '1250', '0.7752', '1612.40', '1249.93'),
  'SC_MERCADO_CERRADO');

-- -----------------------------------------------------------------------------
-- Quién opera: sc_mercado, con KYC, sin congelamiento
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('Carla, sin KYC, no compra (movimiento IFPE)', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia, ref_movimiento_espejo)
     VALUES ('7e000000-0000-0000-0000-0000000000fb', 'COMPRA_MERCADO', '1c000000-0000-0000-0000-00000000001c',
             'e0000000-0000-0000-0000-00000000000e', 9.92, 'cli:c0000000-0000-0000-0000-00000000000c:sin-kyc',
             '0d000000-0000-0000-0000-0000000000fb')$q$
], 'row-level security');

SELECT pg_temp.debe_fallar('Carla, sin KYC, no compra (la orden sola)', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$INSERT INTO mercado.ordenes (id, cliente_id, cuenta_ifpe_id, ticker, lado, monto_solicitado, cantidad, precio, monto,
                                  fuente_precio, tx_ifpe_id, clave_idempotencia)
     VALUES ('0d000000-0000-0000-0000-0000000000fc', 'c0000000-0000-0000-0000-00000000000c',
             '1c000000-0000-0000-0000-00000000001c', 'SCT28', 'COMPRA', 9.92, 1, 9.92, 9.92, 'SIMULADO',
             '7e000000-0000-0000-0000-0000000000fc', 'cli:c0000000-0000-0000-0000-00000000000c:sin-kyc-2')$q$
], 'row-level security');

SELECT pg_temp.debe_fallar('una cuenta congelada por el operador no compra', ARRAY[
  $q$UPDATE ifpe.cuentas SET estado = 'CONGELADA' WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$
] || pg_temp.orden(
  'COMPRA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-0000000000fd', '7e000000-0000-0000-0000-0000000000fd',
  'cli:a0000000-0000-0000-0000-00000000000a:congelada', 'AUG1', '1250', '0.7752', '1612.40', '1249.93'),
  'SC_CUENTA_CONGELADA');

SELECT pg_temp.debe_fallar('… ni vende', ARRAY[
  $q$UPDATE ifpe.cuentas SET estado = 'CONGELADA' WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$
] || pg_temp.orden(
  'VENTA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-0000000000fe', '7e000000-0000-0000-0000-0000000000fe',
  'cli:a0000000-0000-0000-0000-00000000000a:congelada-venta', 'AUG1', '500', '0.3100', '1612.40', '499.84',
  '499.84300826'),
  'SC_CUENTA_CONGELADA');

SELECT pg_temp.debe_fallar('solo sc_mercado escribe movimientos del mercado', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia, ref_movimiento_espejo)
     VALUES (gen_random_uuid(), 'COMPRA_MERCADO', '1a000000-0000-0000-0000-00000000001a',
             'e0000000-0000-0000-0000-00000000000e', 100, 'cli:x:ifpe-mercado', gen_random_uuid())$q$
], 'row-level security');

SELECT pg_temp.debe_fallar('sc_mercado solo escribe sus dos movimientos (no una transferencia)', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 100 WHERE id = '1b000000-0000-0000-0000-00000000001b'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia)
     VALUES (gen_random_uuid(), 'TRANSFERENCIA', '1a000000-0000-0000-0000-00000000001a',
             '1b000000-0000-0000-0000-00000000001b', 100, 'cli:x:mercado-transfiere')$q$
], 'row-level security');

SELECT pg_temp.debe_fallar('sc_mercado no mueve el saldo retenido', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE ifpe.cuentas SET saldo_bloqueado = 1 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$
], 'permission denied');

SELECT pg_temp.debe_fallar('PLD: sc_mercado no evalúa la inversión en mutuo', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$SELECT ifpe.aml_evaluar('a0000000-0000-0000-0000-00000000000a', 'SALIDA', 'INVERSION_MUTUO', 100,
                             gen_random_uuid(), 'cli:x:mutuo', '{}'::jsonb)$q$
], 'SC_AML_PARAMETRO_INVALIDO');

SELECT pg_temp.debe_fallar('PLD: la venta no es un canal (dinero del propio cliente que regresa)', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$SELECT ifpe.aml_evaluar('a0000000-0000-0000-0000-00000000000a', 'ENTRADA', 'VENTA_MERCADO', 100,
                             gen_random_uuid(), 'cli:x:venta', '{}'::jsonb)$q$
], 'SC_AML_PARAMETRO_INVALIDO');

-- -----------------------------------------------------------------------------
-- Ventas: parcial (costo promedio) y total (sin residuos)
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('Beto no vende lo que no tiene', pg_temp.orden(
  'VENTA', 'b0000000-0000-0000-0000-00000000000b', '1b000000-0000-0000-0000-00000000001b',
  '0d000000-0000-0000-0000-0000000000e1', '7e000000-0000-0000-0000-0000000000e1',
  'cli:b0000000-0000-0000-0000-00000000000b:sin-posicion', 'AUG1', '100', '0.0620', '1612.40', '99.97', '0'),
  'SC_POSICION_INSUFICIENTE');

SELECT pg_temp.debe_fallar('Ana no vende más de lo que vale su posición', pg_temp.orden(
  'VENTA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-0000000000e2', '7e000000-0000-0000-0000-0000000000e2',
  'cli:a0000000-0000-0000-0000-00000000000a:de-mas', 'AUG1', '1300', '0.7752', '1612.40', '1249.93', '1249.93'),
  'SC_POSICION_INSUFICIENTE');

SELECT pg_temp.debe_fallar('una venta parcial que libera otro costo', pg_temp.orden(
  'VENTA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-0000000000e3', '7e000000-0000-0000-0000-0000000000e3',
  'cli:a0000000-0000-0000-0000-00000000000a:costo', 'AUG1', '500', '0.3100', '1612.40', '499.84', '500'),
  'SC_ORDEN_INCOHERENTE');

SELECT pg_temp.debe_pasar('Ana vende 500 MXN: 0.3100 g por 499.84, libera 499.84300826 de costo', pg_temp.orden(
  'VENTA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-000000000002', '7e000000-0000-0000-0000-000000000002',
  'cli:a0000000-0000-0000-0000-00000000000a:venta-1', 'AUG1', '500', '0.3100', '1612.40', '499.84', '499.84300826'));

SELECT pg_temp.exigir('la venta abona 499.84 a la bóveda de Ana',
  pg_temp.saldo('1a000000-0000-0000-0000-00000000001a') = 19249.91);
SELECT pg_temp.exigir('quedan 0.4652 g con su costo promedio',
  pg_temp.posicion('a0000000-0000-0000-0000-00000000000a', 'AUG1') = '0.46520000 / 750.08699174');
SELECT pg_temp.exigir('la venta no se evalúa en PLD (como RETORNO_DE_SOFOM) ni deja marca diferida', (
  SELECT count(*) = 0 FROM aml.evaluaciones WHERE transaccion_id = '7e000000-0000-0000-0000-000000000002'));

SELECT pg_temp.debe_fallar('lo que vale toda la posición vende toda la posición, no una parte', pg_temp.orden(
  'VENTA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-0000000000e4', '7e000000-0000-0000-0000-0000000000e4',
  'cli:a0000000-0000-0000-0000-00000000000a:parcial', 'AUG1', '750.08', '0.4651', '1612.40', '749.93', '749.9'),
  'SC_ORDEN_INCOHERENTE');

SELECT pg_temp.debe_pasar('Ana vende lo que vale el resto (750.08): toda la posición, por 750.09', pg_temp.orden(
  'VENTA', 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
  '0d000000-0000-0000-0000-000000000003', '7e000000-0000-0000-0000-000000000003',
  'cli:a0000000-0000-0000-0000-00000000000a:venta-2', 'AUG1', '750.08', '0.4652', '1612.40', '750.09', '750.08699174'));

SELECT pg_temp.exigir('al mismo precio, comprar y vender todo devuelve exactamente lo pagado',
  pg_temp.saldo('1a000000-0000-0000-0000-00000000001a') = 20000);
SELECT pg_temp.exigir('la posición queda cerrada, sin costo',
  pg_temp.posicion('a0000000-0000-0000-0000-00000000000a', 'AUG1') = '0.00000000 / 0.00000000');
SELECT pg_temp.exigir('la reserva sigue al 100 % con todo de vuelta en custodia', (
  SELECT pasivo_clientes = activo_reserva AND activo_reserva = 20100 FROM ifpe.estado_reserva));

-- -----------------------------------------------------------------------------
-- El ledger del mercado: encadenado, inmutable y la única fuente de posiciones
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('tres órdenes encadenadas: cada hash es su payload y enlaza con el anterior', (
  SELECT count(*) = 3 AND bool_and(hash_propio AND enlace)
    FROM (SELECT tx_hash = nucleo.sha256_hex(mercado.payload_canonico(o)) AS hash_propio,
                 hash_anterior = coalesce(lag(tx_hash) OVER (ORDER BY secuencia), nucleo.hash_genesis()) AS enlace
            FROM mercado.ordenes o) cadena));

SELECT pg_temp.debe_fallar('una orden no se modifica (ni el dueño)', ARRAY[
  $q$UPDATE mercado.ordenes SET precio = 1 WHERE id = '0d000000-0000-0000-0000-000000000001'$q$
], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('una orden no se borra', ARRAY[
  $q$DELETE FROM mercado.ordenes WHERE id = '0d000000-0000-0000-0000-000000000001'$q$
], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('el ledger del mercado no se trunca', ARRAY[
  'TRUNCATE mercado.ordenes'
], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('una posición que el ledger no justifica (el dueño la edita)', ARRAY[
  $q$UPDATE mercado.posiciones SET cantidad = 10, costo = 1 WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a'$q$
], 'SC_POSICION_SIN_LEDGER');

-- -----------------------------------------------------------------------------
-- RLS: cada cliente lee el catálogo y solo lo suyo
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Beto compra 1 SCT28', pg_temp.orden(
  'COMPRA', 'b0000000-0000-0000-0000-00000000000b', '1b000000-0000-0000-0000-00000000001b',
  '0d000000-0000-0000-0000-000000000004', '7e000000-0000-0000-0000-000000000004',
  'cli:b0000000-0000-0000-0000-00000000000b:compra-1', 'SCT28', '10', '1', '9.92', '9.92'));

SELECT pg_temp.exigir('Beto: 1 SCT28 por 9.92, sin decimales (incremento de 1 unidad)',
  pg_temp.posicion('b0000000-0000-0000-0000-00000000000b', 'SCT28') = '1.00000000 / 9.92000000'
  AND pg_temp.saldo('1b000000-0000-0000-0000-00000000001b') = 90.08);

-- El catálogo completo, contado como dueño, para compararlo con lo que ve el cliente.
SELECT count(*) AS catalogo FROM mercado.activos \gset
SELECT pg_temp.exigir('como Ana, sc_cliente ve sus tres órdenes, su posición y el catálogo completo', (
  SELECT set_config('sc.cliente_actual', 'a0000000-0000-0000-0000-00000000000a', true) IS NOT NULL));
SET LOCAL ROLE sc_cliente;
SELECT pg_temp.exigir('… sus órdenes, ninguna de Beto', (
  SELECT count(*) = 3 AND bool_and(cliente_id = 'a0000000-0000-0000-0000-00000000000a') FROM mercado.ordenes));
SELECT pg_temp.exigir('… su posición, ninguna de Beto', (
  SELECT count(*) = 1 AND bool_and(cliente_id = 'a0000000-0000-0000-0000-00000000000a') FROM mercado.posiciones));
SELECT pg_temp.exigir('… y el catálogo completo', (SELECT count(*) = :catalogo AND :catalogo >= 3 FROM mercado.activos));
SELECT pg_temp.exigir('… aunque pida las de Beto por id', (
  SELECT count(*) = 0 FROM mercado.ordenes WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b'));
RESET ROLE;
SELECT set_config('sc.cliente_actual', '', true);
SET LOCAL ROLE sc_cliente;
SELECT pg_temp.exigir('sin cliente en la transacción, sc_cliente no ve órdenes ni posiciones', (
  SELECT (SELECT count(*) FROM mercado.ordenes) = 0 AND (SELECT count(*) FROM mercado.posiciones) = 0));
RESET ROLE;

-- -----------------------------------------------------------------------------
-- Invariantes globales al final
-- -----------------------------------------------------------------------------
SET CONSTRAINTS ALL IMMEDIATE;
SELECT pg_temp.exigir('reserva del 100 %: pasivo = custodia = suma de las cuentas', (
  SELECT e.pasivo_clientes = e.activo_reserva
         AND e.activo_reserva = (SELECT sum(saldo) FROM ifpe.reserva_custodia)
         AND e.pasivo_clientes = (SELECT sum(saldo_disponible + saldo_bloqueado) FROM ifpe.cuentas)
    FROM ifpe.estado_reserva e));
SELECT pg_temp.exigir('cada movimiento del mercado tiene su orden y cada orden su movimiento', (
  SELECT count(*) = 4 FROM ifpe.transacciones t JOIN mercado.ordenes o ON o.id = t.ref_movimiento_espejo
   WHERE o.tx_ifpe_id = t.id AND o.monto = t.monto)
  AND (SELECT count(*) FROM ifpe.transacciones WHERE tipo IN ('COMPRA_MERCADO', 'VENTA_MERCADO')) = 4);

\o
\echo 'tests/db/0013_mercado_valores.sql: todas las pruebas pasaron (ROLLBACK)'
ROLLBACK;
