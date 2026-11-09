-- =============================================================================
--  Pruebas de la migración 0011 (Row-Level Security por cliente). Corren en una
--  transacción que termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0011_rls_por_cliente.sql
--
--  Lo que la base de datos garantiza por sí sola, sin importar el backend: con
--  el rol sc_cliente solo se ven las filas del cliente de sc.cliente_actual
--  (usuarios, cuentas, movimientos, SPEI, SOFOM); sin cliente no se ve nada;
--  sc_cliente no escribe ni borra; los demás roles no cambian.
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

-- Filas visibles para sc_cliente a nombre de `cliente` (texto: '' = sin cliente).
CREATE FUNCTION pg_temp.visibles(cliente TEXT, consulta TEXT) RETURNS BIGINT
LANGUAGE plpgsql AS $$
DECLARE
    n BIGINT;
BEGIN
    PERFORM set_config('sc.cliente_actual', cliente, true);
    SET LOCAL ROLE sc_cliente;
    EXECUTE consulta INTO n;
    RESET ROLE;
    PERFORM set_config('sc.cliente_actual', '', true);
    RETURN n;
END
$$;

-- -----------------------------------------------------------------------------
-- Datos: Ana y Beto con depósito cada uno y una transferencia de Ana a Beto
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ana',  'Prueba', 'ana@example.mx',  'VERIFICADO', now(), 'BAJO'),
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Beto', 'Prueba', 'beto@example.mx', 'VERIFICADO', now(), 'BAJO');
INSERT INTO ifpe.reserva_custodia (id, institucion, tipo, numero_cuenta) VALUES
  ('e0000000-0000-0000-0000-00000000000e', 'Banco de México', 'BANXICO', 'SC-RES-001');
INSERT INTO ifpe.cuentas (id, cliente_id, clabe) VALUES
  ('1a000000-0000-0000-0000-00000000001a', 'a0000000-0000-0000-0000-00000000000a', '646180157000000017'),
  ('1b000000-0000-0000-0000-00000000001b', 'b0000000-0000-0000-0000-00000000000b', '646180157000000020');
INSERT INTO sofom.cuentas (id, cliente_id) VALUES
  ('2a000000-0000-0000-0000-00000000002a', 'a0000000-0000-0000-0000-00000000000a');

SELECT pg_temp.debe_pasar('datos: depósitos SPEI de Ana (1000) y Beto (500)', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 1000 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 1000 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
     VALUES ('7d000000-0000-0000-0000-000000000001', 'DEPOSITO', '1a000000-0000-0000-0000-00000000001a',
             'e0000000-0000-0000-0000-00000000000e', 1000, 'spei:40012:20261001:SPEI0001', '40012:20261001:SPEI0001')$q$,
  $q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                    estado, transaccion_id, origen, raw_payload)
     VALUES ('40012', 'SPEI0001', '2026-10-01', '646180157000000017', '1a000000-0000-0000-0000-00000000001a', 1000,
             'ABONADO', '7d000000-0000-0000-0000-000000000001', 'WEBHOOK', '{}')$q$,
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 500 WHERE id = '1b000000-0000-0000-0000-00000000001b'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 500 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
     VALUES ('7d000000-0000-0000-0000-000000000002', 'DEPOSITO', '1b000000-0000-0000-0000-00000000001b',
             'e0000000-0000-0000-0000-00000000000e', 500, 'spei:40012:20261001:SPEI0002', '40012:20261001:SPEI0002')$q$,
  $q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                    estado, transaccion_id, origen, raw_payload)
     VALUES ('40012', 'SPEI0002', '2026-10-01', '646180157000000020', '1b000000-0000-0000-0000-00000000001b', 500,
             'ABONADO', '7d000000-0000-0000-0000-000000000002', 'WEBHOOK', '{}')$q$,
  'RESET ROLE'
]);

SELECT pg_temp.debe_pasar('datos: transferencia de Ana a Beto (100)', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 100 WHERE id = '1b000000-0000-0000-0000-00000000001b'$q$,
  $q$INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia)
     VALUES ('TRANSFERENCIA', '1a000000-0000-0000-0000-00000000001a', '1b000000-0000-0000-0000-00000000001b', 100, 'trf-1')$q$,
  'RESET ROLE'
]);

-- -----------------------------------------------------------------------------
-- Estructura
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('RLS activa en las 8 tablas del cliente', (
  SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE c.relrowsecurity AND format('%s.%s', n.nspname, c.relname) IN (
     'kyc.clientes', 'ifpe.cuentas', 'ifpe.transacciones', 'ifpe.spei_outbound', 'ifpe.spei_inbound',
     'sofom.cuentas', 'sofom.contratos_mutuo', 'sofom.transacciones')) = 8);

-- Las de esta migración; las tablas de migraciones posteriores (mercado, 0013)
-- las revisa su propia suite.
SELECT pg_temp.exigir('política RESTRICTIVE FOR ALL solo para sc_cliente en las 8 tablas', (
  SELECT count(*) FROM pg_policies
   WHERE policyname = 'solo_del_cliente' AND permissive = 'RESTRICTIVE' AND cmd = 'ALL' AND roles = '{sc_cliente}'
     AND schemaname IN ('kyc', 'ifpe', 'sofom')) = 8);

SELECT pg_temp.exigir('sc_cliente: SELECT en las 8 tablas y ningún otro privilegio en ninguna', (
  SELECT count(*) FILTER (WHERE privilege_type = 'SELECT' AND table_schema IN ('kyc', 'ifpe', 'sofom')) = 8
         AND count(*) FILTER (WHERE privilege_type <> 'SELECT') = 0
    FROM information_schema.role_table_grants WHERE grantee = 'sc_cliente'));

SELECT pg_temp.exigir('sc_cliente no inicia sesión ni salta RLS', (
  SELECT NOT rolcanlogin AND NOT rolbypassrls AND NOT rolsuper FROM pg_roles WHERE rolname = 'sc_cliente'));

-- -----------------------------------------------------------------------------
-- Sin cliente: nada (falla cerrado)
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('sin cliente: 0 usuarios, cuentas, movimientos y abonos', (
      pg_temp.visibles('', 'SELECT count(*) FROM kyc.clientes')
    + pg_temp.visibles('', 'SELECT count(*) FROM ifpe.cuentas')
    + pg_temp.visibles('', 'SELECT count(*) FROM ifpe.transacciones')
    + pg_temp.visibles('', 'SELECT count(*) FROM ifpe.spei_inbound')
    + pg_temp.visibles('', 'SELECT count(*) FROM sofom.cuentas')) = 0);

SELECT pg_temp.debe_fallar('un cliente que no es UUID hace fallar la consulta', ARRAY[
  $q$SELECT set_config('sc.cliente_actual', 'todos', true)$q$,
  'SET LOCAL ROLE sc_cliente',
  'SELECT count(*) FROM ifpe.cuentas'
], 'invalid input syntax for type uuid');

-- -----------------------------------------------------------------------------
-- Con cliente: solo lo suyo
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('Ana: su usuario, su cuenta IFPE y su cuenta SOFOM', (
      pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM kyc.clientes') = 1
  AND pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM ifpe.cuentas') = 1
  AND pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM sofom.cuentas') = 1));

SELECT pg_temp.exigir('Ana: su depósito y su transferencia, no el depósito de Beto', (
      pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM ifpe.transacciones') = 2
  AND pg_temp.visibles('a0000000-0000-0000-0000-00000000000a',
        $q$SELECT count(*) FROM ifpe.transacciones WHERE id = '7d000000-0000-0000-0000-000000000002'$q$) = 0
  AND pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM ifpe.spei_inbound') = 1));

SELECT pg_temp.exigir('Beto: su depósito y la transferencia que recibió (cada lado la ve)', (
      pg_temp.visibles('b0000000-0000-0000-0000-00000000000b', 'SELECT count(*) FROM ifpe.transacciones') = 2
  AND pg_temp.visibles('b0000000-0000-0000-0000-00000000000b',
        $q$SELECT count(*) FROM ifpe.transacciones WHERE tipo = 'TRANSFERENCIA'$q$) = 1
  AND pg_temp.visibles('b0000000-0000-0000-0000-00000000000b', 'SELECT count(*) FROM sofom.cuentas') = 0));

SELECT pg_temp.exigir('pedir las filas de otro cliente (IDOR) no devuelve nada', (
      pg_temp.visibles('a0000000-0000-0000-0000-00000000000a',
        $q$SELECT count(*) FROM kyc.clientes WHERE id = 'b0000000-0000-0000-0000-00000000000b'$q$) = 0
  AND pg_temp.visibles('a0000000-0000-0000-0000-00000000000a',
        $q$SELECT count(*) FROM ifpe.cuentas WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b'$q$) = 0));

-- -----------------------------------------------------------------------------
-- sc_cliente no escribe ni borra, ni siquiera sus propias filas
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('sc_cliente no actualiza su propio usuario', ARRAY[
  $q$SELECT set_config('sc.cliente_actual', 'a0000000-0000-0000-0000-00000000000a', true)$q$,
  'SET LOCAL ROLE sc_cliente',
  $q$UPDATE kyc.clientes SET telefono = '5512345678' WHERE id = 'a0000000-0000-0000-0000-00000000000a'$q$
], 'permission denied');

SELECT pg_temp.debe_fallar('sc_cliente no cambia el saldo de su cuenta', ARRAY[
  $q$SELECT set_config('sc.cliente_actual', 'a0000000-0000-0000-0000-00000000000a', true)$q$,
  'SET LOCAL ROLE sc_cliente',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = 1000000 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$
], 'permission denied');

SELECT pg_temp.debe_fallar('sc_cliente no borra sus movimientos', ARRAY[
  $q$SELECT set_config('sc.cliente_actual', 'a0000000-0000-0000-0000-00000000000a', true)$q$,
  'SET LOCAL ROLE sc_cliente',
  $q$DELETE FROM ifpe.transacciones WHERE cuenta_destino_id = '1a000000-0000-0000-0000-00000000001a'$q$
], 'permission denied');

SELECT pg_temp.debe_fallar('sc_cliente no lee credenciales', ARRAY[
  $q$SELECT set_config('sc.cliente_actual', 'a0000000-0000-0000-0000-00000000000a', true)$q$,
  'SET LOCAL ROLE sc_cliente',
  'SELECT 1 FROM kyc.credenciales'
], 'permission denied');

-- -----------------------------------------------------------------------------
-- Los demás roles no cambian, haya o no cliente en la transacción
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('sc_ifpe ve todas las cuentas y movimientos aunque la transacción tenga cliente', ARRAY[
  $q$SELECT set_config('sc.cliente_actual', 'a0000000-0000-0000-0000-00000000000a', true)$q$,
  'SET LOCAL ROLE sc_ifpe',
  $q$DO $d$ BEGIN
       IF (SELECT count(*) FROM ifpe.cuentas) <> 2 OR (SELECT count(*) FROM ifpe.transacciones) <> 3 THEN
         RAISE EXCEPTION 'sc_ifpe dejó de ver todas las filas';
       END IF;
     END $d$$q$,
  'RESET ROLE'
]);

\o
\echo 'tests/db/0011_rls_por_cliente.sql: todas las pruebas pasaron (ROLLBACK)'
ROLLBACK;
