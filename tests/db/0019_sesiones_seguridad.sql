-- =============================================================================
--  Pruebas de la migración 0019 (sesiones por dispositivo y centro de
--  seguridad). Corren en una transacción que termina en ROLLBACK: no dejan
--  datos.
--
--    psql "$DATABASE_URL" -f tests/db/0019_sesiones_seguridad.sql
--
--  Lo que la base de datos garantiza por sí sola: una sesión nace vigente, con
--  un dispositivo resumido y las fechas de la base de datos; de ella solo
--  cambian el último uso (hacia adelante) y, una vez y con su motivo, la
--  revocación; nunca se borra. La preferencia de inactividad es 5, 15, 30 o
--  60 minutos. La bitácora acepta los eventos nuevos, con un detalle que solo
--  es un número, y sigue inmutable. sc_auth escribe, sc_kyc lee lo que valida
--  cada petición y el cliente lee lo suyo y nada más. backend/tests/sesiones.rs
--  cubre los tokens, la validación y los endpoints.
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

-- Sin bloque EXCEPTION: las sentencias corren en la transacción principal.
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
-- Datos: dos clientes
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ludwig', 'von Mises', 'ludwig@example.mx', 'VERIFICADO', now(), 'BAJO'),
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Friedrich', 'Hayek', 'friedrich@example.mx', 'VERIFICADO', now(), 'BAJO');
INSERT INTO kyc.credenciales (cliente_id, password_hash, rol) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'x', 'CLIENTE'),
  ('b0000000-0000-0000-0000-00000000000b', 'x', 'CLIENTE');

-- -----------------------------------------------------------------------------
-- 1. Una sesión nace vigente, con un dispositivo resumido y las fechas de la base
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('sc_auth abre dos sesiones de Ludwig y una de Friedrich', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo, ip) VALUES
       ('5e000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a',
        now() + interval '60 minutes', FALSE, 'Chrome on macOS', '203.0.113.7'),
       ('5e000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a',
        now() + interval '60 minutes', TRUE, 'Safari on iPhone', '2001:db8::7'),
       ('5e000000-0000-0000-0000-000000000003', 'b0000000-0000-0000-0000-00000000000b',
        now() + interval '60 minutes', FALSE, 'Firefox on Linux', NULL)$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('nacen vigentes, abiertas y usadas en el mismo instante, el de la base de datos', (
  SELECT count(*) = 3 AND bool_and(revocada_en IS NULL AND motivo_revocacion IS NULL)
     AND bool_and(creada_en = now() AND ultimo_uso_en = now())
    FROM kyc.sesiones));

SELECT pg_temp.debe_fallar('sc_auth no elige cuándo se abrió una sesión', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.sesiones (id, cliente_id, creada_en, expira_en, mfa, dispositivo)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', now() - interval '1 day',
             now() + interval '60 minutes', FALSE, 'Chrome on macOS')$q$
], 'permission denied');
SELECT pg_temp.debe_fallar('ni su último uso', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.sesiones (id, cliente_id, ultimo_uso_en, expira_en, mfa, dispositivo)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', now() + interval '1 day',
             now() + interval '60 minutes', FALSE, 'Chrome on macOS')$q$
], 'permission denied');
SELECT pg_temp.debe_fallar('ni abre una sesión ya revocada', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo, revocada_en, motivo_revocacion)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', now() + interval '60 minutes', FALSE,
             'Chrome on macOS', now(), 'CIERRE')$q$
], 'permission denied');
SELECT pg_temp.debe_fallar('sin dispositivo no hay sesión', ARRAY[
  $q$INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', now() + interval '60 minutes', FALSE, '')$q$
], 'ck_sesiones_dispositivo');
SELECT pg_temp.debe_fallar('el dispositivo es un resumen, no el User-Agent completo (máximo 80)', ARRAY[
  $q$INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', now() + interval '60 minutes', FALSE,
             'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0')$q$
], 'ck_sesiones_dispositivo');
SELECT pg_temp.debe_fallar('una sesión vence después de abrirse', ARRAY[
  $q$INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', now(), FALSE, 'Chrome on macOS')$q$
], 'ck_sesiones_vigencia');
SELECT pg_temp.debe_fallar('no se usa antes de abrirse', ARRAY[
  $q$INSERT INTO kyc.sesiones (id, cliente_id, ultimo_uso_en, expira_en, mfa, dispositivo)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', now() - interval '1 second',
             now() + interval '60 minutes', FALSE, 'Chrome on macOS')$q$
], 'ck_sesiones_uso');
SELECT pg_temp.debe_fallar('una revocación lleva su motivo', ARRAY[
  $q$INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo, revocada_en)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', now() + interval '60 minutes', FALSE,
             'Chrome on macOS', now())$q$
], 'ck_sesiones_revocacion');
SELECT pg_temp.debe_fallar('y un motivo, su revocación', ARRAY[
  $q$INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo, motivo_revocacion)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', now() + interval '60 minutes', FALSE,
             'Chrome on macOS', 'CIERRE')$q$
], 'ck_sesiones_revocacion');
SELECT pg_temp.debe_fallar('solo motivos del catálogo', ARRAY[
  $q$INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo, revocada_en, motivo_revocacion)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', now() + interval '60 minutes', FALSE,
             'Chrome on macOS', now(), 'CADUCADA')$q$
], 'ck_sesiones_motivo');
SELECT pg_temp.debe_fallar('solo de un cliente que existe', ARRAY[
  $q$INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo)
     VALUES (gen_random_uuid(), 'f0000000-0000-0000-0000-00000000000f', now() + interval '60 minutes', FALSE,
             'Chrome on macOS')$q$
], 'foreign key');
SELECT pg_temp.debe_fallar('un sid no se repite', ARRAY[
  $q$INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo)
     VALUES ('5e000000-0000-0000-0000-000000000001', 'b0000000-0000-0000-0000-00000000000b',
             now() + interval '60 minutes', FALSE, 'Chrome on macOS')$q$
], 'duplicate key');

-- -----------------------------------------------------------------------------
-- 2. Solo cambian el último uso, hacia adelante, y una vez la revocación
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('sc_auth registra el uso de una sesión', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.sesiones SET ultimo_uso_en = now() + interval '5 minutes'
      WHERE id = '5e000000-0000-0000-0000-000000000001'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_fallar('el último uso no retrocede (ni para el dueño del esquema)', ARRAY[
  $q$UPDATE kyc.sesiones SET ultimo_uso_en = now() + interval '1 minute'
      WHERE id = '5e000000-0000-0000-0000-000000000001'$q$
], 'SC_SESION_INMUTABLE');
SELECT pg_temp.debe_fallar('una sesión no se alarga', ARRAY[
  $q$UPDATE kyc.sesiones SET expira_en = expira_en + interval '1 day'
      WHERE id = '5e000000-0000-0000-0000-000000000001'$q$
], 'SC_SESION_INMUTABLE');
SELECT pg_temp.debe_fallar('ni gana el segundo factor después', ARRAY[
  $q$UPDATE kyc.sesiones SET mfa = TRUE WHERE id = '5e000000-0000-0000-0000-000000000001'$q$
], 'SC_SESION_INMUTABLE');
SELECT pg_temp.debe_fallar('ni cambia de dispositivo, de IP o de dueño', ARRAY[
  $q$UPDATE kyc.sesiones SET dispositivo = 'Edge on Windows', ip = '198.51.100.1'
      WHERE id = '5e000000-0000-0000-0000-000000000001'$q$
], 'SC_SESION_INMUTABLE');
SELECT pg_temp.debe_fallar('de dueño tampoco', ARRAY[
  $q$UPDATE kyc.sesiones SET cliente_id = 'b0000000-0000-0000-0000-00000000000b'
      WHERE id = '5e000000-0000-0000-0000-000000000001'$q$
], 'SC_SESION_INMUTABLE');
SELECT pg_temp.debe_fallar('sc_auth no tiene permiso para alargarla', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.sesiones SET expira_en = expira_en + interval '1 day'
      WHERE id = '5e000000-0000-0000-0000-000000000001'$q$
], 'permission denied');
SELECT pg_temp.debe_pasar('sc_auth revoca una sesión, con su motivo', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.sesiones SET revocada_en = now(), motivo_revocacion = 'REVOCADA'
      WHERE id = '5e000000-0000-0000-0000-000000000002'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_fallar('una revocación es definitiva: la sesión no revive', ARRAY[
  $q$UPDATE kyc.sesiones SET revocada_en = NULL, motivo_revocacion = NULL
      WHERE id = '5e000000-0000-0000-0000-000000000002'$q$
], 'SC_SESION_INMUTABLE');
SELECT pg_temp.debe_pasar('una sesión abandonada se revoca por inactividad', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.sesiones SET revocada_en = now(), motivo_revocacion = 'INACTIVIDAD'
      WHERE id = '5e000000-0000-0000-0000-000000000003'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_fallar('ni cambia su motivo', ARRAY[
  $q$UPDATE kyc.sesiones SET motivo_revocacion = 'OPERADOR' WHERE id = '5e000000-0000-0000-0000-000000000002'$q$
], 'SC_SESION_INMUTABLE');
SELECT pg_temp.debe_fallar('ni su fecha', ARRAY[
  $q$UPDATE kyc.sesiones SET revocada_en = now() + interval '1 hour'
      WHERE id = '5e000000-0000-0000-0000-000000000002'$q$
], 'SC_SESION_INMUTABLE');
SELECT pg_temp.debe_fallar('una sesión no se borra (ni por el dueño del esquema)', ARRAY[
  $q$DELETE FROM kyc.sesiones WHERE id = '5e000000-0000-0000-0000-000000000002'$q$
], 'SC_SESION_INMUTABLE');
SELECT pg_temp.debe_fallar('sc_auth no tiene permiso para borrarla', ARRAY[
  'SET LOCAL ROLE sc_auth', 'DELETE FROM kyc.sesiones'
], 'permission denied');
SELECT pg_temp.debe_fallar('la tabla no se vacía', ARRAY['TRUNCATE kyc.sesiones CASCADE'], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.exigir('Ludwig: una sesión vigente usada hace nada y una revocada', (
  SELECT count(*) FILTER (WHERE revocada_en IS NULL AND ultimo_uso_en = now() + interval '5 minutes') = 1
     AND count(*) FILTER (WHERE motivo_revocacion = 'REVOCADA') = 1
    FROM kyc.sesiones WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a'));

-- -----------------------------------------------------------------------------
-- 3. Cierre por inactividad: 5, 15, 30 o 60 minutos
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('ni 10 minutos', ARRAY[
  $q$INSERT INTO kyc.preferencias_seguridad (cliente_id, inactividad_minutos)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 10)$q$
], 'ck_preferencias_inactividad');
SELECT pg_temp.debe_fallar('ni sin cierre', ARRAY[
  $q$INSERT INTO kyc.preferencias_seguridad (cliente_id, inactividad_minutos)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 0)$q$
], 'ck_preferencias_inactividad');
SELECT pg_temp.debe_pasar('sc_auth guarda 15 minutos y luego 5', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.preferencias_seguridad (cliente_id, inactividad_minutos)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 15)
     ON CONFLICT (cliente_id) DO UPDATE SET inactividad_minutos = EXCLUDED.inactividad_minutos$q$,
  $q$INSERT INTO kyc.preferencias_seguridad (cliente_id, inactividad_minutos)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 5)
     ON CONFLICT (cliente_id) DO UPDATE SET inactividad_minutos = EXCLUDED.inactividad_minutos$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('una fila por cliente, con lo último que eligió', (
  SELECT count(*) = 1 AND min(inactividad_minutos) = 5 FROM kyc.preferencias_seguridad));
SELECT pg_temp.debe_fallar('sc_auth no elige la fecha del cambio', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.preferencias_seguridad SET actualizado_en = now() - interval '1 day'$q$
], 'permission denied');
SELECT pg_temp.debe_fallar('ni borra la preferencia', ARRAY[
  'SET LOCAL ROLE sc_auth', 'DELETE FROM kyc.preferencias_seguridad'
], 'permission denied');

-- -----------------------------------------------------------------------------
-- 4. La bitácora: los eventos nuevos, un detalle numérico, inmutable
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('sc_auth registra un inicio de sesión con su sesión y dispositivo, y una revocación de 3', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, sesion_id, dispositivo)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'INICIO_SESION', '203.0.113.7',
             '5e000000-0000-0000-0000-000000000001', 'Chrome on macOS')$q$,
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, sesion_id, dispositivo, detalle)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'SESIONES_REVOCADAS', '203.0.113.7',
             '5e000000-0000-0000-0000-000000000001', 'Chrome on macOS', '3')$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_pasar('el catálogo: los 7 eventos de 0010 y los 9 de 0019', ARRAY[
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento)
     SELECT 'b0000000-0000-0000-0000-00000000000b', e FROM unnest(ARRAY[
       'MFA_SECRETO_GENERADO', 'MFA_ACTIVADO', 'MFA_VERIFICADO', 'MFA_CODIGO_RECHAZADO', 'MFA_DESACTIVADO',
       'RESTABLECIMIENTO_SOLICITADO', 'CONTRASENA_RESTABLECIDA', 'INICIO_SESION', 'INICIO_FALLIDO',
       'CUENTA_BLOQUEADA', 'SESION_CERRADA', 'SESION_REVOCADA', 'SESIONES_REVOCADAS', 'CONTRASENA_CAMBIADA',
       'CAMBIO_CONTRASENA_RECHAZADO', 'PREFERENCIAS_ACTUALIZADAS']) AS e$q$
]);
SELECT pg_temp.debe_fallar('ningún evento fuera del catálogo', ARRAY[
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento) VALUES ('a0000000-0000-0000-0000-00000000000a', 'LOGOUT')$q$
], 'ck_eventos_autenticacion_evento');
SELECT pg_temp.debe_fallar('el detalle es un número, no texto libre', ARRAY[
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, detalle)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'SESIONES_REVOCADAS', 'tres sesiones')$q$
], 'ck_eventos_autenticacion_detalle');
SELECT pg_temp.debe_fallar('de seis cifras como máximo', ARRAY[
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, detalle)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'SESIONES_REVOCADAS', '1234567')$q$
], 'ck_eventos_autenticacion_detalle');
SELECT pg_temp.debe_fallar('un dispositivo vacío no se registra', ARRAY[
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, dispositivo)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'INICIO_SESION', '')$q$
], 'ck_eventos_autenticacion_dispositivo');
SELECT pg_temp.debe_fallar('la sesión de un evento existe', ARRAY[
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, sesion_id)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'SESION_CERRADA', 'f0000000-0000-0000-0000-00000000000f')$q$
], 'foreign key');
SELECT pg_temp.debe_fallar('un evento nuevo tampoco se corrige', ARRAY[
  $q$UPDATE kyc.eventos_autenticacion SET detalle = '9' WHERE evento = 'SESIONES_REVOCADAS'$q$
], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('ni se borra', ARRAY[
  $q$DELETE FROM kyc.eventos_autenticacion WHERE evento = 'INICIO_SESION'$q$
], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('sc_auth sigue sin leer la bitácora', ARRAY[
  'SET LOCAL ROLE sc_auth', 'SELECT detalle FROM kyc.eventos_autenticacion'
], 'permission denied');
SELECT pg_temp.exigir('quedan a nombre de quien los registró', (
  SELECT bool_and(registrado_por = 'sc_auth') FROM kyc.eventos_autenticacion
   WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a'));

-- -----------------------------------------------------------------------------
-- 5. sc_kyc lee lo que valida cada petición, y nada más
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('sc_kyc lee la vigencia de la sesión y la inactividad permitida', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  'SELECT id, cliente_id, expira_en, ultimo_uso_en, revocada_en FROM kyc.sesiones',
  'SELECT cliente_id, inactividad_minutos FROM kyc.preferencias_seguridad',
  'RESET ROLE'
]);
SELECT pg_temp.debe_fallar('sc_kyc no lee el dispositivo ni la IP', ARRAY[
  'SET LOCAL ROLE sc_kyc', 'SELECT dispositivo, ip FROM kyc.sesiones'
], 'permission denied');
SELECT pg_temp.debe_fallar('sc_kyc no abre sesiones', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', now() + interval '60 minutes', FALSE,
             'Chrome on macOS')$q$
], 'permission denied');
SELECT pg_temp.debe_fallar('ni las revoca', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.sesiones SET revocada_en = now(), motivo_revocacion = 'OPERADOR'$q$
], 'permission denied');
-- 0025: sc_ifpe y sc_spei escriben en la bitácora los eventos de las
-- aclaraciones SPEI y ningún otro (sus políticas, en 0025_aclaraciones_spei.sql);
-- no la leen ni la cambian.
SELECT pg_temp.exigir('los demás roles de la aplicación no tocan sesiones, preferencias ni bitácora', (
  SELECT bool_and(NOT has_any_column_privilege(r, t, 'SELECT, INSERT, UPDATE, REFERENCES'))
    FROM unnest(ARRAY['sc_ifpe', 'sc_sofom', 'sc_puente', 'sc_spei', 'sc_mercado', 'sc_aml', 'sc_auditor',
                      'sc_auditor_pld', 'sc_kyc_verificador']) AS r,
         unnest(ARRAY['kyc.sesiones', 'kyc.preferencias_seguridad', 'kyc.eventos_autenticacion']) AS t
   WHERE NOT (r IN ('sc_ifpe', 'sc_spei') AND t = 'kyc.eventos_autenticacion'))
  AND NOT has_any_column_privilege('sc_ifpe', 'kyc.eventos_autenticacion', 'SELECT, UPDATE, REFERENCES')
  AND NOT has_any_column_privilege('sc_spei', 'kyc.eventos_autenticacion', 'SELECT, UPDATE, REFERENCES'));

-- -----------------------------------------------------------------------------
-- 6. El cliente lee lo suyo y nada más (RLS, como en 0011)
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('RLS en las tres tablas, con la política RESTRICTIVE de sc_cliente', (
  SELECT count(*) = 3 FROM pg_class
   WHERE relrowsecurity AND oid IN ('kyc.sesiones'::regclass, 'kyc.preferencias_seguridad'::regclass,
                                    'kyc.eventos_autenticacion'::regclass))
  AND (SELECT count(*) = 3 FROM pg_policies
        WHERE schemaname = 'kyc' AND tablename IN ('sesiones', 'preferencias_seguridad', 'eventos_autenticacion')
          AND policyname = 'solo_del_cliente' AND permissive = 'RESTRICTIVE' AND cmd = 'ALL'
          AND roles = '{sc_cliente}'));
SELECT pg_temp.exigir('sc_cliente solo tiene SELECT, columna por columna, en las tres tablas', (
  SELECT count(*) > 0 AND bool_and(privilege_type = 'SELECT')
    FROM information_schema.column_privileges
   WHERE grantee = 'sc_cliente' AND table_schema = 'kyc'
     AND table_name IN ('sesiones', 'preferencias_seguridad', 'eventos_autenticacion'))
  AND NOT EXISTS (SELECT 1 FROM information_schema.role_table_grants
                   WHERE grantee = 'sc_cliente' AND table_schema = 'kyc'
                     AND table_name IN ('sesiones', 'preferencias_seguridad', 'eventos_autenticacion')));
SELECT pg_temp.exigir('Ludwig ve sus 2 sesiones, Friedrich la suya; sin cliente, ninguna', (
  pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM kyc.sesiones') = 2
  AND pg_temp.visibles('b0000000-0000-0000-0000-00000000000b', 'SELECT count(*) FROM kyc.sesiones') = 1
  AND pg_temp.visibles('', 'SELECT count(*) FROM kyc.sesiones') = 0));
SELECT pg_temp.exigir('ni pidiendo la sesión de otro por su id', (
  pg_temp.visibles('b0000000-0000-0000-0000-00000000000b',
                   $q$SELECT count(*) FROM kyc.sesiones WHERE id = '5e000000-0000-0000-0000-000000000001'$q$) = 0));
SELECT pg_temp.exigir('cada uno ve sus eventos y su preferencia', (
  pg_temp.visibles('a0000000-0000-0000-0000-00000000000a',
                   'SELECT count(*) FROM kyc.eventos_autenticacion WHERE cliente_id <> nucleo.cliente_actual()') = 0
  AND pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM kyc.eventos_autenticacion') = 2
  AND pg_temp.visibles('b0000000-0000-0000-0000-00000000000b', 'SELECT count(*) FROM kyc.eventos_autenticacion') = 16
  AND pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM kyc.preferencias_seguridad') = 1
  AND pg_temp.visibles('b0000000-0000-0000-0000-00000000000b', 'SELECT count(*) FROM kyc.preferencias_seguridad') = 0));
SELECT pg_temp.debe_fallar('el cliente no lee quién registró un evento', ARRAY[
  $q$SELECT set_config('sc.cliente_actual', 'a0000000-0000-0000-0000-00000000000a', true)$q$,
  'SET LOCAL ROLE sc_cliente', 'SELECT registrado_por FROM kyc.eventos_autenticacion'
], 'permission denied');
SELECT pg_temp.debe_fallar('el cliente no revoca su sesión directamente', ARRAY[
  $q$SELECT set_config('sc.cliente_actual', 'a0000000-0000-0000-0000-00000000000a', true)$q$,
  'SET LOCAL ROLE sc_cliente',
  $q$UPDATE kyc.sesiones SET revocada_en = now(), motivo_revocacion = 'CIERRE'
      WHERE id = '5e000000-0000-0000-0000-000000000001'$q$
], 'permission denied');
SELECT pg_temp.debe_fallar('ni abre una', ARRAY[
  $q$SELECT set_config('sc.cliente_actual', 'a0000000-0000-0000-0000-00000000000a', true)$q$,
  'SET LOCAL ROLE sc_cliente',
  $q$INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', now() + interval '60 minutes', TRUE,
             'Chrome on macOS')$q$
], 'permission denied');
SELECT pg_temp.debe_fallar('ni cambia su preferencia', ARRAY[
  $q$SELECT set_config('sc.cliente_actual', 'a0000000-0000-0000-0000-00000000000a', true)$q$,
  'SET LOCAL ROLE sc_cliente', 'UPDATE kyc.preferencias_seguridad SET inactividad_minutos = 60'
], 'permission denied');
SELECT pg_temp.debe_fallar('ni escribe en la bitácora', ARRAY[
  $q$SELECT set_config('sc.cliente_actual', 'a0000000-0000-0000-0000-00000000000a', true)$q$,
  'SET LOCAL ROLE sc_cliente',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'SESION_CERRADA')$q$
], 'permission denied');

\o
\echo 'tests/db/0019_sesiones_seguridad.sql: todas las pruebas pasaron (ROLLBACK)'
ROLLBACK;
