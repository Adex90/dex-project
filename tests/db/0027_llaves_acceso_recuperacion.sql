-- =============================================================================
--  Pruebas de la migración 0027 (llaves de acceso y códigos de recuperación).
--  Corren en una transacción que termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0027_llaves_acceso_recuperacion.sql
--
--  Lo que la base de datos garantiza por sí sola: una llave de acceso la
--  registra y la revoca el propio cliente (nunca el personal) desde una sesión
--  vigente con su ventana de verificación reforzada abierta, a lo más 10
--  activas; de una llave solo cambian su uso (el contador avanza, salvo en un
--  autenticador que no cuenta) y, una vez, su revocación; nunca se borra. Un
--  desafío vale 5 minutos y una sola vez. Una generación de códigos de
--  recuperación tiene exactamente 8, nace en una sesión con su ventana
--  abierta, reemplaza a la anterior (cuyos códigos dejan de valer), y cada
--  código se usa una vez, en una sesión del cliente. Cada registro, revocación,
--  generación y uso deja su evento en la bitácora, y sc_auth no puede
--  escribirlos sin el cambio. El cliente lee lo suyo sin claves ni hashes.
--  backend/tests/llaves.rs cubre la API, WebAuthn y el inicio de sesión.
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

-- Un id de 32 bytes a partir de un texto (para credenciales, claves y desafíos).
CREATE FUNCTION pg_temp.b(texto TEXT) RETURNS TEXT
LANGUAGE sql AS $$ SELECT encode(sha256(convert_to(texto, 'UTF8')), 'hex') $$;

-- Registra una llave como sc_auth (como backend/src/auth/llaves.rs).
CREATE FUNCTION pg_temp.registrar(id TEXT, cliente TEXT, credencial TEXT, sesion TEXT,
                                  nombre TEXT DEFAULT 'iPhone de Ana', contador BIGINT DEFAULT 0,
                                  algoritmo INTEGER DEFAULT -7, elegible BOOLEAN DEFAULT TRUE,
                                  respaldada BOOLEAN DEFAULT TRUE) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
      'SET LOCAL ROLE sc_auth',
      format($q$INSERT INTO kyc.credenciales_webauthn (id, cliente_id, credencial, clave_publica, algoritmo, contador,
                       nombre, transportes, aaguid, respaldo_elegible, respaldada, sesion_id, ip, dispositivo)
                VALUES (%L, %L, decode(%L, 'hex'), decode(%L, 'hex'), %s, %s, %L, '{internal,hybrid}',
                        '00000000-0000-0000-0000-000000000000', %L, %L, %L, '203.0.113.7', 'Chrome on macOS')$q$,
             id, cliente, pg_temp.b(credencial), pg_temp.b('clave ' || credencial) || pg_temp.b('x ' || credencial),
             algoritmo, contador, nombre, elegible, respaldada, sesion),
      'RESET ROLE']
$$;

-- Un uso como sc_auth: el contador que reportó el autenticador.
CREATE FUNCTION pg_temp.usar(id TEXT, contador BIGINT, respaldada BOOLEAN DEFAULT TRUE) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY['SET LOCAL ROLE sc_auth',
                 format('UPDATE kyc.credenciales_webauthn SET usos = usos + 1, contador = %s, respaldada = %L WHERE id = %L',
                        contador, respaldada, id),
                 'RESET ROLE']
$$;

-- Una revocación como sc_auth desde la sesión dada.
CREATE FUNCTION pg_temp.revocar(id TEXT, sesion TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY['SET LOCAL ROLE sc_auth',
                 format($q$UPDATE kyc.credenciales_webauthn SET revocada_en = now(), revocada_sesion_id = %L,
                                  revocada_ip = '198.51.100.4', revocada_dispositivo = 'Safari on iPhone'
                            WHERE id = %L$q$, sesion, id),
                 'RESET ROLE']
$$;

-- Una generación de `n` códigos como sc_auth (los hashes, de `semilla-i`).
CREATE FUNCTION pg_temp.generar(lote TEXT, cliente TEXT, sesion TEXT, semilla TEXT, n INTEGER DEFAULT 8) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY['SET LOCAL ROLE sc_auth',
                 format($q$INSERT INTO kyc.lotes_recuperacion (id, cliente_id, sesion_id, ip, dispositivo)
                           VALUES (%L, %L, %L, '203.0.113.7', 'Chrome on macOS')$q$, lote, cliente, sesion),
                 format($q$INSERT INTO kyc.codigos_recuperacion (lote_id, cliente_id, hash)
                           SELECT %L, %L, sha256(convert_to(%L || '-' || i, 'UTF8')) FROM generate_series(1, %s) i$q$,
                        lote, cliente, semilla, n),
                 'RESET ROLE']
$$;

-- El uso de un código como sc_auth, en la sesión dada.
CREATE FUNCTION pg_temp.usar_codigo(cliente TEXT, semilla TEXT, sesion TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY['SET LOCAL ROLE sc_auth',
                 format($q$DO $d$ BEGIN
                             UPDATE kyc.codigos_recuperacion SET usado_en = now(), usado_sesion_id = %L,
                                    usado_ip = '198.51.100.4', usado_dispositivo = 'Safari on iPhone'
                              WHERE cliente_id = %L AND hash = sha256(convert_to(%L, 'UTF8'));
                             IF NOT FOUND THEN RAISE EXCEPTION 'SIN_CODIGO'; END IF;
                           END $d$ $q$, sesion, cliente, semilla),
                 'RESET ROLE']
$$;

-- -----------------------------------------------------------------------------
-- Datos: Ana y Beto (clientes) y Olga (operadora), con sus credenciales.
-- Sesiones: 1 de Ana con ventana; 2 de Ana sin ventana; 3 de Beto con
-- ventana; 4 de Olga con ventana; 5 de Ana vencida.
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ana',  'Prueba', 'ana@example.mx',  'VERIFICADO', now(), 'BAJO'),
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Beto', 'Prueba', 'beto@example.mx', 'VERIFICADO', now(), 'BAJO'),
  ('0a000000-0000-0000-0000-0000000000a0', 'FISICA', 'Olga', 'Prueba', 'olga@example.mx', 'VERIFICADO', now(), 'BAJO');
INSERT INTO kyc.credenciales (cliente_id, password_hash, rol) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'x', 'CLIENTE'),
  ('b0000000-0000-0000-0000-00000000000b', 'x', 'CLIENTE'),
  ('0a000000-0000-0000-0000-0000000000a0', 'x', 'OPERADOR');
INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo, ip) VALUES
  ('5e000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a', now() + interval '1 hour', FALSE,
   'Chrome on macOS', '203.0.113.7'),
  ('5e000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a', now() + interval '1 hour', FALSE,
   'Safari on iPhone', '198.51.100.4'),
  ('5e000000-0000-0000-0000-000000000003', 'b0000000-0000-0000-0000-00000000000b', now() + interval '1 hour', FALSE,
   'Firefox on Linux', NULL),
  ('5e000000-0000-0000-0000-000000000004', '0a000000-0000-0000-0000-0000000000a0', now() + interval '1 hour', TRUE,
   'Edge on Windows', '192.0.2.10');
INSERT INTO kyc.sesiones (id, cliente_id, creada_en, ultimo_uso_en, expira_en, mfa, dispositivo) VALUES
  ('5e000000-0000-0000-0000-000000000005', 'a0000000-0000-0000-0000-00000000000a', now() - interval '2 hours',
   now() - interval '2 hours', now() - interval '1 hour', FALSE, 'Chrome on macOS');
SELECT pg_temp.debe_pasar('ventanas de verificación reforzada en las sesiones 1, 3 y 4', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.sesiones SET step_up_valido_hasta = now()
      WHERE id IN ('5e000000-0000-0000-0000-000000000001', '5e000000-0000-0000-0000-000000000003',
                   '5e000000-0000-0000-0000-000000000004')$q$,
  'RESET ROLE']);

-- -----------------------------------------------------------------------------
-- Estructura, privilegios y RLS
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('RLS en las cinco tablas, con la política RESTRICTIVE de sc_cliente en las que lee', (
  SELECT count(*) = 5 FROM pg_class
   WHERE oid IN ('kyc.usuarios_webauthn'::regclass, 'kyc.credenciales_webauthn'::regclass,
                 'kyc.desafios_webauthn'::regclass, 'kyc.lotes_recuperacion'::regclass,
                 'kyc.codigos_recuperacion'::regclass) AND relrowsecurity)
  AND (SELECT count(*) = 3 FROM pg_policies
        WHERE schemaname = 'kyc' AND policyname = 'solo_del_cliente' AND permissive = 'RESTRICTIVE'
          AND roles = '{sc_cliente}'
          AND tablename IN ('credenciales_webauthn', 'lotes_recuperacion', 'codigos_recuperacion')));

SELECT pg_temp.exigir('el cliente y el auditor leen nombres, fechas y usos; nunca claves, ids de credencial, hashes ni desafíos',
  has_column_privilege('sc_cliente', 'kyc.credenciales_webauthn', 'nombre', 'SELECT')
  AND has_column_privilege('sc_auditor', 'kyc.credenciales_webauthn', 'ultimo_uso_en', 'SELECT')
  AND NOT has_column_privilege('sc_cliente', 'kyc.credenciales_webauthn', 'clave_publica', 'SELECT')
  AND NOT has_column_privilege('sc_cliente', 'kyc.credenciales_webauthn', 'credencial', 'SELECT')
  AND NOT has_column_privilege('sc_auditor', 'kyc.credenciales_webauthn', 'clave_publica', 'SELECT')
  AND NOT has_column_privilege('sc_cliente', 'kyc.codigos_recuperacion', 'hash', 'SELECT')
  AND NOT has_column_privilege('sc_auditor', 'kyc.codigos_recuperacion', 'hash', 'SELECT')
  AND NOT has_table_privilege('sc_cliente', 'kyc.desafios_webauthn', 'SELECT')
  AND NOT has_table_privilege('sc_cliente', 'kyc.usuarios_webauthn', 'SELECT'));

SELECT pg_temp.exigir('sc_auth no cambia de una llave más que su uso (sin su fecha) y su revocación, ni el hash de un código',
  has_column_privilege('sc_auth', 'kyc.credenciales_webauthn', 'contador', 'UPDATE')
  AND has_column_privilege('sc_auth', 'kyc.credenciales_webauthn', 'revocada_en', 'UPDATE')
  AND NOT has_column_privilege('sc_auth', 'kyc.credenciales_webauthn', 'ultimo_uso_en', 'UPDATE')
  AND NOT has_column_privilege('sc_auth', 'kyc.credenciales_webauthn', 'usos', 'INSERT')
  AND NOT has_column_privilege('sc_auth', 'kyc.credenciales_webauthn', 'nombre', 'UPDATE')
  AND NOT has_column_privilege('sc_auth', 'kyc.credenciales_webauthn', 'clave_publica', 'UPDATE')
  AND NOT has_column_privilege('sc_auth', 'kyc.credenciales_webauthn', 'creada_en', 'INSERT')
  AND NOT has_column_privilege('sc_auth', 'kyc.codigos_recuperacion', 'hash', 'UPDATE')
  AND NOT has_table_privilege('sc_auth', 'kyc.credenciales_webauthn', 'DELETE')
  AND NOT has_table_privilege('sc_auth', 'kyc.codigos_recuperacion', 'DELETE'));

SELECT pg_temp.exigir('ningún otro rol de la aplicación escribe llaves, desafíos ni códigos', (
  SELECT bool_and(NOT has_table_privilege(r, t, p))
    FROM unnest(ARRAY['sc_kyc', 'sc_ifpe', 'sc_spei', 'sc_mercado', 'sc_cliente', 'sc_auditor']) r,
         unnest(ARRAY['kyc.usuarios_webauthn', 'kyc.credenciales_webauthn', 'kyc.desafios_webauthn',
                      'kyc.lotes_recuperacion', 'kyc.codigos_recuperacion']) t,
         unnest(ARRAY['INSERT', 'UPDATE', 'DELETE']) p));

-- -----------------------------------------------------------------------------
-- La bitácora: sc_auth no escribe los eventos sin su cambio (Beto aún no hizo nada)
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('sc_auth no escribe PASSKEY_REGISTRADA sin una llave registrada en esa sesión', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, sesion_id, dispositivo, detalle)
     VALUES ('b0000000-0000-0000-0000-00000000000b', 'PASSKEY_REGISTRADA', '5e000000-0000-0000-0000-000000000003',
             'Firefox on Linux', '1')$q$], 'row-level security');
SELECT pg_temp.debe_fallar('ni PASSKEY_REVOCADA sin una revocación', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, sesion_id, dispositivo, detalle)
     VALUES ('b0000000-0000-0000-0000-00000000000b', 'PASSKEY_REVOCADA', '5e000000-0000-0000-0000-000000000003',
             'Firefox on Linux', '0')$q$], 'row-level security');
SELECT pg_temp.debe_fallar('ni INICIO_SESION_PASSKEY sin una llave usada', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, sesion_id, dispositivo)
     VALUES ('b0000000-0000-0000-0000-00000000000b', 'INICIO_SESION_PASSKEY', '5e000000-0000-0000-0000-000000000003',
             'Firefox on Linux')$q$], 'row-level security');
SELECT pg_temp.debe_fallar('ni CODIGOS_RECUPERACION_GENERADOS sin una generación', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, sesion_id, dispositivo, detalle)
     VALUES ('b0000000-0000-0000-0000-00000000000b', 'CODIGOS_RECUPERACION_GENERADOS',
             '5e000000-0000-0000-0000-000000000003', 'Firefox on Linux', '8')$q$], 'row-level security');
SELECT pg_temp.debe_fallar('ni CODIGO_RECUPERACION_USADO sin un código usado', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, sesion_id, dispositivo, detalle)
     VALUES ('b0000000-0000-0000-0000-00000000000b', 'CODIGO_RECUPERACION_USADO',
             '5e000000-0000-0000-0000-000000000003', 'Firefox on Linux', '7')$q$], 'row-level security');
SELECT pg_temp.debe_pasar('los rechazos sí los escribe el backend', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, dispositivo)
     VALUES ('b0000000-0000-0000-0000-00000000000b', 'PASSKEY_RECHAZADA', '192.0.2.99', 'Chrome on Windows'),
            ('b0000000-0000-0000-0000-00000000000b', 'CODIGO_RECUPERACION_RECHAZADO', '192.0.2.99', 'Chrome on Windows')$q$,
  'RESET ROLE']);
SELECT pg_temp.debe_fallar('un evento fuera del catálogo', ARRAY[
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento) VALUES ('b0000000-0000-0000-0000-00000000000b', 'PASSKEY_COPIADA')$q$],
  'ck_eventos_autenticacion_evento');

-- -----------------------------------------------------------------------------
-- El identificador WebAuthn
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('sc_auth crea el identificador de Ana (32 bytes)', ARRAY[
  'SET LOCAL ROLE sc_auth',
  format($q$INSERT INTO kyc.usuarios_webauthn (cliente_id, identificador) VALUES ('a0000000-0000-0000-0000-00000000000a', decode(%L, 'hex'))$q$,
         pg_temp.b('usuario ana')),
  'RESET ROLE']);
SELECT pg_temp.debe_fallar('uno por cliente', ARRAY[
  format($q$INSERT INTO kyc.usuarios_webauthn (cliente_id, identificador) VALUES ('a0000000-0000-0000-0000-00000000000a', decode(%L, 'hex'))$q$,
         pg_temp.b('otro de ana'))], 'usuarios_webauthn_pkey');
SELECT pg_temp.debe_fallar('de 32 bytes', ARRAY[
  $q$INSERT INTO kyc.usuarios_webauthn (cliente_id, identificador) VALUES ('b0000000-0000-0000-0000-00000000000b', '\x0102')$q$],
  'ck_usuarios_webauthn_identificador');
SELECT pg_temp.debe_fallar('el personal no tiene identificador WebAuthn', ARRAY[
  format($q$INSERT INTO kyc.usuarios_webauthn (cliente_id, identificador) VALUES ('0a000000-0000-0000-0000-0000000000a0', decode(%L, 'hex'))$q$,
         pg_temp.b('usuario olga'))], 'SC_SOLO_CLIENTES');
SELECT pg_temp.debe_fallar('no cambia (los autenticadores lo guardan)', ARRAY[
  format($q$UPDATE kyc.usuarios_webauthn SET identificador = decode(%L, 'hex') WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a'$q$,
         pg_temp.b('nuevo'))], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('ni se borra', ARRAY[
  $q$DELETE FROM kyc.usuarios_webauthn WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a'$q$], 'SC_LEDGER_INMUTABLE');

-- -----------------------------------------------------------------------------
-- Registrar una llave
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana registra una llave desde su sesión con ventana',
  pg_temp.registrar('c1000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a', 'ana-1',
                    '5e000000-0000-0000-0000-000000000001'));
SELECT pg_temp.exigir('nace con la fecha de la base de datos, sin usar ni revocar', (
  SELECT creada_en = now() AND ultimo_uso_en IS NULL AND revocada_en IS NULL
    FROM kyc.credenciales_webauthn WHERE id = 'c1000000-0000-0000-0000-000000000001'));
SELECT pg_temp.exigir('PASSKEY_REGISTRADA en su bitácora, con la sesión, la IP, el dispositivo y las llaves activas', (
  SELECT count(*) = 1 FROM kyc.eventos_autenticacion
   WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND evento = 'PASSKEY_REGISTRADA'
     AND sesion_id = '5e000000-0000-0000-0000-000000000001' AND ip = '203.0.113.7'
     AND dispositivo = 'Chrome on macOS' AND detalle = '1' AND registrado_por = 'sc_auth'));

SELECT pg_temp.debe_fallar('desde una sesión sin ventana, no',
  pg_temp.registrar('c1000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a', 'ana-2',
                    '5e000000-0000-0000-0000-000000000002'), 'SC_STEP_UP_REQUERIDO');
SELECT pg_temp.debe_fallar('ni desde la sesión de otro cliente',
  pg_temp.registrar('c1000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a', 'ana-2',
                    '5e000000-0000-0000-0000-000000000003'), 'SC_STEP_UP_REQUERIDO');
SELECT pg_temp.debe_fallar('ni desde una sesión vencida',
  pg_temp.registrar('c1000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a', 'ana-2',
                    '5e000000-0000-0000-0000-000000000005'), 'SC_STEP_UP_REQUERIDO');
SELECT pg_temp.debe_fallar('el personal no registra llaves (sigue con su TOTP)',
  pg_temp.registrar('c1000000-0000-0000-0000-000000000002', '0a000000-0000-0000-0000-0000000000a0', 'olga-1',
                    '5e000000-0000-0000-0000-000000000004'), 'SC_SOLO_CLIENTES');
SELECT pg_temp.debe_fallar('una credencial es única en todo el sistema',
  pg_temp.registrar('c1000000-0000-0000-0000-000000000002', 'b0000000-0000-0000-0000-00000000000b', 'ana-1',
                    '5e000000-0000-0000-0000-000000000003'), 'ux_credenciales_webauthn_credencial');
SELECT pg_temp.debe_fallar('nace sin usar', ARRAY[
  format($q$INSERT INTO kyc.credenciales_webauthn (cliente_id, credencial, clave_publica, algoritmo, nombre, aaguid,
                   respaldo_elegible, respaldada, sesion_id, dispositivo, ultimo_uso_en)
            VALUES ('a0000000-0000-0000-0000-00000000000a', decode(%L, 'hex'), decode(%L, 'hex'), -7, 'Llave',
                    '00000000-0000-0000-0000-000000000000', FALSE, FALSE, '5e000000-0000-0000-0000-000000000001',
                    'Chrome on macOS', now())$q$, pg_temp.b('ana-x'), pg_temp.b('k') || pg_temp.b('k2'))],
  'SC_PASSKEY_INCOHERENTE');
SELECT pg_temp.debe_fallar('un algoritmo COSE que no se acepta (ES384)',
  pg_temp.registrar('c1000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a', 'ana-2',
                    '5e000000-0000-0000-0000-000000000001', 'Llave', 0, -35), 'ck_credenciales_webauthn_algoritmo');
SELECT pg_temp.debe_fallar('un nombre en blanco',
  pg_temp.registrar('c1000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a', 'ana-2',
                    '5e000000-0000-0000-0000-000000000001', '   '), 'ck_credenciales_webauthn_nombre');
SELECT pg_temp.debe_fallar('un nombre con caracteres de control',
  pg_temp.registrar('c1000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a', 'ana-2',
                    '5e000000-0000-0000-0000-000000000001', E'Llave\nde Ana'), 'ck_credenciales_webauthn_nombre');
SELECT pg_temp.debe_fallar('un nombre de más de 60 caracteres',
  pg_temp.registrar('c1000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a', 'ana-2',
                    '5e000000-0000-0000-0000-000000000001', repeat('x', 61)), 'ck_credenciales_webauthn_nombre');
SELECT pg_temp.debe_fallar('respaldada sin poder respaldarse',
  pg_temp.registrar('c1000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a', 'ana-2',
                    '5e000000-0000-0000-0000-000000000001', 'Llave', 0, -7, FALSE, TRUE),
  'ck_credenciales_webauthn_respaldo');
SELECT pg_temp.debe_fallar('un transporte desconocido', ARRAY[
  format($q$INSERT INTO kyc.credenciales_webauthn (cliente_id, credencial, clave_publica, algoritmo, nombre, transportes,
                   aaguid, respaldo_elegible, respaldada, sesion_id, dispositivo)
            VALUES ('a0000000-0000-0000-0000-00000000000a', decode(%L, 'hex'), decode(%L, 'hex'), -7, 'Llave',
                    '{internal,paloma}', '00000000-0000-0000-0000-000000000000', FALSE, FALSE,
                    '5e000000-0000-0000-0000-000000000001', 'Chrome on macOS')$q$,
         pg_temp.b('ana-y'), pg_temp.b('k') || pg_temp.b('k2'))], 'ck_credenciales_webauthn_transportes');

-- A lo más 10 activas: con la primera, nueve más; la undécima no; revocar libera un lugar.
SELECT pg_temp.debe_pasar('Ana registra nueve llaves más (EdDSA y RS256 entre ellas)',
  (SELECT array_agg(s ORDER BY n, o)
     FROM generate_series(2, 10) n,
          unnest(pg_temp.registrar(format('c1000000-0000-0000-0000-%s', lpad(n::text, 12, '0')),
                                   'a0000000-0000-0000-0000-00000000000a', 'ana-' || n,
                                   '5e000000-0000-0000-0000-000000000001', 'Llave ' || n, 0,
                                   CASE WHEN n = 2 THEN -8 WHEN n = 3 THEN -257 ELSE -7 END)) WITH ORDINALITY AS u (s, o)));
SELECT pg_temp.debe_fallar('la undécima activa, no',
  pg_temp.registrar('c1000000-0000-0000-0000-000000000011', 'a0000000-0000-0000-0000-00000000000a', 'ana-11',
                    '5e000000-0000-0000-0000-000000000001'), 'SC_LIMITE_PASSKEYS');

-- -----------------------------------------------------------------------------
-- Usar una llave: el contador avanza
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('un uso con el contador en 5', pg_temp.usar('c1000000-0000-0000-0000-000000000001', 5));
SELECT pg_temp.exigir('un uso; su fecha la pone la base de datos', (
  SELECT contador = 5 AND usos = 1 AND ultimo_uso_en = now() FROM kyc.credenciales_webauthn
   WHERE id = 'c1000000-0000-0000-0000-000000000001'));
SELECT pg_temp.debe_pasar('con la llave recién usada, sc_auth escribe INICIO_SESION_PASSKEY', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, sesion_id, ip, dispositivo)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'INICIO_SESION_PASSKEY', '5e000000-0000-0000-0000-000000000002',
             '198.51.100.4', 'Safari on iPhone')$q$,
  'RESET ROLE']);
SELECT pg_temp.debe_fallar('el mismo contador otra vez (aun en la misma transacción): una copia de la llave',
  pg_temp.usar('c1000000-0000-0000-0000-000000000001', 5), 'SC_PASSKEY_CONTADOR');
SELECT pg_temp.debe_fallar('un contador menor', pg_temp.usar('c1000000-0000-0000-0000-000000000001', 3),
  'SC_PASSKEY_CONTADOR');
SELECT pg_temp.debe_fallar('ni de vuelta a cero', pg_temp.usar('c1000000-0000-0000-0000-000000000001', 0),
  'SC_PASSKEY_CONTADOR');
SELECT pg_temp.debe_pasar('un autenticador que no cuenta (siempre 0) se usa una y otra vez',
  pg_temp.usar('c1000000-0000-0000-0000-000000000004', 0) || pg_temp.usar('c1000000-0000-0000-0000-000000000004', 0));
SELECT pg_temp.exigir('y sus usos se cuentan', (
  SELECT usos = 2 AND contador = 0 FROM kyc.credenciales_webauthn WHERE id = 'c1000000-0000-0000-0000-000000000004'));
SELECT pg_temp.debe_fallar('los usos se cuentan de uno en uno', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.credenciales_webauthn SET usos = usos + 2, contador = 50 WHERE id = 'c1000000-0000-0000-0000-000000000001'$q$],
  'SC_PASSKEY_INCOHERENTE');
SELECT pg_temp.debe_fallar('el último uso no cambia sin un uso (ni para el dueño del esquema)', ARRAY[
  $q$UPDATE kyc.credenciales_webauthn SET ultimo_uso_en = now() + interval '1 day'
      WHERE id = 'c1000000-0000-0000-0000-000000000001'$q$], 'SC_PASSKEY_INMUTABLE');
SELECT pg_temp.debe_fallar('el contador no cambia sin un uso', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.credenciales_webauthn SET contador = 9 WHERE id = 'c1000000-0000-0000-0000-000000000001'$q$],
  'SC_PASSKEY_INMUTABLE');
SELECT pg_temp.debe_fallar('ni el respaldo', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.credenciales_webauthn SET respaldada = FALSE WHERE id = 'c1000000-0000-0000-0000-000000000001'$q$],
  'SC_PASSKEY_INMUTABLE');
SELECT pg_temp.debe_pasar('al usarla, el respaldo que reporta el autenticador',
  pg_temp.usar('c1000000-0000-0000-0000-000000000001', 6, FALSE));
SELECT pg_temp.debe_fallar('el nombre no cambia (ni para el dueño del esquema)', ARRAY[
  $q$UPDATE kyc.credenciales_webauthn SET nombre = 'Otra' WHERE id = 'c1000000-0000-0000-0000-000000000001'$q$],
  'SC_PASSKEY_INMUTABLE');
SELECT pg_temp.debe_fallar('ni la clave pública', ARRAY[
  $q$UPDATE kyc.credenciales_webauthn SET clave_publica = clave_publica || '\x00' WHERE id = 'c1000000-0000-0000-0000-000000000001'$q$],
  'SC_PASSKEY_INMUTABLE');
SELECT pg_temp.debe_fallar('ni el cliente', ARRAY[
  $q$UPDATE kyc.credenciales_webauthn SET cliente_id = 'b0000000-0000-0000-0000-00000000000b'
      WHERE id = 'c1000000-0000-0000-0000-000000000001'$q$], 'SC_PASSKEY_INMUTABLE');

-- -----------------------------------------------------------------------------
-- Revocar una llave
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('revocar desde una sesión sin ventana, no',
  pg_temp.revocar('c1000000-0000-0000-0000-000000000010', '5e000000-0000-0000-0000-000000000002'), 'SC_STEP_UP_REQUERIDO');
SELECT pg_temp.debe_fallar('ni desde la sesión de otro cliente',
  pg_temp.revocar('c1000000-0000-0000-0000-000000000010', '5e000000-0000-0000-0000-000000000003'), 'SC_STEP_UP_REQUERIDO');
SELECT pg_temp.debe_fallar('revocar no es usar', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.credenciales_webauthn SET revocada_en = now(), revocada_sesion_id = '5e000000-0000-0000-0000-000000000001',
            revocada_dispositivo = 'Chrome on macOS', usos = usos + 1, contador = 1
      WHERE id = 'c1000000-0000-0000-0000-000000000010'$q$], 'SC_PASSKEY_INCOHERENTE');
SELECT pg_temp.debe_fallar('los datos de la revocación van con ella', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.credenciales_webauthn SET revocada_sesion_id = '5e000000-0000-0000-0000-000000000001'
      WHERE id = 'c1000000-0000-0000-0000-000000000010'$q$], 'SC_PASSKEY_INCOHERENTE');
SELECT pg_temp.debe_pasar('Ana revoca la décima desde su sesión con ventana',
  pg_temp.revocar('c1000000-0000-0000-0000-000000000010', '5e000000-0000-0000-0000-000000000001'));
SELECT pg_temp.exigir('revocada con la fecha de la base de datos; PASSKEY_REVOCADA con la sesión, la IP, el dispositivo y las 9 activas', (
  SELECT revocada_en = now() FROM kyc.credenciales_webauthn WHERE id = 'c1000000-0000-0000-0000-000000000010')
  AND (SELECT count(*) = 1 FROM kyc.eventos_autenticacion
        WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND evento = 'PASSKEY_REVOCADA'
          AND sesion_id = '5e000000-0000-0000-0000-000000000001' AND ip = '198.51.100.4'
          AND dispositivo = 'Safari on iPhone' AND detalle = '9'));
SELECT pg_temp.debe_fallar('revocada, ya no se usa', pg_temp.usar('c1000000-0000-0000-0000-000000000010', 1),
  'SC_PASSKEY_REVOCADA');
SELECT pg_temp.debe_fallar('ni se revoca otra vez', pg_temp.revocar('c1000000-0000-0000-0000-000000000010',
  '5e000000-0000-0000-0000-000000000001'), 'SC_PASSKEY_REVOCADA');
SELECT pg_temp.debe_pasar('revocar liberó un lugar: la décima activa',
  pg_temp.registrar('c1000000-0000-0000-0000-000000000011', 'a0000000-0000-0000-0000-00000000000a', 'ana-11',
                    '5e000000-0000-0000-0000-000000000001'));
SELECT pg_temp.debe_fallar('sc_auth no revoca sin una sesión', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.credenciales_webauthn SET revocada_en = now() WHERE id = 'c1000000-0000-0000-0000-000000000009'$q$],
  'SC_STEP_UP_REQUERIDO');
SELECT pg_temp.debe_fallar('ni con una IP sin sesión', ARRAY[
  $q$UPDATE kyc.credenciales_webauthn SET revocada_en = now(), revocada_ip = '192.0.2.1'
      WHERE id = 'c1000000-0000-0000-0000-000000000009'$q$], 'ck_credenciales_webauthn_revocacion');
SELECT pg_temp.debe_pasar('el dueño del esquema sí (soporte: perdió todos sus dispositivos)', ARRAY[
  $q$UPDATE kyc.credenciales_webauthn SET revocada_en = now() WHERE id = 'c1000000-0000-0000-0000-000000000009'$q$]);
SELECT pg_temp.exigir('PASSKEY_REVOCADA sin sesión ni dispositivo, registrada por el dueño, con las 9 activas', (
  SELECT count(*) = 1 FROM kyc.eventos_autenticacion
   WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND evento = 'PASSKEY_REVOCADA'
     AND sesion_id IS NULL AND ip IS NULL AND dispositivo IS NULL AND detalle = '9'
     AND registrado_por = current_user));
SELECT pg_temp.debe_fallar('una llave no se borra', ARRAY[
  $q$DELETE FROM kyc.credenciales_webauthn WHERE id = 'c1000000-0000-0000-0000-000000000010'$q$], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('ni se vacía la tabla', ARRAY['TRUNCATE kyc.credenciales_webauthn CASCADE'],
  'SC_LEDGER_INMUTABLE');

-- -----------------------------------------------------------------------------
-- Desafíos: 5 minutos, una sola vez
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('sc_auth crea un desafío de inicio de sesión (de nadie) y uno de registro de Ana', ARRAY[
  'SET LOCAL ROLE sc_auth',
  format($q$INSERT INTO kyc.desafios_webauthn (id, proposito, desafio) VALUES ('d1000000-0000-0000-0000-000000000001', 'INICIO', decode(%L, 'hex'))$q$,
         pg_temp.b('desafio 1')),
  format($q$INSERT INTO kyc.desafios_webauthn (id, proposito, desafio, cliente_id, sesion_id)
            VALUES ('d1000000-0000-0000-0000-000000000002', 'REGISTRO', decode(%L, 'hex'),
                    'a0000000-0000-0000-0000-00000000000a', '5e000000-0000-0000-0000-000000000002')$q$,
         pg_temp.b('desafio 2')),
  'RESET ROLE']);
SELECT pg_temp.exigir('vale 5 minutos desde ahora, lo diga quien lo diga', (
  SELECT bool_and(creado_en = now() AND expira_en = now() + interval '5 minutes' AND usado_en IS NULL)
    FROM kyc.desafios_webauthn WHERE id IN ('d1000000-0000-0000-0000-000000000001', 'd1000000-0000-0000-0000-000000000002')));
SELECT pg_temp.debe_fallar('un registro es de una sesión', ARRAY[
  format($q$INSERT INTO kyc.desafios_webauthn (proposito, desafio) VALUES ('REGISTRO', decode(%L, 'hex'))$q$, pg_temp.b('d3'))],
  'ck_desafios_webauthn_dueno');
SELECT pg_temp.debe_fallar('un inicio de sesión no es de nadie todavía', ARRAY[
  format($q$INSERT INTO kyc.desafios_webauthn (proposito, desafio, cliente_id, sesion_id)
            VALUES ('INICIO', decode(%L, 'hex'), 'a0000000-0000-0000-0000-00000000000a', '5e000000-0000-0000-0000-000000000001')$q$,
         pg_temp.b('d3'))], 'ck_desafios_webauthn_dueno');
SELECT pg_temp.debe_fallar('el desafío de Ana no va en la sesión de Beto', ARRAY[
  format($q$INSERT INTO kyc.desafios_webauthn (proposito, desafio, cliente_id, sesion_id)
            VALUES ('STEP_UP', decode(%L, 'hex'), 'a0000000-0000-0000-0000-00000000000a', '5e000000-0000-0000-0000-000000000003')$q$,
         pg_temp.b('d3'))], 'SC_DESAFIO_INCOHERENTE');
SELECT pg_temp.debe_fallar('ni en una sesión vencida', ARRAY[
  format($q$INSERT INTO kyc.desafios_webauthn (proposito, desafio, cliente_id, sesion_id)
            VALUES ('STEP_UP', decode(%L, 'hex'), 'a0000000-0000-0000-0000-00000000000a', '5e000000-0000-0000-0000-000000000005')$q$,
         pg_temp.b('d3'))], 'SC_DESAFIO_INCOHERENTE');
SELECT pg_temp.debe_fallar('de 32 bytes', ARRAY[
  $q$INSERT INTO kyc.desafios_webauthn (proposito, desafio) VALUES ('INICIO', '\x0102')$q$], 'ck_desafios_webauthn_desafio');
SELECT pg_temp.debe_fallar('cada desafío es único', ARRAY[
  format($q$INSERT INTO kyc.desafios_webauthn (proposito, desafio) VALUES ('INICIO', decode(%L, 'hex'))$q$,
         pg_temp.b('desafio 1'))], 'ux_desafios_webauthn_desafio');
SELECT pg_temp.debe_pasar('sc_auth lo usa', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.desafios_webauthn SET usado_en = now() WHERE id = 'd1000000-0000-0000-0000-000000000001'$q$,
  'RESET ROLE']);
SELECT pg_temp.debe_fallar('una sola vez', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.desafios_webauthn SET usado_en = now() WHERE id = 'd1000000-0000-0000-0000-000000000001'$q$],
  'SC_DESAFIO_USADO');
SELECT pg_temp.debe_fallar('ni cambia su desafío', ARRAY[
  format($q$UPDATE kyc.desafios_webauthn SET desafio = decode(%L, 'hex'), usado_en = now()
             WHERE id = 'd1000000-0000-0000-0000-000000000002'$q$, pg_temp.b('otro'))], 'SC_DESAFIO_INMUTABLE');
SELECT pg_temp.debe_fallar('ni se borra mientras vale', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$DELETE FROM kyc.desafios_webauthn WHERE id = 'd1000000-0000-0000-0000-000000000002'$q$], 'SC_DESAFIO_VIGENTE');
-- Uno de hace dos días (lo escribe el dueño del esquema sin sus triggers).
SELECT pg_temp.debe_pasar('un desafío de hace dos días', ARRAY[
  'ALTER TABLE kyc.desafios_webauthn DISABLE TRIGGER trg_desafios_webauthn',
  format($q$INSERT INTO kyc.desafios_webauthn (id, proposito, desafio, creado_en, expira_en)
            VALUES ('d1000000-0000-0000-0000-000000000009', 'INICIO', decode(%L, 'hex'), now() - interval '2 days',
                    now() - interval '2 days' + interval '5 minutes')$q$, pg_temp.b('viejo')),
  'ALTER TABLE kyc.desafios_webauthn ENABLE TRIGGER trg_desafios_webauthn']);
SELECT pg_temp.debe_fallar('vencido, ya no se usa', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.desafios_webauthn SET usado_en = now() WHERE id = 'd1000000-0000-0000-0000-000000000009'$q$],
  'SC_DESAFIO_VENCIDO');
SELECT pg_temp.debe_pasar('vencido hace más de un día, sc_auth lo borra (limpieza)', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$DELETE FROM kyc.desafios_webauthn WHERE id = 'd1000000-0000-0000-0000-000000000009'$q$,
  'RESET ROLE']);
SELECT pg_temp.debe_fallar('la tabla no se vacía', ARRAY['TRUNCATE kyc.desafios_webauthn'], 'SC_LEDGER_INMUTABLE');

-- -----------------------------------------------------------------------------
-- Códigos de recuperación
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana genera 8 códigos desde su sesión con ventana',
  pg_temp.generar('10000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a',
                  '5e000000-0000-0000-0000-000000000001', 'primera'));
SELECT pg_temp.exigir('CODIGOS_RECUPERACION_GENERADOS con la sesión, la IP, el dispositivo y los 8', (
  SELECT count(*) = 1 FROM kyc.eventos_autenticacion
   WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND evento = 'CODIGOS_RECUPERACION_GENERADOS'
     AND sesion_id = '5e000000-0000-0000-0000-000000000001' AND ip = '203.0.113.7'
     AND dispositivo = 'Chrome on macOS' AND detalle = '8'));
SELECT pg_temp.debe_fallar('una generación tiene exactamente 8 (no 7)',
  pg_temp.generar('10000000-0000-0000-0000-000000000009', 'b0000000-0000-0000-0000-00000000000b',
                  '5e000000-0000-0000-0000-000000000003', 'siete', 7), 'SC_LOTE_INCOMPLETO');
SELECT pg_temp.debe_fallar('ni 9',
  pg_temp.generar('10000000-0000-0000-0000-000000000009', 'b0000000-0000-0000-0000-00000000000b',
                  '5e000000-0000-0000-0000-000000000003', 'nueve', 9), 'SC_LOTE_CERRADO');
SELECT pg_temp.debe_fallar('desde una sesión sin ventana, no',
  pg_temp.generar('10000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a',
                  '5e000000-0000-0000-0000-000000000002', 'sin ventana'), 'SC_STEP_UP_REQUERIDO');
SELECT pg_temp.debe_fallar('el personal no tiene códigos de recuperación',
  pg_temp.generar('10000000-0000-0000-0000-000000000009', '0a000000-0000-0000-0000-0000000000a0',
                  '5e000000-0000-0000-0000-000000000004', 'olga'), 'SC_SOLO_CLIENTES');
SELECT pg_temp.debe_fallar('un código no entra en la generación de otro cliente', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.codigos_recuperacion (lote_id, cliente_id, hash)
     VALUES ('10000000-0000-0000-0000-000000000001', 'b0000000-0000-0000-0000-00000000000b', sha256('ajeno'::bytea))$q$],
  'SC_LOTE_CERRADO');
SELECT pg_temp.debe_fallar('ni un noveno en la de Ana', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.codigos_recuperacion (lote_id, cliente_id, hash)
     VALUES ('10000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a', sha256('noveno'::bytea))$q$],
  'SC_LOTE_CERRADO');

SELECT pg_temp.debe_pasar('Ana usa un código en su sesión sin ventana (un inicio de sesión)',
  pg_temp.usar_codigo('a0000000-0000-0000-0000-00000000000a', 'primera-3', '5e000000-0000-0000-0000-000000000002'));
SELECT pg_temp.exigir('usado con la fecha de la base de datos; CODIGO_RECUPERACION_USADO con los 7 que quedan', (
  SELECT usado_en = now() FROM kyc.codigos_recuperacion WHERE hash = sha256('primera-3'::bytea))
  AND (SELECT count(*) = 1 FROM kyc.eventos_autenticacion
        WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND evento = 'CODIGO_RECUPERACION_USADO'
          AND sesion_id = '5e000000-0000-0000-0000-000000000002' AND ip = '198.51.100.4'
          AND dispositivo = 'Safari on iPhone' AND detalle = '7'));
SELECT pg_temp.debe_fallar('un código se usa una sola vez',
  pg_temp.usar_codigo('a0000000-0000-0000-0000-00000000000a', 'primera-3', '5e000000-0000-0000-0000-000000000002'),
  'SC_CODIGO_USADO');
SELECT pg_temp.debe_fallar('en una sesión de su cliente',
  pg_temp.usar_codigo('a0000000-0000-0000-0000-00000000000a', 'primera-4', '5e000000-0000-0000-0000-000000000003'),
  'SC_CODIGO_INCOHERENTE');
SELECT pg_temp.debe_fallar('el hash no cambia', ARRAY[
  $q$UPDATE kyc.codigos_recuperacion SET hash = sha256('otro'::bytea), usado_en = now(),
            usado_sesion_id = '5e000000-0000-0000-0000-000000000001', usado_dispositivo = 'x'
      WHERE hash = sha256('primera-4'::bytea)$q$], 'SC_CODIGO_INMUTABLE');

SELECT pg_temp.debe_pasar('Ana genera otros 8: reemplazan a los primeros',
  pg_temp.generar('10000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a',
                  '5e000000-0000-0000-0000-000000000001', 'segunda'));
SELECT pg_temp.exigir('la primera generación quedó reemplazada; la segunda es la vigente', (
  SELECT reemplazado_en = now() FROM kyc.lotes_recuperacion WHERE id = '10000000-0000-0000-0000-000000000001')
  AND (SELECT reemplazado_en IS NULL FROM kyc.lotes_recuperacion WHERE id = '10000000-0000-0000-0000-000000000002'));
SELECT pg_temp.debe_fallar('un código de la primera ya no vale',
  pg_temp.usar_codigo('a0000000-0000-0000-0000-00000000000a', 'primera-5', '5e000000-0000-0000-0000-000000000001'),
  'SC_CODIGO_REEMPLAZADO');
SELECT pg_temp.debe_pasar('uno de la segunda, sí (en una verificación reforzada)',
  pg_temp.usar_codigo('a0000000-0000-0000-0000-00000000000a', 'segunda-1', '5e000000-0000-0000-0000-000000000001'));
SELECT pg_temp.debe_fallar('una generación no deja de estar reemplazada', ARRAY[
  $q$UPDATE kyc.lotes_recuperacion SET reemplazado_en = NULL WHERE id = '10000000-0000-0000-0000-000000000001'$q$],
  'SC_LOTE_INMUTABLE');
SELECT pg_temp.debe_fallar('ni cambia su dispositivo', ARRAY[
  $q$UPDATE kyc.lotes_recuperacion SET dispositivo = 'Otro' WHERE id = '10000000-0000-0000-0000-000000000002'$q$],
  'SC_LOTE_INMUTABLE');
SELECT pg_temp.debe_fallar('las generaciones no se borran', ARRAY[
  $q$DELETE FROM kyc.lotes_recuperacion WHERE id = '10000000-0000-0000-0000-000000000001'$q$], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('ni los códigos', ARRAY[
  $q$DELETE FROM kyc.codigos_recuperacion WHERE lote_id = '10000000-0000-0000-0000-000000000001'$q$], 'SC_LEDGER_INMUTABLE');
-- Una generación de otra transacción: ya no admite códigos.
SELECT pg_temp.debe_pasar('una generación de Beto de hace un día (con sus 8)', ARRAY[
  'ALTER TABLE kyc.lotes_recuperacion DISABLE TRIGGER USER',
  'ALTER TABLE kyc.codigos_recuperacion DISABLE TRIGGER USER',
  $q$INSERT INTO kyc.lotes_recuperacion (id, cliente_id, sesion_id, dispositivo, creado_en)
     VALUES ('10000000-0000-0000-0000-000000000003', 'b0000000-0000-0000-0000-00000000000b',
             '5e000000-0000-0000-0000-000000000003', 'Firefox on Linux', now() - interval '1 day')$q$,
  $q$INSERT INTO kyc.codigos_recuperacion (lote_id, cliente_id, hash, creado_en)
     SELECT '10000000-0000-0000-0000-000000000003', 'b0000000-0000-0000-0000-00000000000b',
            sha256(convert_to('beto-' || i, 'UTF8')), now() - interval '1 day' FROM generate_series(1, 8) i$q$,
  'ALTER TABLE kyc.lotes_recuperacion ENABLE TRIGGER USER',
  'ALTER TABLE kyc.codigos_recuperacion ENABLE TRIGGER USER']);
SELECT pg_temp.debe_fallar('a una generación ya cerrada no se le agregan códigos', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.codigos_recuperacion (lote_id, cliente_id, hash)
     VALUES ('10000000-0000-0000-0000-000000000003', 'b0000000-0000-0000-0000-00000000000b', sha256('tarde'::bytea))$q$],
  'SC_LOTE_CERRADO');

-- -----------------------------------------------------------------------------
-- El cliente lee lo suyo
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('Ana ve sus 11 llaves (dos revocadas), sus 2 generaciones y sus 16 códigos; nada de Beto', (
  pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM kyc.credenciales_webauthn') = 11
  AND pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM kyc.lotes_recuperacion') = 2
  AND pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM kyc.codigos_recuperacion') = 16
  AND pg_temp.visibles('a0000000-0000-0000-0000-00000000000a',
                       $q$SELECT count(*) FROM kyc.lotes_recuperacion WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b'$q$) = 0));
SELECT pg_temp.exigir('Beto no ve las llaves de Ana; sin cliente actual, nada', (
  pg_temp.visibles('b0000000-0000-0000-0000-00000000000b', 'SELECT count(*) FROM kyc.credenciales_webauthn') = 0
  AND pg_temp.visibles('b0000000-0000-0000-0000-00000000000b', 'SELECT count(*) FROM kyc.codigos_recuperacion') = 8
  AND pg_temp.visibles('', 'SELECT count(*) FROM kyc.credenciales_webauthn') = 0));

\o
\echo tests/db/0027_llaves_acceso_recuperacion.sql: todas las pruebas pasaron (ROLLBACK)
ROLLBACK;
