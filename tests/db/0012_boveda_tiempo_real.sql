-- =============================================================================
--  Pruebas de la migración 0012 (Bóveda Soberana en tiempo real). Corren en una
--  transacción que termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0012_boveda_tiempo_real.sql
--
--  Lo que la base de datos garantiza por sí sola: la versión de una cuenta
--  (seq) sube una vez por transacción que cambia sus saldos, y nadie la fija
--  ni la reescribe; un SPEI acreditado y un reembolso llevan la transacción
--  que movió el dinero. Los avisos (NOTIFY) solo se entregan al confirmar:
--  esta suite, que termina en ROLLBACK, comprueba los triggers que los emiten,
--  y backend/tests/boveda.rs su entrega.
-- =============================================================================
\set ON_ERROR_STOP on
\set QUIET on
\o /dev/null
SET client_min_messages = notice;

BEGIN;

CREATE FUNCTION pg_temp.debe_fallar(prueba TEXT, sentencias TEXT[], patron TEXT) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    s TEXT;
BEGIN
    BEGIN
        FOREACH s IN ARRAY sentencias LOOP
            EXECUTE s;
        END LOOP;
    EXCEPTION WHEN OTHERS THEN
        IF strpos(SQLERRM, patron) = 0 THEN
            RAISE EXCEPTION 'FALLO [%]: error inesperado "%" (se esperaba "%")', prueba, SQLERRM, patron;
        END IF;
        RAISE NOTICE 'OK  rechazado  %', prueba;
        RETURN;
    END;
    RAISE EXCEPTION 'FALLO [%]: se esperaba "%" pero las sentencias se aceptaron', prueba, patron;
END
$$;

CREATE FUNCTION pg_temp.debe_pasar(prueba TEXT, sentencias TEXT[]) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    s TEXT;
BEGIN
    FOREACH s IN ARRAY sentencias LOOP
        EXECUTE s;
    END LOOP;
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

CREATE FUNCTION pg_temp.seq(cuenta UUID) RETURNS BIGINT
LANGUAGE sql AS $$ SELECT seq FROM ifpe.cuentas WHERE id = cuenta $$;

CREATE FUNCTION pg_temp.seq_xact(cuenta UUID) RETURNS xid8
LANGUAGE sql AS $$ SELECT seq_xact FROM ifpe.cuentas WHERE id = cuenta $$;

-- -----------------------------------------------------------------------------
-- Datos: Ana y Beto con su cuenta; la reserva en custodia
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ana',   'Prueba', 'ana@example.mx',   'VERIFICADO', now(), 'BAJO'),
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Beto',  'Prueba', 'beto@example.mx',  'VERIFICADO', now(), 'BAJO'),
  ('c0000000-0000-0000-0000-00000000000c', 'FISICA', 'Carla', 'Prueba', 'carla@example.mx', 'VERIFICADO', now(), 'BAJO');
INSERT INTO ifpe.reserva_custodia (id, institucion, tipo, numero_cuenta) VALUES
  ('e0000000-0000-0000-0000-00000000000e', 'Banco de México', 'BANXICO', 'SC-RES-001');
INSERT INTO ifpe.cuentas (id, cliente_id, clabe) VALUES
  ('1a000000-0000-0000-0000-00000000001a', 'a0000000-0000-0000-0000-00000000000a', '646180157000000017'),
  ('1b000000-0000-0000-0000-00000000001b', 'b0000000-0000-0000-0000-00000000000b', '646180157000000020');

-- -----------------------------------------------------------------------------
-- Estructura
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('ifpe.cuentas: seq BIGINT NOT NULL y seq_xact xid8', (
  SELECT count(*) = 2 FROM information_schema.columns
   WHERE table_schema = 'ifpe' AND table_name = 'cuentas'
     AND ((column_name = 'seq' AND data_type = 'bigint' AND is_nullable = 'NO')
       OR (column_name = 'seq_xact' AND udt_name = 'xid8'))));

SELECT pg_temp.exigir('los cinco triggers de la migración existen y están activos', (
  SELECT count(*) = 5 FROM pg_trigger
   WHERE NOT tgisinternal AND tgenabled = 'O' AND tgname IN (
     'trg_ifpe_cuentas_seq', 'trg_ifpe_cuentas_seq_aviso', 'trg_spei_inbound_xact',
     'trg_spei_outbound_xact', 'trg_ifpe_cortes_reserva_aviso')));

SELECT pg_temp.exigir('los avisos van al canal sc_boveda', (
  SELECT bool_and(prosrc LIKE '%pg_notify(''sc_boveda''%') FROM pg_proc
   WHERE proname IN ('fn_cuenta_avisar', 'fn_corte_avisar')
     AND pronamespace = 'ifpe'::regnamespace));

-- -----------------------------------------------------------------------------
-- La versión nace en 0 y nadie la fija
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('una cuenta nace en la versión 0',
  pg_temp.seq('1a000000-0000-0000-0000-00000000001a') = 0
  AND pg_temp.seq_xact('1a000000-0000-0000-0000-00000000001a') IS NULL);

SELECT pg_temp.debe_pasar('el dueño crea una cuenta pidiendo seq 99', ARRAY[
  $q$INSERT INTO ifpe.cuentas (id, cliente_id, clabe, seq, seq_xact)
     VALUES ('1c000000-0000-0000-0000-00000000001c', 'c0000000-0000-0000-0000-00000000000c',
             '646180157000000033', 99, '5'::xid8)$q$
]);
SELECT pg_temp.exigir('… y nace igual en la versión 0',
  pg_temp.seq('1c000000-0000-0000-0000-00000000001c') = 0
  AND pg_temp.seq_xact('1c000000-0000-0000-0000-00000000001c') IS NULL);

-- -----------------------------------------------------------------------------
-- Un SPEI acreditado: la versión sube y el SPEI lleva su transacción
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('depósito SPEI de Ana (1000), como sc_spei', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 1000 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 1000 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
     VALUES ('7d000000-0000-0000-0000-000000000001', 'DEPOSITO', '1a000000-0000-0000-0000-00000000001a',
             'e0000000-0000-0000-0000-00000000000e', 1000, 'spei:40012:20261101:BOV0001', '40012:20261101:BOV0001')$q$,
  $q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                    estado, transaccion_id, origen, raw_payload)
     VALUES ('40012', 'BOV0001', '2026-11-01', '646180157000000017', '1a000000-0000-0000-0000-00000000001a', 1000,
             'ABONADO', '7d000000-0000-0000-0000-000000000001', 'WEBHOOK', '{}')$q$,
  'RESET ROLE'
]);

SELECT pg_temp.exigir('el depósito sube la versión de Ana a 1',
  pg_temp.seq('1a000000-0000-0000-0000-00000000001a') = 1);
SELECT pg_temp.exigir('… sellada con esta transacción',
  pg_temp.seq_xact('1a000000-0000-0000-0000-00000000001a') = pg_current_xact_id());
SELECT pg_temp.exigir('el SPEI acreditado lleva la misma transacción que la versión', (
  SELECT xact = pg_temp.seq_xact('1a000000-0000-0000-0000-00000000001a')
    FROM ifpe.spei_inbound WHERE clave_rastreo = 'BOV0001'));

-- -----------------------------------------------------------------------------
-- Una transacción, una versión
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('en la misma transacción, Ana transfiere 100 a Beto (sc_ifpe)', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 100 WHERE id = '1b000000-0000-0000-0000-00000000001b'$q$,
  $q$INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia)
     VALUES ('TRANSFERENCIA', '1a000000-0000-0000-0000-00000000001a', '1b000000-0000-0000-0000-00000000001b', 100, 'bov-1')$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('Ana sigue en la versión 1: su transacción ya la contó',
  pg_temp.seq('1a000000-0000-0000-0000-00000000001a') = 1);
SELECT pg_temp.exigir('Beto, que cambia por primera vez, pasa a la versión 1',
  pg_temp.seq('1b000000-0000-0000-0000-00000000001b') = 1);

SELECT pg_temp.debe_pasar('UPDATE que no cambia el saldo de Carla', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible WHERE id = '1c000000-0000-0000-0000-00000000001c'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('… no sube su versión', pg_temp.seq('1c000000-0000-0000-0000-00000000001c') = 0);

-- -----------------------------------------------------------------------------
-- Nadie reescribe la versión
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('el dueño del esquema intenta fijar seq y seq_xact', ARRAY[
  $q$UPDATE ifpe.cuentas SET seq = 42, seq_xact = '7'::xid8 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$
]);
SELECT pg_temp.exigir('… y no cambian',
  pg_temp.seq('1a000000-0000-0000-0000-00000000001a') = 1
  AND pg_temp.seq_xact('1a000000-0000-0000-0000-00000000001a') = pg_current_xact_id());

-- Que cada transacción nueva vuelve a subirla (1, 2, 3…) se prueba en
-- backend/tests/boveda.rs: esta suite vive dentro de una sola transacción.

-- -----------------------------------------------------------------------------
-- SPEI salientes: el reembolso queda sellado; la orden, no
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('orden SPEI de Ana por 200 (RETIRO + PENDIENTE), como sc_ifpe', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 200 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo - 200 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia, referencia_externa)
     VALUES ('7d000000-0000-0000-0000-000000000020', 'RETIRO', '1a000000-0000-0000-0000-00000000001a',
             'e0000000-0000-0000-0000-00000000000e', 200, 'cli:BOVORDEN01', '012180001234567899')$q$,
  $q$INSERT INTO ifpe.spei_outbound (id, cliente_id, cuenta_id, clave_idempotencia, clave_rastreo, fecha_solicitud,
                                     ordenante_nombre, ordenante_clabe, banco_receptor, beneficiario_clabe,
                                     beneficiario_nombre, monto, concepto, referencia_numerica, tx_retiro_id)
     VALUES ('0a000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a',
             '1a000000-0000-0000-0000-00000000001a', 'cli:BOVORDEN01', 'BOVORDEN01', '2026-11-01', 'ANA PRUEBA',
             '646180157000000017', '012', '012180001234567899', 'BENEFICIARIO', 200, 'PAGO', 1,
             '7d000000-0000-0000-0000-000000000020')$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_fallar('sc_ifpe no puede escribir xact_reverso', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$UPDATE ifpe.spei_outbound SET xact_reverso = '8'::xid8 WHERE id = '0a000000-0000-0000-0000-000000000001'$q$
], 'permission denied');
SELECT pg_temp.exigir('una orden sin reembolso no tiene xact_reverso', (
  SELECT xact_reverso IS NULL FROM ifpe.spei_outbound WHERE id = '0a000000-0000-0000-0000-000000000001'));

SELECT pg_temp.debe_pasar('reembolso de la orden (DEPOSITO de reverso + RECHAZADA), como sc_spei', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 200 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 200 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia)
     VALUES ('7d000000-0000-0000-0000-000000000021', 'DEPOSITO', '1a000000-0000-0000-0000-00000000001a',
             'e0000000-0000-0000-0000-00000000000e', 200, 'spei-reverso:0a000000-0000-0000-0000-000000000001')$q$,
  $q$UPDATE ifpe.spei_outbound SET estado = 'RECHAZADA', tx_reverso_id = '7d000000-0000-0000-0000-000000000021',
            causa_devolucion = 99, revision_manual = FALSE, finalizada_en = now()
      WHERE id = '0a000000-0000-0000-0000-000000000001'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('el reembolso queda sellado con su transacción', (
  SELECT xact_reverso = pg_current_xact_id() FROM ifpe.spei_outbound WHERE id = '0a000000-0000-0000-0000-000000000001'));

-- -----------------------------------------------------------------------------
-- spei_inbound: el sello no se elige
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('SPEI devuelto registrado pidiendo xact 1, como el dueño', ARRAY[
  $q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, monto, estado,
                                    causa_devolucion, origen, raw_payload, xact)
     VALUES ('40012', 'BOV0002', '2026-11-01', '999999999999999999', 10, 'DEVUELTO', 1, 'WEBHOOK', '{}', '1'::xid8)$q$
]);
SELECT pg_temp.exigir('… y lleva la transacción que lo registró', (
  SELECT xact = pg_current_xact_id() FROM ifpe.spei_inbound WHERE clave_rastreo = 'BOV0002'));

\o
\echo 'tests/db/0012_boveda_tiempo_real.sql: todas las pruebas pasaron (ROLLBACK)'
ROLLBACK;
