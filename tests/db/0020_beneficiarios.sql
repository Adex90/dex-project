-- =============================================================================
--  Pruebas de la migración 0020 (beneficiarios de la cuenta). Corren en una
--  transacción que termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0020_beneficiarios.sql
--
--  Lo que la base de datos garantiza por sí sola: cada versión de la
--  designación tiene de 0 a 5 beneficiarios en las posiciones 1..n que suman
--  exactamente 100% (o ninguno), identificados por CURP o fecha de nacimiento
--  coherentes, ninguno el titular ni repetido; la versión, la fecha y la
--  cadena de hashes las pone la base de datos, que al COMMIT comprueba que el
--  hash cubre exactamente lo guardado; nada se corrige ni se borra; solo un
--  cliente verificado designa; escribe sc_kyc y el cliente lee lo suyo; cada
--  versión deja su evento en la bitácora de seguridad del cliente, el único
--  que sc_kyc puede escribir allí.
--  backend/tests/beneficiarios.rs cubre la API y la cadena recalculada en Rust.
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

-- Sin bloque EXCEPTION: las sentencias corren en la transacción principal y
-- las comprobaciones diferidas se disparan como al COMMIT.
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

-- Una versión como la guarda el backend (sc_kyc): primero sus beneficiarios
-- (posicion, nombre, parentesco, curp, fecha, porcentaje), después la versión,
-- desde la sesión del cliente con su verificación reforzada vigente (0022;
-- tests/db/0022_verificacion_reforzada.sql prueba esa regla).
CREATE FUNCTION pg_temp.designacion(id TEXT, cliente TEXT, beneficiarios TEXT[], num INTEGER, total TEXT)
RETURNS TEXT[]
LANGUAGE plpgsql AS $$
DECLARE
    sentencias TEXT[] := ARRAY['SET LOCAL ROLE sc_kyc'];
    b          TEXT;
BEGIN
    FOREACH b IN ARRAY beneficiarios LOOP
        sentencias := sentencias || format(
            'INSERT INTO kyc.beneficiarios (designacion_id, posicion, nombre_completo, parentesco, curp, fecha_nacimiento, porcentaje) VALUES (%L, %s)',
            id, b);
    END LOOP;
    RETURN sentencias || format(
        'INSERT INTO kyc.designaciones_beneficiarios (id, cliente_id, num_beneficiarios, total_porcentaje, sesion_id, ip, dispositivo) VALUES (%L, %L, %s, %s, %L, %L, %L)',
        id, cliente, num, total, pg_temp.sesion_de(cliente), '203.0.113.7', 'Chrome on macOS') || 'RESET ROLE'::text;
END
$$;

-- La sesión de cada cliente: 5e…0a para Ana, 5e…0b para Beto, 5e…0c para Carla.
CREATE FUNCTION pg_temp.sesion_de(cliente TEXT) RETURNS TEXT
LANGUAGE sql IMMUTABLE AS $$ SELECT '5e000000-0000-0000-0000-0000000000' || right(cliente, 2) $$;

-- Filas visibles para sc_cliente a nombre de `cliente`.
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
-- Datos: Ana y Beto verificados, Carla pendiente
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, curp, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ana', 'Rojas', 'ana@example.mx', 'ROJA850315MDFDRN03', 'VERIFICADO', now(), 'BAJO'),
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Beto', 'Ibarra', 'beto@example.mx', NULL, 'VERIFICADO', now(), 'BAJO'),
  ('c0000000-0000-0000-0000-00000000000c', 'FISICA', 'Carla', 'Mena', 'carla@example.mx', NULL, 'PENDIENTE', NULL, 'BAJO');

-- Una sesión por cliente, verificada de nuevo (0022): la ventana dura 5
-- minutos, y en esta transacción `now()` no avanza.
INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo, ip)
SELECT pg_temp.sesion_de(c)::uuid, c::uuid, now() + interval '60 minutes', FALSE, 'Chrome on macOS', '203.0.113.7'
  FROM unnest(ARRAY['a0000000-0000-0000-0000-00000000000a', 'b0000000-0000-0000-0000-00000000000b',
                    'c0000000-0000-0000-0000-00000000000c']) AS c;
UPDATE kyc.sesiones SET step_up_valido_hasta = now();

\set maria '''1, ''''MARÍA LÓPEZ GARCÍA'''', ''''HIJO'''', ''''LOGM900101MDFPRR05'''', NULL, 60'''
\set jose '''2, ''''JOSÉ LÓPEZ GARCÍA'''', ''''HIJO'''', NULL, ''''2005-06-07'''', 40'''

-- -----------------------------------------------------------------------------
-- 1. Una designación válida: la base de datos pone versión, fecha y cadena
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana designa a dos hijos, 60% y 40%',
  pg_temp.designacion('d1000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a',
                      ARRAY[:maria, :jose], 2, '100'));
SELECT pg_temp.exigir('versión 1, encadenada al génesis, fechada por la base de datos y con su hash recalculable', (
  SELECT version = 1 AND hash_anterior = repeat('0', 64) AND creada_en = now()
         AND hash = nucleo.sha256_hex(kyc.payload_designacion(d))
    FROM kyc.designaciones_beneficiarios d WHERE id = 'd1000000-0000-0000-0000-000000000001')
  AND kyc.verificar_designaciones('a0000000-0000-0000-0000-00000000000a') IS NULL);
SELECT pg_temp.exigir('el porcentaje se guarda a centésimos, con escala 4 en el payload', (
  SELECT string_agg(porcentaje::text, ',' ORDER BY posicion) = '60.0000,40.0000'
    FROM kyc.beneficiarios WHERE designacion_id = 'd1000000-0000-0000-0000-000000000001'));

-- -----------------------------------------------------------------------------
-- 2. Exactamente 100%, de 1 a 5, en las posiciones 1..n
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('60% y 30% no suman 100%',
  pg_temp.designacion('d2000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY[:maria, '2, ''JOSÉ LÓPEZ GARCÍA'', ''HIJO'', NULL, ''2005-06-07'', 30'], 2, '100'),
  'SC_BENEFICIARIOS_PORCENTAJE');
SELECT pg_temp.debe_fallar('ni 60% y 50%',
  pg_temp.designacion('d2000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY[:maria, '2, ''JOSÉ LÓPEZ GARCÍA'', ''HIJO'', NULL, ''2005-06-07'', 50'], 2, '100'),
  'SC_BENEFICIARIOS_PORCENTAJE');
SELECT pg_temp.debe_fallar('una versión con beneficiarios declara 100%',
  pg_temp.designacion('d2000000-0000-0000-0000-000000000003', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY[:maria, :jose], 2, '90'),
  'ck_designaciones_total');
SELECT pg_temp.debe_fallar('y una vacía, 0%',
  pg_temp.designacion('d2000000-0000-0000-0000-000000000004', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY[]::text[], 0, '100'),
  'ck_designaciones_total');
SELECT pg_temp.debe_fallar('declara tres y tiene dos',
  pg_temp.designacion('d2000000-0000-0000-0000-000000000005', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY[:maria, :jose], 3, '100'),
  'SC_DESIGNACION_INVALIDA');
SELECT pg_temp.debe_fallar('posiciones 1 y 3: con un hueco',
  pg_temp.designacion('d2000000-0000-0000-0000-000000000006', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY[:maria, '3, ''JOSÉ LÓPEZ GARCÍA'', ''HIJO'', NULL, ''2005-06-07'', 40'], 2, '100'),
  'SC_DESIGNACION_INVALIDA');
SELECT pg_temp.debe_fallar('un sexto beneficiario no cabe',
  pg_temp.designacion('d2000000-0000-0000-0000-000000000007', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY['6, ''LUIS PÉREZ'', ''OTRO'', NULL, ''1970-01-01'', 100'], 1, '100'),
  'ck_beneficiarios_posicion');
SELECT pg_temp.debe_fallar('ni una versión de seis',
  pg_temp.designacion('d2000000-0000-0000-0000-000000000008', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY[]::text[], 6, '100'),
  'ck_designaciones_num');
SELECT pg_temp.debe_fallar('un porcentaje es positivo',
  pg_temp.designacion('d2000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY['1, ''LUIS PÉREZ'', ''OTRO'', NULL, ''1970-01-01'', 0'], 1, '100'),
  'ck_beneficiarios_porcentaje');
SELECT pg_temp.debe_fallar('a centésimos: 33.333 no se redondea en silencio',
  pg_temp.designacion('d2000000-0000-0000-0000-000000000010', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY['1, ''LUIS PÉREZ'', ''OTRO'', NULL, ''1970-01-01'', 33.333'], 1, '100'),
  'ck_beneficiarios_porcentaje');

-- -----------------------------------------------------------------------------
-- 3. Quién es cada beneficiario
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('sin CURP ni fecha de nacimiento no se identifica',
  pg_temp.designacion('d3000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY['1, ''LUIS PÉREZ'', ''OTRO'', NULL, NULL, 100'], 1, '100'),
  'ck_beneficiarios_identidad');
SELECT pg_temp.debe_fallar('una CURP sin la estructura oficial',
  pg_temp.designacion('d3000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY['1, ''LUIS PÉREZ'', ''OTRO'', ''PEPL70010'', NULL, 100'], 1, '100'),
  'ck_beneficiarios_curp');
SELECT pg_temp.debe_fallar('con CURP y fecha, la fecha es la de la CURP',
  pg_temp.designacion('d3000000-0000-0000-0000-000000000003', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY['1, ''MARÍA LÓPEZ GARCÍA'', ''HIJO'', ''LOGM900101MDFPRR05'', ''1990-02-01'', 100'], 1, '100'),
  'ck_beneficiarios_curp_fecha');
SELECT pg_temp.debe_fallar('y su siglo: una letra en la posición 17 es de 2000 en adelante',
  pg_temp.designacion('d3000000-0000-0000-0000-000000000004', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY['1, ''JOSÉ LÓPEZ GARCÍA'', ''HIJO'', ''LOGJ050607HDFPRRA1'', ''1905-06-07'', 100'], 1, '100'),
  'ck_beneficiarios_curp_fecha');
SELECT pg_temp.debe_pasar('una CURP de 2005 (letra en la posición 17) con su fecha',
  pg_temp.designacion('d3000000-0000-0000-0000-000000000005', 'b0000000-0000-0000-0000-00000000000b',
    ARRAY['1, ''JOSÉ LÓPEZ GARCÍA'', ''HIJO'', ''LOGJ050607HDFPRRA1'', ''2005-06-07'', 100'], 1, '100'));
SELECT pg_temp.debe_fallar('no nace en el futuro',
  pg_temp.designacion('d3000000-0000-0000-0000-000000000006', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY[format('1, ''LUIS PÉREZ'', ''OTRO'', NULL, %L, 100', current_date + 1)], 1, '100'),
  'SC_BENEFICIARIO_INVALIDO');
SELECT pg_temp.debe_fallar('ni antes de 1900',
  pg_temp.designacion('d3000000-0000-0000-0000-000000000007', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY['1, ''LUIS PÉREZ'', ''OTRO'', NULL, ''1899-12-31'', 100'], 1, '100'),
  'ck_beneficiarios_fecha');
SELECT pg_temp.debe_fallar('la misma CURP dos veces',
  pg_temp.designacion('d3000000-0000-0000-0000-000000000008', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY[:maria, '2, ''MARÍA LÓPEZ G.'', ''HIJO'', ''LOGM900101MDFPRR05'', NULL, 40'], 2, '100'),
  'ux_beneficiarios_curp');
SELECT pg_temp.debe_fallar('el titular no es su propio beneficiario',
  pg_temp.designacion('d3000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY['1, ''ANA ROJAS'', ''OTRO'', ''ROJA850315MDFDRN03'', NULL, 100'], 1, '100'),
  'SC_BENEFICIARIO_TITULAR');
SELECT pg_temp.debe_fallar('un nombre sin dígitos',
  pg_temp.designacion('d3000000-0000-0000-0000-000000000010', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY['1, ''LUIS P3REZ'', ''OTRO'', NULL, ''1970-01-01'', 100'], 1, '100'),
  'ck_beneficiarios_nombre');
SELECT pg_temp.debe_fallar('ni espacios a los lados',
  pg_temp.designacion('d3000000-0000-0000-0000-000000000011', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY['1, '' LUIS PÉREZ'', ''OTRO'', NULL, ''1970-01-01'', 100'], 1, '100'),
  'ck_beneficiarios_nombre');
SELECT pg_temp.debe_fallar('solo parentescos del catálogo',
  pg_temp.designacion('d3000000-0000-0000-0000-000000000012', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY['1, ''LUIS PÉREZ'', ''VECINO'', NULL, ''1970-01-01'', 100'], 1, '100'),
  'ck_beneficiarios_parentesco');

-- -----------------------------------------------------------------------------
-- 4. Versiones y cadena
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana guarda una versión nueva con tres: 33.34, 33.33 y 33.33',
  pg_temp.designacion('d4000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY['1, ''MARÍA LÓPEZ GARCÍA'', ''HIJO'', ''LOGM900101MDFPRR05'', NULL, 33.34',
          '2, ''JOSÉ LÓPEZ GARCÍA'', ''HIJO'', NULL, ''2005-06-07'', 33.33',
          '3, ''PEDRO ROJAS'', ''CONYUGE'', NULL, ''1984-02-29'', 33.33'], 3, '100'));
SELECT pg_temp.debe_pasar('y luego retira la designación: una versión vacía',
  pg_temp.designacion('d4000000-0000-0000-0000-000000000003', 'a0000000-0000-0000-0000-00000000000a',
    ARRAY[]::text[], 0, '0'));
SELECT pg_temp.exigir('tres versiones encadenadas, la vigente vacía', (
  SELECT array_agg(version ORDER BY version) = ARRAY[1, 2, 3]
     AND bool_and(hash_anterior = coalesce(
           (SELECT p.hash FROM kyc.designaciones_beneficiarios p
             WHERE p.cliente_id = d.cliente_id AND p.version = d.version - 1), repeat('0', 64)))
    FROM kyc.designaciones_beneficiarios d WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a')
  AND (SELECT num_beneficiarios = 0 AND total_porcentaje = 0 FROM kyc.designaciones_beneficiarios
        WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND version = 3)
  AND kyc.verificar_designaciones('a0000000-0000-0000-0000-00000000000a') IS NULL);
SELECT pg_temp.debe_fallar('sc_kyc no elige la versión',
  ARRAY['SET LOCAL ROLE sc_kyc',
        $q$INSERT INTO kyc.designaciones_beneficiarios (id, cliente_id, version, num_beneficiarios, total_porcentaje)
           VALUES ('d4000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a', 9, 0, 0)$q$],
  'permission denied');
SELECT pg_temp.debe_fallar('ni el hash',
  ARRAY['SET LOCAL ROLE sc_kyc',
        $q$INSERT INTO kyc.designaciones_beneficiarios (id, cliente_id, num_beneficiarios, total_porcentaje, hash)
           VALUES ('d4000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a', 0, 0, repeat('a', 64))$q$],
  'permission denied');
SELECT pg_temp.debe_fallar('un beneficiario agregado después a una versión guardada',
  ARRAY['SET LOCAL ROLE sc_kyc',
        $q$INSERT INTO kyc.beneficiarios (designacion_id, posicion, nombre_completo, parentesco, curp, fecha_nacimiento, porcentaje)
           VALUES ('d1000000-0000-0000-0000-000000000001', 3, 'LUIS PÉREZ', 'OTRO', NULL, '1970-01-01', 10)$q$],
  'SC_DESIGNACION_INVALIDA');
SELECT pg_temp.debe_fallar('beneficiarios sin su versión',
  ARRAY['SET LOCAL ROLE sc_kyc',
        $q$INSERT INTO kyc.beneficiarios (designacion_id, posicion, nombre_completo, parentesco, curp, fecha_nacimiento, porcentaje)
           VALUES ('d4000000-0000-0000-0000-000000000008', 1, 'LUIS PÉREZ', 'OTRO', NULL, '1970-01-01', 100)$q$],
  'foreign key');
SELECT pg_temp.debe_fallar('Carla, sin KYC verificado, no designa',
  pg_temp.designacion('d4000000-0000-0000-0000-000000000004', 'c0000000-0000-0000-0000-00000000000c',
    ARRAY['1, ''LUIS PÉREZ'', ''OTRO'', NULL, ''1970-01-01'', 100'], 1, '100'),
  'SC_KYC_NO_VERIFICADO');

-- -----------------------------------------------------------------------------
-- 5. Inmutables
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('una versión no se corrige (ni por el dueño del esquema)',
  ARRAY[$q$UPDATE kyc.designaciones_beneficiarios SET dispositivo = 'Edge on Windows' WHERE version = 1$q$],
  'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('ni se borra',
  ARRAY[$q$DELETE FROM kyc.designaciones_beneficiarios WHERE version = 3$q$], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('un beneficiario no cambia de porcentaje',
  ARRAY[$q$UPDATE kyc.beneficiarios SET porcentaje = 50 WHERE posicion = 1$q$], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('ni se borra',
  ARRAY[$q$DELETE FROM kyc.beneficiarios$q$], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('las tablas no se vacían',
  ARRAY['TRUNCATE kyc.designaciones_beneficiarios CASCADE'], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('sc_kyc no tiene permiso para corregir',
  ARRAY['SET LOCAL ROLE sc_kyc', $q$UPDATE kyc.beneficiarios SET porcentaje = 50$q$], 'permission denied');

-- -----------------------------------------------------------------------------
-- 6. Privilegios y RLS
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('RLS en las dos tablas, con la política RESTRICTIVE de sc_cliente', (
  SELECT count(*) = 2 FROM pg_class
   WHERE relrowsecurity AND oid IN ('kyc.designaciones_beneficiarios'::regclass, 'kyc.beneficiarios'::regclass))
  AND (SELECT count(*) = 2 FROM pg_policies
        WHERE schemaname = 'kyc' AND tablename IN ('designaciones_beneficiarios', 'beneficiarios')
          AND policyname = 'solo_del_cliente' AND permissive = 'RESTRICTIVE' AND roles = '{sc_cliente}'));
SELECT pg_temp.exigir('Ana ve sus 3 versiones y sus 5 beneficiarios; Beto, la suya; sin cliente, nada', (
  pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM kyc.designaciones_beneficiarios') = 3
  AND pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM kyc.beneficiarios') = 5
  AND pg_temp.visibles('b0000000-0000-0000-0000-00000000000b', 'SELECT count(*) FROM kyc.designaciones_beneficiarios') = 1
  AND pg_temp.visibles('b0000000-0000-0000-0000-00000000000b', 'SELECT count(*) FROM kyc.beneficiarios') = 1
  AND pg_temp.visibles('', 'SELECT count(*) FROM kyc.beneficiarios') = 0));
SELECT pg_temp.debe_fallar('el cliente no escribe su designación directamente',
  ARRAY[$q$SELECT set_config('sc.cliente_actual', 'a0000000-0000-0000-0000-00000000000a', true)$q$,
        'SET LOCAL ROLE sc_cliente',
        $q$INSERT INTO kyc.designaciones_beneficiarios (id, cliente_id, num_beneficiarios, total_porcentaje)
           VALUES ('d6000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a', 0, 0)$q$],
  'permission denied');
SELECT pg_temp.exigir('ningún otro rol de la aplicación toca la designación', (
  SELECT bool_and(NOT has_any_column_privilege(r, t, 'SELECT, INSERT, UPDATE, REFERENCES'))
    FROM unnest(ARRAY['sc_ifpe', 'sc_sofom', 'sc_puente', 'sc_spei', 'sc_mercado', 'sc_aml', 'sc_auditor',
                      'sc_auditor_pld', 'sc_kyc_verificador', 'sc_auth']) AS r,
         unnest(ARRAY['kyc.designaciones_beneficiarios', 'kyc.beneficiarios']) AS t));

-- -----------------------------------------------------------------------------
-- 7. Cada versión queda también en la bitácora de seguridad del cliente
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('un evento BENEFICIARIOS_DESIGNADOS por versión guardada, con su IP, dispositivo y versión, escrito por sc_kyc', (
  SELECT array_agg(detalle ORDER BY id) = ARRAY['1', '2', '3']
     AND bool_and(ip = '203.0.113.7' AND dispositivo = 'Chrome on macOS' AND registrado_por = 'sc_kyc')
    FROM kyc.eventos_autenticacion
   WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND evento = 'BENEFICIARIOS_DESIGNADOS')
  AND (SELECT count(*) = 1 FROM kyc.eventos_autenticacion
        WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b' AND evento = 'BENEFICIARIOS_DESIGNADOS'));
SELECT pg_temp.exigir('las versiones rechazadas no dejaron evento (ni la de Carla)', (
  SELECT count(*) = 4 FROM kyc.eventos_autenticacion WHERE evento = 'BENEFICIARIOS_DESIGNADOS'));
SELECT pg_temp.exigir('Ana lee sus tres eventos y Beto el suyo', (
  pg_temp.visibles('a0000000-0000-0000-0000-00000000000a',
    $q$SELECT count(*) FROM kyc.eventos_autenticacion WHERE evento = 'BENEFICIARIOS_DESIGNADOS'$q$) = 3
  AND pg_temp.visibles('b0000000-0000-0000-0000-00000000000b',
    $q$SELECT count(*) FROM kyc.eventos_autenticacion WHERE evento = 'BENEFICIARIOS_DESIGNADOS'$q$) = 1));
SELECT pg_temp.debe_fallar('sc_kyc no escribe otro evento en la bitácora',
  ARRAY['SET LOCAL ROLE sc_kyc',
        $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento)
           VALUES ('a0000000-0000-0000-0000-00000000000a', 'INICIO_SESION')$q$],
  'row-level security');
SELECT pg_temp.debe_fallar('ni el de una versión que no existe',
  ARRAY['SET LOCAL ROLE sc_kyc',
        $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, dispositivo, detalle)
           VALUES ('a0000000-0000-0000-0000-00000000000a', 'BENEFICIARIOS_DESIGNADOS', '203.0.113.7', 'Chrome on macOS', '9')$q$],
  'row-level security');
SELECT pg_temp.debe_fallar('ni el de una versión con otro dispositivo',
  ARRAY['SET LOCAL ROLE sc_kyc',
        $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, dispositivo, detalle)
           VALUES ('a0000000-0000-0000-0000-00000000000a', 'BENEFICIARIOS_DESIGNADOS', '203.0.113.7', 'Edge on Windows', '2')$q$],
  'row-level security');
SELECT pg_temp.debe_fallar('ni un segundo evento de la misma versión',
  ARRAY['SET LOCAL ROLE sc_kyc',
        $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, sesion_id, dispositivo, detalle)
           VALUES ('a0000000-0000-0000-0000-00000000000a', 'BENEFICIARIOS_DESIGNADOS', '203.0.113.7',
                   '5e000000-0000-0000-0000-00000000000a', 'Chrome on macOS', '2')$q$],
  'ux_eventos_autenticacion_designacion');
SELECT pg_temp.debe_fallar('y el evento, como toda la bitácora, no se borra',
  ARRAY[$q$DELETE FROM kyc.eventos_autenticacion WHERE evento = 'BENEFICIARIOS_DESIGNADOS'$q$],
  'SC_LEDGER_INMUTABLE');

-- -----------------------------------------------------------------------------
-- 8. La cadena delata una alteración por fuera de la aplicación
-- -----------------------------------------------------------------------------
-- El dueño del esquema desactiva el trigger y cambia un nombre de la versión 1:
-- la versión 1 ya no cuadra con su hash.
-- (ALTER TABLE no admite eventos diferidos pendientes: se disparan antes.)
SET CONSTRAINTS ALL IMMEDIATE;
ALTER TABLE kyc.beneficiarios DISABLE TRIGGER trg_beneficiarios_inmutable;
UPDATE kyc.beneficiarios SET nombre_completo = 'MARIO LÓPEZ GARCÍA'
 WHERE designacion_id = 'd1000000-0000-0000-0000-000000000001' AND posicion = 1;
ALTER TABLE kyc.beneficiarios ENABLE TRIGGER trg_beneficiarios_inmutable;
SELECT pg_temp.exigir('alterar un beneficiario guardado rompe la cadena en su versión',
  kyc.verificar_designaciones('a0000000-0000-0000-0000-00000000000a') = 1);

\o
\echo 'tests/db/0020_beneficiarios.sql: todas las pruebas pasaron (ROLLBACK)'
ROLLBACK;
