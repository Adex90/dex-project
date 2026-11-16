-- =============================================================================
--  Pruebas de la migración 0016 (cuentas vinculadas, la lista blanca de
--  retiros). Corren en una transacción que termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0016_cuentas_vinculadas.sql
--
--  Lo que la base de datos garantiza por sí sola: una cuenta se vincula solo a
--  nombre del cliente (sin importar acentos, mayúsculas ni signos), solo si el
--  cliente puede operar y nunca con una CLABE de Synthetic Capital; una CLABE
--  externa está activa para un solo cliente; las cuentas vinculadas no se
--  borran ni cambian, solo se dan de baja, y la baja es definitiva; la orden
--  SPEI de un retiro a cuenta propia va a una cuenta vinculada activa de su
--  cliente, con su CLABE y su titular como beneficiario (cortado a los 40
--  caracteres de SPEI), y su vínculo no cambia; sc_cliente solo lee sus
--  cuentas. backend/tests/patrimonio.rs cubre el camino completo desde la API.
-- =============================================================================
\set ON_ERROR_STOP on
\set QUIET on
\o /dev/null
SET client_min_messages = notice;

BEGIN;

-- Las sentencias corren en una subtransacción (bloque EXCEPTION); SET
-- CONSTRAINTS ALL IMMEDIATE dispara también las revisiones diferidas.
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

-- Sin bloque EXCEPTION: las sentencias corren en la transacción principal (el
-- cargo y la orden se revisan contra el mismo xmin).
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

-- Cuántas filas ve sc_cliente con `consulta` como el cliente dado.
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

-- Vincula una cuenta como sc_ifpe (como backend/src/patrimonio).
CREATE FUNCTION pg_temp.vincular(id TEXT, cliente TEXT, clabe TEXT, titular TEXT, origen TEXT DEFAULT 'DEMO')
RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
      'SET LOCAL ROLE sc_ifpe',
      format($q$INSERT INTO ifpe.cuentas_vinculadas (id, cliente_id, clabe, banco, titular, origen)
                VALUES (%L, %L, %L, %L, %L, %L)$q$, id, cliente, clabe, substr(clabe, 1, 3), titular, origen),
      'RESET ROLE']
$$;

-- Un retiro a cuenta propia como lo escribe backend/src/spei/outbound.rs:
-- evaluación PLD, cargo (RETIRO) al disponible y a la reserva, y la orden
-- PENDIENTE ligada a su cuenta vinculada (NULL: sin vínculo).
CREATE FUNCTION pg_temp.retiro(orden TEXT, tx TEXT, cliente TEXT, cuenta TEXT, ordenante_clabe TEXT,
                               vinculada TEXT, clabe TEXT, beneficiario TEXT, monto TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
      'SET LOCAL ROLE sc_ifpe',
      format($q$SELECT ifpe.aml_evaluar(%L, 'SALIDA', 'SPEI_ENVIADO', %s, %L, %L, jsonb_build_object('cuenta', %L))$q$,
             cliente, monto, tx, 'cli:' || orden, clabe),
      format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - %s WHERE id = %L$q$, monto, cuenta),
      format($q$UPDATE ifpe.reserva_custodia SET saldo = saldo - %s WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
             monto),
      format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia,
                                               referencia_externa)
                VALUES (%L, 'RETIRO', %L, 'e0000000-0000-0000-0000-00000000000e', %s, %L, %L)$q$,
             tx, cuenta, monto, 'cli:' || orden, clabe),
      format($q$INSERT INTO ifpe.spei_outbound (id, cliente_id, cuenta_id, clave_idempotencia, clave_rastreo,
                    fecha_solicitud, ordenante_nombre, ordenante_clabe, banco_receptor, beneficiario_clabe,
                    beneficiario_nombre, monto, concepto, referencia_numerica, tx_retiro_id, cuenta_vinculada_id)
                VALUES (%L, %L, %L, %L, %L, current_date, %L, %L, %L, %L, %L, %s, 'RETIRO A CUENTA PROPIA', 1, %L, %L)$q$,
             orden, cliente, cuenta, 'cli:' || orden, 'SC16' || upper(right(replace(orden, '-', ''), 12)),
             beneficiario, ordenante_clabe, substr(clabe, 1, 3), clabe, beneficiario, monto, tx, vinculada),
      'RESET ROLE']
$$;

-- -----------------------------------------------------------------------------
-- Datos: Ana, Beto, José María (con acentos y ñ), Juan Carlos (un nombre de
-- más de 40 caracteres) y otra Ana Prueba (homónima), verificados; Carla sin
-- KYC. La reserva y 5 000 para cada cliente verificado.
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, apellido_materno, email, estatus_kyc,
                          kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ana', 'Prueba', NULL, 'ana@example.mx', 'VERIFICADO', now(), 'BAJO'),
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Beto', 'Prueba', NULL, 'beto@example.mx', 'VERIFICADO', now(), 'BAJO'),
  ('d0000000-0000-0000-0000-00000000000d', 'FISICA', 'José María', 'Núñez', 'Ibáñez', 'jose@example.mx', 'VERIFICADO',
   now(), 'BAJO'),
  ('5a000000-0000-0000-0000-00000000005a', 'FISICA', 'Juan Carlos de la Santísima Trinidad', 'Pérez', 'López',
   'juan@example.mx', 'VERIFICADO', now(), 'BAJO'),
  ('a2000000-0000-0000-0000-0000000000a2', 'FISICA', 'Ana', 'Prueba', NULL, 'ana.dos@example.mx', 'VERIFICADO', now(),
   'BAJO');
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, nivel_riesgo_pld) VALUES
  ('c0000000-0000-0000-0000-00000000000c', 'FISICA', 'Carla', 'Prueba', 'carla@example.mx', 'BAJO');
INSERT INTO ifpe.reserva_custodia (id, institucion, tipo, numero_cuenta) VALUES
  ('e0000000-0000-0000-0000-00000000000e', 'Banco de México', 'BANXICO', 'SC-RES-001');
INSERT INTO ifpe.cuentas (id, cliente_id, clabe) VALUES
  ('1a000000-0000-0000-0000-00000000001a', 'a0000000-0000-0000-0000-00000000000a', '646180157000000017'),
  ('1b000000-0000-0000-0000-00000000001b', 'b0000000-0000-0000-0000-00000000000b', '646180157000000020'),
  ('1c000000-0000-0000-0000-00000000001c', 'c0000000-0000-0000-0000-00000000000c', '646180157000000033'),
  ('1d000000-0000-0000-0000-00000000001d', 'd0000000-0000-0000-0000-00000000000d', '646180157000000046'),
  ('15000000-0000-0000-0000-00000000005a', '5a000000-0000-0000-0000-00000000005a', '646180157000000059');

SELECT pg_temp.debe_pasar('depósitos SPEI: 5 000 para Ana, Beto, José María y Juan Carlos, como sc_spei', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 5000
      WHERE id IN ('1a000000-0000-0000-0000-00000000001a', '1b000000-0000-0000-0000-00000000001b',
                   '1d000000-0000-0000-0000-00000000001d', '15000000-0000-0000-0000-00000000005a')$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 20000 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
     SELECT ('7d000000-0000-0000-0000-00000000000' || n)::uuid, 'DEPOSITO', cuenta::uuid,
            'e0000000-0000-0000-0000-00000000000e', 5000, 'spei:40012:20261101:DEP' || n, '40012:20261101:DEP' || n
       FROM (VALUES (1, '1a000000-0000-0000-0000-00000000001a'), (2, '1b000000-0000-0000-0000-00000000001b'),
                    (3, '1d000000-0000-0000-0000-00000000001d'), (4, '15000000-0000-0000-0000-00000000005a')) d (n, cuenta)$q$,
  $q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                    estado, transaccion_id, origen, raw_payload)
     SELECT '40012', 'DEP' || n, '2026-11-01', c.clabe, c.id, 5000, 'ABONADO',
            ('7d000000-0000-0000-0000-00000000000' || n)::uuid, 'WEBHOOK', '{}'
       FROM (VALUES (1, '1a000000-0000-0000-0000-00000000001a'), (2, '1b000000-0000-0000-0000-00000000001b'),
                    (3, '1d000000-0000-0000-0000-00000000001d'), (4, '15000000-0000-0000-0000-00000000005a')) d (n, cuenta)
       JOIN ifpe.cuentas c ON c.id = d.cuenta::uuid$q$,
  'RESET ROLE'
]);

-- -----------------------------------------------------------------------------
-- Estructura, privilegios y RLS
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('el nombre comparable: mayúsculas, sin acentos, ñ ni signos, espacios simples',
  ifpe.nombre_comparable('  José-María  Núñez o''Brien, Ibáñez  ') = 'JOSE MARIA NUNEZ O BRIEN IBANEZ'
  AND ifpe.nombre_comparable('ÁÉÍÓÚ àèìòù ÄËÏÖÜ âêîôû Ññ') = 'AEIOU AEIOU AEIOU AEIOU NN'
  AND ifpe.nombre_comparable('...') = '');

SELECT pg_temp.exigir('RLS en cuentas vinculadas, con la política RESTRICTIVE de sc_cliente (0011)', (
  SELECT relrowsecurity FROM pg_class WHERE oid = 'ifpe.cuentas_vinculadas'::regclass)
  AND (SELECT count(*) = 1 FROM pg_policies
        WHERE schemaname = 'ifpe' AND tablename = 'cuentas_vinculadas' AND policyname = 'solo_del_cliente'
          AND permissive = 'RESTRICTIVE' AND cmd = 'ALL' AND roles = '{sc_cliente}'));

SELECT pg_temp.exigir('sc_cliente solo lee sus cuentas vinculadas', (
  SELECT array_agg(privilege_type::text ORDER BY privilege_type) = ARRAY['SELECT']
    FROM information_schema.role_table_grants
   WHERE grantee = 'sc_cliente' AND table_schema = 'ifpe' AND table_name = 'cuentas_vinculadas'));

SELECT pg_temp.exigir('sc_ifpe vincula (sin elegir estado ni fechas), da de baja y liga órdenes; nadie borra',
  has_table_privilege('sc_ifpe', 'ifpe.cuentas_vinculadas', 'SELECT')
  AND has_column_privilege('sc_ifpe', 'ifpe.cuentas_vinculadas', 'titular', 'INSERT')
  AND NOT has_column_privilege('sc_ifpe', 'ifpe.cuentas_vinculadas', 'estado', 'INSERT')
  AND NOT has_column_privilege('sc_ifpe', 'ifpe.cuentas_vinculadas', 'creado_en', 'INSERT')
  AND NOT has_column_privilege('sc_ifpe', 'ifpe.cuentas_vinculadas', 'baja_en', 'INSERT')
  AND has_column_privilege('sc_ifpe', 'ifpe.cuentas_vinculadas', 'estado', 'UPDATE')
  AND NOT has_column_privilege('sc_ifpe', 'ifpe.cuentas_vinculadas', 'titular', 'UPDATE')
  AND NOT has_column_privilege('sc_ifpe', 'ifpe.cuentas_vinculadas', 'clabe', 'UPDATE')
  AND NOT has_column_privilege('sc_ifpe', 'ifpe.cuentas_vinculadas', 'baja_en', 'UPDATE')
  AND has_column_privilege('sc_ifpe', 'ifpe.spei_outbound', 'cuenta_vinculada_id', 'INSERT')
  AND NOT has_column_privilege('sc_ifpe', 'ifpe.spei_outbound', 'cuenta_vinculada_id', 'UPDATE')
  AND NOT has_column_privilege('sc_spei', 'ifpe.spei_outbound', 'cuenta_vinculada_id', 'UPDATE')
  AND NOT has_any_column_privilege('sc_spei', 'ifpe.cuentas_vinculadas', 'INSERT')
  AND NOT has_any_column_privilege('sc_spei', 'ifpe.cuentas_vinculadas', 'UPDATE')
  AND NOT EXISTS (SELECT 1 FROM pg_roles r WHERE r.rolname LIKE 'sc\_%'
                     AND has_table_privilege(r.rolname, 'ifpe.cuentas_vinculadas', 'DELETE')));

-- -----------------------------------------------------------------------------
-- Alta: titular, KYC, CLABE externa y única
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana vincula su BBVA a nombre de "ANA PRUEBA" (mayúsculas)', pg_temp.vincular(
  'ca000000-0000-0000-0000-0000000000a1', 'a0000000-0000-0000-0000-00000000000a', '012180001234567899', 'ANA PRUEBA'));

SELECT pg_temp.exigir('la cuenta nace activa, sin baja, con el banco de su CLABE', (
  SELECT estado = 'ACTIVA' AND baja_en IS NULL AND banco = '012' AND origen = 'DEMO' AND creado_en = now()
    FROM ifpe.cuentas_vinculadas WHERE id = 'ca000000-0000-0000-0000-0000000000a1'));

SELECT pg_temp.debe_pasar('José María vincula a nombre de "JOSE MARIA NUNEZ IBANEZ" (sin acentos ni ñ)',
  pg_temp.vincular('cd000000-0000-0000-0000-0000000000d1', 'd0000000-0000-0000-0000-00000000000d',
                   '012180005555555557', 'JOSE MARIA NUNEZ IBANEZ', 'VERIFICACION'));

SELECT pg_temp.debe_pasar('Juan Carlos vincula su Banorte con su nombre completo (47 caracteres)',
  pg_temp.vincular('c5000000-0000-0000-0000-00000000005a', '5a000000-0000-0000-0000-00000000005a',
                   '072180001111111117', 'Juan Carlos de la Santísima Trinidad Pérez López'));

SELECT pg_temp.debe_fallar('el titular no es el cliente (Beto vincula a nombre de Ana)', pg_temp.vincular(
  'cb000000-0000-0000-0000-0000000000b9', 'b0000000-0000-0000-0000-00000000000b', '012180009999999995', 'Ana Prueba'),
  'SC_CUENTA_VINCULADA_TITULAR');

SELECT pg_temp.debe_fallar('el titular es solo parte del nombre ("JOSE MARIA NUNEZ")', pg_temp.vincular(
  'cd000000-0000-0000-0000-0000000000d9', 'd0000000-0000-0000-0000-00000000000d', '012180007777777771',
  'JOSE MARIA NUNEZ'), 'SC_CUENTA_VINCULADA_TITULAR');

SELECT pg_temp.debe_fallar('un cliente sin KYC verificado no vincula cuentas (Carla)', pg_temp.vincular(
  'cc000000-0000-0000-0000-0000000000c9', 'c0000000-0000-0000-0000-00000000000c', '012180007777777771', 'Carla Prueba'),
  'SC_CUENTA_VINCULADA_KYC');

SELECT pg_temp.debe_fallar('una CLABE de Synthetic Capital no es una cuenta externa (la de Beto, para Ana)',
  pg_temp.vincular('ca000000-0000-0000-0000-0000000000a9', 'a0000000-0000-0000-0000-00000000000a',
                   '646180157000000020', 'Ana Prueba'),
  'SC_CUENTA_VINCULADA_PROPIA');

SELECT pg_temp.debe_fallar('una CLABE con el dígito verificador equivocado', pg_temp.vincular(
  'ca000000-0000-0000-0000-0000000000a9', 'a0000000-0000-0000-0000-00000000000a', '012180001234567898', 'Ana Prueba'),
  'cuentas_vinculadas_clabe_check');

SELECT pg_temp.debe_fallar('el banco no es el de la CLABE', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$INSERT INTO ifpe.cuentas_vinculadas (cliente_id, clabe, banco, titular, origen)
     VALUES ('a0000000-0000-0000-0000-00000000000a', '012180007777777771', '014', 'Ana Prueba', 'DEMO')$q$],
  'ck_cuentas_vinculadas_banco');

SELECT pg_temp.debe_fallar('un origen fuera del catálogo', pg_temp.vincular(
  'ca000000-0000-0000-0000-0000000000a9', 'a0000000-0000-0000-0000-00000000000a', '012180007777777771', 'Ana Prueba',
  'MANUAL'), 'cuentas_vinculadas_origen_check');

SELECT pg_temp.debe_fallar('la misma CLABE activa dos veces para Ana', pg_temp.vincular(
  'ca000000-0000-0000-0000-0000000000a9', 'a0000000-0000-0000-0000-00000000000a', '012180001234567899', 'Ana Prueba'),
  'ux_cuentas_vinculadas_activa');

SELECT pg_temp.debe_fallar('la CLABE de Ana para otra Ana Prueba (homónima): una cuenta de retiro, un cliente',
  pg_temp.vincular('ca200000-0000-0000-0000-0000000000a2', 'a2000000-0000-0000-0000-0000000000a2',
                   '012180001234567899', 'Ana Prueba'),
  'ux_cuentas_vinculadas_activa');

SELECT pg_temp.debe_fallar('sc_ifpe no vincula una cuenta ya dada de baja', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$INSERT INTO ifpe.cuentas_vinculadas (cliente_id, clabe, banco, titular, origen, estado)
     VALUES ('a0000000-0000-0000-0000-00000000000a', '012180007777777771', '012', 'Ana Prueba', 'DEMO', 'BAJA')$q$],
  'permission denied');

SELECT pg_temp.debe_fallar('el dueño del esquema tampoco vincula una cuenta en BAJA', ARRAY[
  $q$INSERT INTO ifpe.cuentas_vinculadas (cliente_id, clabe, banco, titular, origen, estado, baja_en)
     VALUES ('a0000000-0000-0000-0000-00000000000a', '012180007777777771', '012', 'Ana Prueba', 'DEMO', 'BAJA', now())$q$],
  'SC_CUENTA_VINCULADA: una cuenta se vincula activa');

-- -----------------------------------------------------------------------------
-- Inmutabilidad y baja
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('el titular no cambia (ni como dueño del esquema)', ARRAY[
  $q$UPDATE ifpe.cuentas_vinculadas SET titular = 'Beto Prueba' WHERE id = 'ca000000-0000-0000-0000-0000000000a1'$q$],
  'SC_CUENTA_VINCULADA_INMUTABLE');

SELECT pg_temp.debe_fallar('la CLABE no cambia', ARRAY[
  $q$UPDATE ifpe.cuentas_vinculadas SET clabe = '012180007777777771' WHERE id = 'ca000000-0000-0000-0000-0000000000a1'$q$],
  'SC_CUENTA_VINCULADA_INMUTABLE');

SELECT pg_temp.debe_fallar('la cuenta no pasa a otro cliente', ARRAY[
  $q$UPDATE ifpe.cuentas_vinculadas SET cliente_id = 'b0000000-0000-0000-0000-00000000000b'
      WHERE id = 'ca000000-0000-0000-0000-0000000000a1'$q$],
  'SC_CUENTA_VINCULADA_INMUTABLE');

SELECT pg_temp.debe_fallar('sc_ifpe no cambia el titular (sin privilegio)', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$UPDATE ifpe.cuentas_vinculadas SET titular = 'Beto Prueba' WHERE id = 'ca000000-0000-0000-0000-0000000000a1'$q$],
  'permission denied');

SELECT pg_temp.debe_fallar('una cuenta vinculada no se borra', ARRAY[
  $q$DELETE FROM ifpe.cuentas_vinculadas WHERE id = 'ca000000-0000-0000-0000-0000000000a1'$q$],
  'SC_CUENTA_VINCULADA_INMUTABLE');

-- Sin CASCADE la detiene antes la llave foránea de las órdenes; con CASCADE,
-- su propio trigger (el primero que corre: el de la tabla nombrada).
SELECT pg_temp.debe_fallar('ni se vacía la tabla', ARRAY['TRUNCATE ifpe.cuentas_vinculadas CASCADE'],
  'SC_LEDGER_INMUTABLE: TRUNCATE sobre ifpe.cuentas_vinculadas');

-- -----------------------------------------------------------------------------
-- La orden de un retiro a cuenta propia y su cuenta vinculada
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana retira 1 000 a su BBVA vinculada, a su nombre', pg_temp.retiro(
  '0a000000-0000-0000-0000-000000000001', '7a000000-0000-0000-0000-000000000001',
  'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a', '646180157000000017',
  'ca000000-0000-0000-0000-0000000000a1', '012180001234567899', 'Ana Prueba', '1000'));

SELECT pg_temp.exigir('el retiro descontó el disponible y la orden quedó PENDIENTE con su vínculo', (
  SELECT saldo_disponible = 4000 FROM ifpe.cuentas WHERE id = '1a000000-0000-0000-0000-00000000001a')
  AND (SELECT estado = 'PENDIENTE' AND cuenta_vinculada_id = 'ca000000-0000-0000-0000-0000000000a1'
         FROM ifpe.spei_outbound WHERE id = '0a000000-0000-0000-0000-000000000001'));

SELECT pg_temp.debe_pasar('José María retira a "Jose Maria Nunez Ibanez" (el nombre SPEI, sin acentos)',
  pg_temp.retiro('0a000000-0000-0000-0000-000000000002', '7a000000-0000-0000-0000-000000000002',
                 'd0000000-0000-0000-0000-00000000000d', '1d000000-0000-0000-0000-00000000001d', '646180157000000046',
                 'cd000000-0000-0000-0000-0000000000d1', '012180005555555557', 'Jose Maria Nunez Ibanez', '250'));

SELECT pg_temp.debe_pasar('Juan Carlos retira con su nombre cortado a los 40 caracteres de SPEI',
  pg_temp.retiro('0a000000-0000-0000-0000-000000000003', '7a000000-0000-0000-0000-000000000003',
                 '5a000000-0000-0000-0000-00000000005a', '15000000-0000-0000-0000-00000000005a', '646180157000000059',
                 'c5000000-0000-0000-0000-00000000005a', '072180001111111117',
                 'Juan Carlos de la Santisima Trinidad Per', '300'));

SELECT pg_temp.debe_fallar('un nombre corto que solo es el inicio del titular no es el titular',
  pg_temp.retiro('0a000000-0000-0000-0000-000000000004', '7a000000-0000-0000-0000-000000000004',
                 '5a000000-0000-0000-0000-00000000005a', '15000000-0000-0000-0000-00000000005a', '646180157000000059',
                 'c5000000-0000-0000-0000-00000000005a', '072180001111111117', 'Juan Carlos', '300'),
  'SC_SPEI_CUENTA_VINCULADA');

SELECT pg_temp.debe_fallar('el beneficiario no es el titular de la cuenta vinculada',
  pg_temp.retiro('0a000000-0000-0000-0000-000000000005', '7a000000-0000-0000-0000-000000000005',
                 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a', '646180157000000017',
                 'ca000000-0000-0000-0000-0000000000a1', '012180001234567899', 'Beto Prueba', '100'),
  'SC_SPEI_CUENTA_VINCULADA');

SELECT pg_temp.debe_fallar('la orden va a otra CLABE que la de su cuenta vinculada',
  pg_temp.retiro('0a000000-0000-0000-0000-000000000006', '7a000000-0000-0000-0000-000000000006',
                 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a', '646180157000000017',
                 'ca000000-0000-0000-0000-0000000000a1', '012180007777777771', 'Ana Prueba', '100'),
  'SC_SPEI_CUENTA_VINCULADA');

SELECT pg_temp.debe_fallar('Beto no retira a la cuenta vinculada de Ana',
  pg_temp.retiro('0a000000-0000-0000-0000-000000000007', '7a000000-0000-0000-0000-000000000007',
                 'b0000000-0000-0000-0000-00000000000b', '1b000000-0000-0000-0000-00000000001b', '646180157000000020',
                 'ca000000-0000-0000-0000-0000000000a1', '012180001234567899', 'Ana Prueba', '100'),
  'SC_SPEI_CUENTA_VINCULADA');

SELECT pg_temp.debe_fallar('una cuenta vinculada que no existe',
  pg_temp.retiro('0a000000-0000-0000-0000-000000000008', '7a000000-0000-0000-0000-000000000008',
                 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a', '646180157000000017',
                 'ca000000-0000-0000-0000-00000000ffff', '012180001234567899', 'Ana Prueba', '100'),
  'SC_SPEI_CUENTA_VINCULADA');

SELECT pg_temp.debe_fallar('el vínculo de una orden no cambia (ni como dueño del esquema)', ARRAY[
  $q$UPDATE ifpe.spei_outbound SET cuenta_vinculada_id = 'cd000000-0000-0000-0000-0000000000d1'
      WHERE id = '0a000000-0000-0000-0000-000000000001'$q$],
  'SC_SPEI_INMUTABLE');

SELECT pg_temp.debe_fallar('ni se quita', ARRAY[
  $q$UPDATE ifpe.spei_outbound SET cuenta_vinculada_id = NULL WHERE id = '0a000000-0000-0000-0000-000000000001'$q$],
  'SC_SPEI_INMUTABLE');

SELECT pg_temp.debe_pasar('el despachador (sc_spei) sigue llevando la orden: ENVIADA y LIQUIDADA', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$UPDATE ifpe.spei_outbound SET estado = 'ENVIADA', enviada_en = now() WHERE id = '0a000000-0000-0000-0000-000000000001'$q$,
  $q$UPDATE ifpe.spei_outbound SET estado = 'LIQUIDADA', liquidada_en = now(), finalizada_en = now()
      WHERE id = '0a000000-0000-0000-0000-000000000001'$q$,
  'RESET ROLE']);

-- -----------------------------------------------------------------------------
-- Baja: definitiva, y la CLABE se puede vincular de nuevo
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('sc_ifpe da de baja la cuenta de Ana', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$UPDATE ifpe.cuentas_vinculadas SET estado = 'BAJA' WHERE id = 'ca000000-0000-0000-0000-0000000000a1'$q$,
  'RESET ROLE']);

SELECT pg_temp.exigir('la baja fija su fecha', (
  SELECT estado = 'BAJA' AND baja_en = now() FROM ifpe.cuentas_vinculadas WHERE id = 'ca000000-0000-0000-0000-0000000000a1'));

SELECT pg_temp.debe_fallar('una cuenta dada de baja no recibe retiros',
  pg_temp.retiro('0a000000-0000-0000-0000-000000000009', '7a000000-0000-0000-0000-000000000009',
                 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a', '646180157000000017',
                 'ca000000-0000-0000-0000-0000000000a1', '012180001234567899', 'Ana Prueba', '100'),
  'SC_SPEI_CUENTA_VINCULADA');

SELECT pg_temp.debe_fallar('la baja es definitiva (no se reactiva)', ARRAY[
  $q$UPDATE ifpe.cuentas_vinculadas SET estado = 'ACTIVA', baja_en = NULL WHERE id = 'ca000000-0000-0000-0000-0000000000a1'$q$],
  'SC_CUENTA_VINCULADA_INMUTABLE');

SELECT pg_temp.debe_fallar('ni cambia su fecha', ARRAY[
  $q$UPDATE ifpe.cuentas_vinculadas SET baja_en = now() - interval '1 day'
      WHERE id = 'ca000000-0000-0000-0000-0000000000a1'$q$],
  'SC_CUENTA_VINCULADA_INMUTABLE');

SELECT pg_temp.debe_pasar('después de la baja, la otra Ana Prueba vincula esa CLABE', pg_temp.vincular(
  'ca200000-0000-0000-0000-0000000000a2', 'a2000000-0000-0000-0000-0000000000a2', '012180001234567899', 'Ana Prueba'));

SELECT pg_temp.exigir('la orden liquidada de Ana conserva su vínculo a la cuenta dada de baja', (
  SELECT estado = 'LIQUIDADA' AND cuenta_vinculada_id = 'ca000000-0000-0000-0000-0000000000a1'
    FROM ifpe.spei_outbound WHERE id = '0a000000-0000-0000-0000-000000000001'));

SELECT pg_temp.debe_pasar('una orden sin vínculo (pago a un tercero) no se revisa contra la lista blanca',
  pg_temp.retiro('0a000000-0000-0000-0000-00000000000a', '7a000000-0000-0000-0000-00000000000a',
                 'b0000000-0000-0000-0000-00000000000b', '1b000000-0000-0000-0000-00000000001b', '646180157000000020',
                 NULL, '012180007777777771', 'Proveedor SA de CV', '100'));

-- -----------------------------------------------------------------------------
-- sc_cliente: solo sus cuentas
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('Ana ve su cuenta (dada de baja) y ninguna otra; José María, la suya; Carla, ninguna',
  pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM ifpe.cuentas_vinculadas') = 1
  AND pg_temp.visibles('a0000000-0000-0000-0000-00000000000a',
                       $q$SELECT count(*) FROM ifpe.cuentas_vinculadas WHERE cliente_id <> 'a0000000-0000-0000-0000-00000000000a'$q$) = 0
  AND pg_temp.visibles('d0000000-0000-0000-0000-00000000000d', 'SELECT count(*) FROM ifpe.cuentas_vinculadas') = 1
  AND pg_temp.visibles('c0000000-0000-0000-0000-00000000000c', 'SELECT count(*) FROM ifpe.cuentas_vinculadas') = 0);

SELECT pg_temp.exigir('sin cliente, sc_cliente no ve ninguna (falla cerrado)',
  pg_temp.visibles('', 'SELECT count(*) FROM ifpe.cuentas_vinculadas') = 0);

SELECT pg_temp.debe_fallar('sc_cliente no vincula cuentas', ARRAY[
  $q$SELECT set_config('sc.cliente_actual', 'a0000000-0000-0000-0000-00000000000a', true)$q$,
  'SET LOCAL ROLE sc_cliente',
  $q$INSERT INTO ifpe.cuentas_vinculadas (cliente_id, clabe, banco, titular, origen)
     VALUES ('a0000000-0000-0000-0000-00000000000a', '012180007777777771', '012', 'Ana Prueba', 'DEMO')$q$],
  'permission denied');

SELECT pg_temp.debe_fallar('ni da de baja las suyas', ARRAY[
  $q$SELECT set_config('sc.cliente_actual', 'd0000000-0000-0000-0000-00000000000d', true)$q$,
  'SET LOCAL ROLE sc_cliente',
  $q$UPDATE ifpe.cuentas_vinculadas SET estado = 'BAJA' WHERE id = 'cd000000-0000-0000-0000-0000000000d1'$q$],
  'permission denied');

\o
\echo 'tests/db/0016_cuentas_vinculadas.sql: todas las pruebas pasaron (ROLLBACK)'
ROLLBACK;
