-- =============================================================================
--  Pruebas de la migración 0022 (verificación reforzada antes de las acciones
--  sensibles). Corren en una transacción que termina en ROLLBACK: no dejan
--  datos.
--
--    psql "$DATABASE_URL" -f tests/db/0022_verificacion_reforzada.sql
--
--  Lo que la base de datos garantiza por sí sola: una sesión nace sin ventana;
--  escribir la ventana la deja en 5 minutos desde ahora (o en el vencimiento
--  de la sesión, si llega antes), sea cual sea el valor escrito, y solo en una
--  sesión vigente; NULL la cierra. Cada ventana abierta deja
--  STEP_UP_VERIFICADO en la bitácora del cliente, con la IP y el dispositivo
--  de la petición y los segundos de la ventana, y sc_auth no registra uno sin
--  su ventana. Una designación de beneficiarios nace en una sesión vigente del
--  mismo cliente con su ventana abierta, la escriba quien la escriba. sc_auth
--  abre la ventana, sc_kyc la lee y nadie más la toca.
--  backend/tests/step_up.rs cubre el endpoint, el bloqueo y las rutas.
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

-- Abre la ventana de una sesión como lo hace el backend (sc_auth), con la IP y
-- el dispositivo de la petición cuando se dan.
CREATE FUNCTION pg_temp.verificar(sesion TEXT, ip TEXT DEFAULT NULL, dispositivo TEXT DEFAULT NULL)
RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
        'SET LOCAL ROLE sc_auth',
        format('SELECT set_config(%L, %L, true), set_config(%L, %L, true)',
               'sc.origen_ip', coalesce(ip, ''), 'sc.origen_dispositivo', coalesce(dispositivo, '')),
        format('UPDATE kyc.sesiones SET step_up_valido_hasta = now() WHERE id = %L', sesion),
        $q$SELECT set_config('sc.origen_ip', '', true), set_config('sc.origen_dispositivo', '', true)$q$,
        'RESET ROLE']
$$;

-- Una designación de un beneficiario al 100%, como la guarda el backend
-- (sc_kyc) desde la sesión `sesion`; con `rol` NULL, el dueño del esquema.
CREATE FUNCTION pg_temp.designacion(id TEXT, cliente TEXT, sesion TEXT, rol TEXT DEFAULT 'sc_kyc')
RETURNS TEXT[]
LANGUAGE plpgsql AS $$
DECLARE
    sentencias TEXT[] := ARRAY[]::TEXT[];
BEGIN
    IF rol IS NOT NULL THEN
        sentencias := ARRAY[format('SET LOCAL ROLE %I', rol)];
    END IF;
    sentencias := sentencias || format(
        'INSERT INTO kyc.beneficiarios (designacion_id, posicion, nombre_completo, parentesco, curp, fecha_nacimiento, porcentaje) VALUES (%L, 1, %L, %L, NULL, %L, 100)',
        id, 'JOSÉ LÓPEZ GARCÍA', 'HIJO', '2005-06-07');
    sentencias := sentencias || format(
        'INSERT INTO kyc.designaciones_beneficiarios (id, cliente_id, num_beneficiarios, total_porcentaje, sesion_id, ip, dispositivo) VALUES (%L, %L, 1, 100, %L, %L, %L)',
        id, cliente, sesion, '203.0.113.7', 'Chrome on macOS');
    RETURN sentencias || 'RESET ROLE'::TEXT;
END
$$;

-- -----------------------------------------------------------------------------
-- Datos: Ludwig y Friedrich verificados, Carl pendiente; sus sesiones
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ludwig', 'von Mises', 'ludwig@example.mx', 'VERIFICADO', now(), 'BAJO'),
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Friedrich', 'Hayek', 'friedrich@example.mx', 'VERIFICADO', now(), 'BAJO'),
  ('c0000000-0000-0000-0000-00000000000c', 'FISICA', 'Carl', 'Menger', 'carl@example.mx', 'PENDIENTE', NULL, 'BAJO');

-- 1, 4, 6: Ludwig, una hora. 2: Ludwig, vence en 2 minutos. 3: Friedrich, sin
-- IP. 7: Carl. 5: Ludwig, vencida hace una hora (la abre el dueño del esquema:
-- sc_auth no elige fechas).
SELECT pg_temp.debe_pasar('sc_auth abre seis sesiones', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo, ip) VALUES
       ('5e000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a',
        now() + interval '60 minutes', FALSE, 'Chrome on macOS', '203.0.113.7'),
       ('5e000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a',
        now() + interval '2 minutes', TRUE, 'Safari on iPhone', '2001:db8::7'),
       ('5e000000-0000-0000-0000-000000000003', 'b0000000-0000-0000-0000-00000000000b',
        now() + interval '60 minutes', FALSE, 'Firefox on Linux', NULL),
       ('5e000000-0000-0000-0000-000000000004', 'a0000000-0000-0000-0000-00000000000a',
        now() + interval '60 minutes', FALSE, 'Edge on Windows', '198.51.100.4'),
       ('5e000000-0000-0000-0000-000000000006', 'a0000000-0000-0000-0000-00000000000a',
        now() + interval '60 minutes', FALSE, 'Chrome on Android', '203.0.113.9'),
       ('5e000000-0000-0000-0000-000000000007', 'c0000000-0000-0000-0000-00000000000c',
        now() + interval '60 minutes', FALSE, 'Firefox on Windows', '192.0.2.70')$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_pasar('y una que ya venció', ARRAY[
  $q$INSERT INTO kyc.sesiones (id, cliente_id, creada_en, ultimo_uso_en, expira_en, mfa, dispositivo) VALUES
       ('5e000000-0000-0000-0000-000000000005', 'a0000000-0000-0000-0000-00000000000a',
        now() - interval '2 hours', now() - interval '2 hours', now() - interval '1 hour', FALSE, 'Opera on Linux')$q$
]);

-- -----------------------------------------------------------------------------
-- 1. Una sesión nace sin ventana
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('ninguna sesión nace con la ventana abierta', (
  SELECT count(*) = 7 AND bool_and(step_up_valido_hasta IS NULL) FROM kyc.sesiones));
SELECT pg_temp.debe_fallar('sc_auth no abre una sesión con la ventana puesta', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo, step_up_valido_hasta)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', now() + interval '60 minutes', FALSE,
             'Chrome on macOS', now() + interval '5 minutes')$q$
], 'permission denied');
SELECT pg_temp.debe_fallar('ni el dueño del esquema: iniciar sesión no es verificar de nuevo', ARRAY[
  $q$INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo, step_up_valido_hasta)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', now() + interval '60 minutes', FALSE,
             'Chrome on macOS', now() + interval '5 minutes')$q$
], 'SC_STEP_UP_INVALIDO');

-- -----------------------------------------------------------------------------
-- 2. Abrir la ventana: la fecha es de la base de datos, y solo en una sesión vigente
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('sc_auth abre la ventana de la sesión 1 desde otra IP y otro dispositivo',
  pg_temp.verificar('5e000000-0000-0000-0000-000000000001', '198.51.100.23', 'Chrome on Windows'));
SELECT pg_temp.exigir('la ventana dura 5 minutos desde ahora', (
  SELECT step_up_valido_hasta = now() + interval '5 minutes'
    FROM kyc.sesiones WHERE id = '5e000000-0000-0000-0000-000000000001')
  AND kyc.vigencia_step_up() = interval '5 minutes');
SELECT pg_temp.exigir('y deja STEP_UP_VERIFICADO con la sesión, la IP y el dispositivo de la petición, 300 segundos, a nombre de sc_auth', (
  SELECT count(*) = 1
     AND bool_and(cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND ip = '198.51.100.23'
                  AND dispositivo = 'Chrome on Windows' AND detalle = '300' AND registrado_por = 'sc_auth')
    FROM kyc.eventos_autenticacion
   WHERE evento = 'STEP_UP_VERIFICADO' AND sesion_id = '5e000000-0000-0000-0000-000000000001'));
SELECT pg_temp.debe_pasar('sc_auth escribe una fecha lejana en la sesión 3', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.sesiones SET step_up_valido_hasta = '2099-01-01' WHERE id = '5e000000-0000-0000-0000-000000000003'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('la base de datos la reemplaza; sin datos de la petición, el evento lleva los de la sesión', (
  SELECT step_up_valido_hasta = now() + interval '5 minutes'
    FROM kyc.sesiones WHERE id = '5e000000-0000-0000-0000-000000000003')
  AND (SELECT count(*) = 1 AND bool_and(ip IS NULL AND dispositivo = 'Firefox on Linux' AND detalle = '300')
         FROM kyc.eventos_autenticacion
        WHERE evento = 'STEP_UP_VERIFICADO' AND sesion_id = '5e000000-0000-0000-0000-000000000003'));
SELECT pg_temp.debe_pasar('sc_auth abre la ventana de la sesión 2, que vence en 2 minutos',
  pg_temp.verificar('5e000000-0000-0000-0000-000000000002'));
SELECT pg_temp.exigir('una ventana no pasa del vencimiento de su sesión: 120 segundos', (
  SELECT s.step_up_valido_hasta = s.expira_en AND e.detalle = '120'
     AND e.ip = '2001:db8::7' AND e.dispositivo = 'Safari on iPhone'
    FROM kyc.sesiones s
    JOIN kyc.eventos_autenticacion e ON e.sesion_id = s.id AND e.evento = 'STEP_UP_VERIFICADO'
   WHERE s.id = '5e000000-0000-0000-0000-000000000002'));
SELECT pg_temp.debe_pasar('escribir el mismo valor otra vez', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.sesiones SET step_up_valido_hasta = step_up_valido_hasta
      WHERE id = '5e000000-0000-0000-0000-000000000001'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('también cuenta como una verificación, con su evento', (
  SELECT count(*) = 2 FROM kyc.eventos_autenticacion
   WHERE evento = 'STEP_UP_VERIFICADO' AND sesion_id = '5e000000-0000-0000-0000-000000000001'));
SELECT pg_temp.debe_pasar('sc_auth revoca la sesión 4', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.sesiones SET revocada_en = now(), motivo_revocacion = 'REVOCADA'
      WHERE id = '5e000000-0000-0000-0000-000000000004'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_fallar('una sesión revocada no abre ventana',
  pg_temp.verificar('5e000000-0000-0000-0000-000000000004'), 'SC_STEP_UP_SESION_NO_VIGENTE');
SELECT pg_temp.debe_fallar('ni una vencida',
  pg_temp.verificar('5e000000-0000-0000-0000-000000000005'), 'SC_STEP_UP_SESION_NO_VIGENTE');
SELECT pg_temp.debe_fallar('ni la que se revoca en la misma sentencia', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.sesiones SET revocada_en = now(), motivo_revocacion = 'BLOQUEO', step_up_valido_hasta = now()
      WHERE id = '5e000000-0000-0000-0000-000000000006'$q$
], 'SC_STEP_UP_SESION_NO_VIGENTE');
SELECT pg_temp.debe_fallar('ni para el dueño del esquema', ARRAY[
  $q$UPDATE kyc.sesiones SET step_up_valido_hasta = now() WHERE id = '5e000000-0000-0000-0000-000000000005'$q$
], 'SC_STEP_UP_SESION_NO_VIGENTE');
SELECT pg_temp.debe_fallar('sin el trigger, el CHECK impide una ventana más larga que la sesión', ARRAY[
  'ALTER TABLE kyc.sesiones DISABLE TRIGGER trg_sesiones_step_up',
  $q$UPDATE kyc.sesiones SET step_up_valido_hasta = expira_en + interval '1 second'
      WHERE id = '5e000000-0000-0000-0000-000000000001'$q$
], 'ck_sesiones_step_up');
SELECT pg_temp.debe_pasar('NULL cierra la ventana de la sesión 3', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.sesiones SET step_up_valido_hasta = NULL WHERE id = '5e000000-0000-0000-0000-000000000003'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('sin un evento nuevo', (
  SELECT step_up_valido_hasta IS NULL FROM kyc.sesiones WHERE id = '5e000000-0000-0000-0000-000000000003')
  AND (SELECT count(*) = 1 FROM kyc.eventos_autenticacion
        WHERE evento = 'STEP_UP_VERIFICADO' AND sesion_id = '5e000000-0000-0000-0000-000000000003'));
SELECT pg_temp.exigir('lo demás de la sesión no cambió', (
  SELECT bool_and(revocada_en IS NULL) FROM kyc.sesiones
   WHERE id IN ('5e000000-0000-0000-0000-000000000001', '5e000000-0000-0000-0000-000000000002',
                '5e000000-0000-0000-0000-000000000003', '5e000000-0000-0000-0000-000000000006')));

-- -----------------------------------------------------------------------------
-- 3. La bitácora: cada ventana con su evento, y ningún evento sin su ventana
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('cada ventana abierta tiene su evento', (
  SELECT count(*) = 2 AND bool_and(EXISTS (
           SELECT 1 FROM kyc.eventos_autenticacion e
            WHERE e.sesion_id = s.id AND e.evento = 'STEP_UP_VERIFICADO'))
    FROM kyc.sesiones s WHERE s.step_up_valido_hasta IS NOT NULL));
SELECT pg_temp.debe_pasar('el catálogo acepta STEP_UP_VERIFICADO y STEP_UP_FALLIDO', ARRAY[
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento) VALUES
       ('b0000000-0000-0000-0000-00000000000b', 'STEP_UP_VERIFICADO'),
       ('b0000000-0000-0000-0000-00000000000b', 'STEP_UP_FALLIDO')$q$
]);
SELECT pg_temp.debe_pasar('sc_auth registra un intento fallido con su sesión, IP y dispositivo', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, sesion_id, dispositivo)
     VALUES ('b0000000-0000-0000-0000-00000000000b', 'STEP_UP_FALLIDO', '198.51.100.23',
             '5e000000-0000-0000-0000-000000000003', 'Firefox on Linux')$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_fallar('sc_auth no registra una verificación sin su ventana', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, sesion_id, detalle)
     VALUES ('b0000000-0000-0000-0000-00000000000b', 'STEP_UP_VERIFICADO', '5e000000-0000-0000-0000-000000000003', '300')$q$
], 'step_up_con_ventana');
SELECT pg_temp.debe_fallar('ni con la ventana de la sesión de otro cliente', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, sesion_id, detalle)
     VALUES ('b0000000-0000-0000-0000-00000000000b', 'STEP_UP_VERIFICADO', '5e000000-0000-0000-0000-000000000001', '300')$q$
], 'step_up_con_ventana');
SELECT pg_temp.debe_fallar('ni sin sesión', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, detalle)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'STEP_UP_VERIFICADO', '300')$q$
], 'step_up_con_ventana');
SELECT pg_temp.debe_fallar('el detalle sigue siendo un número', ARRAY[
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, detalle)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'STEP_UP_FALLIDO', 'código 123456')$q$
], 'ck_eventos_autenticacion_detalle');
SELECT pg_temp.debe_fallar('y los eventos, como toda la bitácora, no se corrigen', ARRAY[
  $q$UPDATE kyc.eventos_autenticacion SET detalle = '86400' WHERE evento = 'STEP_UP_VERIFICADO'$q$
], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('ni se borran', ARRAY[
  $q$DELETE FROM kyc.eventos_autenticacion WHERE evento = 'STEP_UP_FALLIDO'$q$
], 'SC_LEDGER_INMUTABLE');

-- -----------------------------------------------------------------------------
-- 4. Beneficiarios: una designación nace en una sesión con su ventana abierta
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('una designación sin sesión no se guarda',
  pg_temp.designacion('d1000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a', NULL),
  'SC_STEP_UP_REQUERIDO');
SELECT pg_temp.debe_fallar('ni desde una sesión sin ventana',
  pg_temp.designacion('d1000000-0000-0000-0000-000000000002', 'b0000000-0000-0000-0000-00000000000b',
                      '5e000000-0000-0000-0000-000000000003'),
  'SC_STEP_UP_REQUERIDO');
SELECT pg_temp.debe_fallar('ni con la ventana de la sesión de otro cliente',
  pg_temp.designacion('d1000000-0000-0000-0000-000000000003', 'b0000000-0000-0000-0000-00000000000b',
                      '5e000000-0000-0000-0000-000000000001'),
  'SC_STEP_UP_REQUERIDO');
-- El dueño del esquema simula una ventana que ya venció en la sesión 6.
SELECT pg_temp.debe_pasar('(la sesión 6 tuvo una ventana que ya venció)', ARRAY[
  'ALTER TABLE kyc.sesiones DISABLE TRIGGER USER',
  $q$UPDATE kyc.sesiones SET step_up_valido_hasta = now() - interval '1 second'
      WHERE id = '5e000000-0000-0000-0000-000000000006'$q$,
  'ALTER TABLE kyc.sesiones ENABLE TRIGGER USER'
]);
SELECT pg_temp.debe_fallar('ni con una ventana vencida',
  pg_temp.designacion('d1000000-0000-0000-0000-000000000004', 'a0000000-0000-0000-0000-00000000000a',
                      '5e000000-0000-0000-0000-000000000006'),
  'SC_STEP_UP_REQUERIDO');
SELECT pg_temp.debe_pasar('sc_auth revoca la sesión 2, con su ventana aún abierta', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.sesiones SET revocada_en = now(), motivo_revocacion = 'BLOQUEO'
      WHERE id = '5e000000-0000-0000-0000-000000000002'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_fallar('ni desde una sesión revocada, aunque su ventana no haya vencido',
  pg_temp.designacion('d1000000-0000-0000-0000-000000000005', 'a0000000-0000-0000-0000-00000000000a',
                      '5e000000-0000-0000-0000-000000000002'),
  'SC_STEP_UP_REQUERIDO');
SELECT pg_temp.debe_fallar('ni el dueño del esquema sin una sesión verificada',
  pg_temp.designacion('d1000000-0000-0000-0000-000000000006', 'a0000000-0000-0000-0000-00000000000a', NULL, NULL),
  'SC_STEP_UP_REQUERIDO');
SELECT pg_temp.debe_fallar('primero quién escribe: Carl, sin ventana, no llega a la regla del KYC',
  pg_temp.designacion('d1000000-0000-0000-0000-000000000007', 'c0000000-0000-0000-0000-00000000000c',
                      '5e000000-0000-0000-0000-000000000007'),
  'SC_STEP_UP_REQUERIDO');
SELECT pg_temp.debe_pasar('Carl verifica de nuevo en la sesión 7',
  pg_temp.verificar('5e000000-0000-0000-0000-000000000007', '192.0.2.70', 'Firefox on Windows'));
SELECT pg_temp.debe_fallar('y entonces sí: sin KYC verificado no designa',
  pg_temp.designacion('d1000000-0000-0000-0000-000000000008', 'c0000000-0000-0000-0000-00000000000c',
                      '5e000000-0000-0000-0000-000000000007'),
  'SC_KYC_NO_VERIFICADO');
SELECT pg_temp.debe_pasar('con su ventana abierta, Ludwig designa desde la sesión 1',
  pg_temp.designacion('d1000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a',
                      '5e000000-0000-0000-0000-000000000001'));
SELECT pg_temp.exigir('la versión guarda su sesión, y su evento también', (
  SELECT sesion_id = '5e000000-0000-0000-0000-000000000001' AND version = 1
    FROM kyc.designaciones_beneficiarios WHERE id = 'd1000000-0000-0000-0000-000000000009')
  AND (SELECT count(*) = 1 FROM kyc.eventos_autenticacion
        WHERE evento = 'BENEFICIARIOS_DESIGNADOS' AND sesion_id = '5e000000-0000-0000-0000-000000000001'
          AND detalle = '1'));
SELECT pg_temp.exigir('las designaciones rechazadas no dejaron nada', (
  SELECT count(*) = 1 FROM kyc.designaciones_beneficiarios)
  AND (SELECT count(*) = 1 FROM kyc.beneficiarios)
  AND (SELECT count(*) = 1 FROM kyc.eventos_autenticacion WHERE evento = 'BENEFICIARIOS_DESIGNADOS'));

-- -----------------------------------------------------------------------------
-- 5. Privilegios: sc_auth abre la ventana, sc_kyc la lee, nadie más la toca
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('sc_kyc lee la ventana', ARRAY[
  'SET LOCAL ROLE sc_kyc', 'SELECT step_up_valido_hasta FROM kyc.sesiones', 'RESET ROLE'
]);
SELECT pg_temp.debe_fallar('pero no la abre', ARRAY[
  'SET LOCAL ROLE sc_kyc', $q$UPDATE kyc.sesiones SET step_up_valido_hasta = now()$q$
], 'permission denied');
SELECT pg_temp.debe_fallar('el cliente no la lee', ARRAY[
  $q$SELECT set_config('sc.cliente_actual', 'a0000000-0000-0000-0000-00000000000a', true)$q$,
  'SET LOCAL ROLE sc_cliente', 'SELECT step_up_valido_hasta FROM kyc.sesiones'
], 'permission denied');
SELECT pg_temp.exigir('sc_auth actualiza la ventana, el último uso y la revocación; nada más', (
  SELECT array_agg(column_name::TEXT ORDER BY column_name::TEXT)
         = ARRAY['motivo_revocacion', 'revocada_en', 'step_up_valido_hasta', 'ultimo_uso_en']
    FROM information_schema.column_privileges
   WHERE grantee = 'sc_auth' AND table_schema = 'kyc' AND table_name = 'sesiones' AND privilege_type = 'UPDATE'));
SELECT pg_temp.exigir('ningún otro rol de la aplicación toca la ventana', (
  SELECT bool_and(NOT has_column_privilege(r, 'kyc.sesiones', 'step_up_valido_hasta', 'SELECT, INSERT, UPDATE, REFERENCES'))
    FROM unnest(ARRAY['sc_ifpe', 'sc_sofom', 'sc_puente', 'sc_spei', 'sc_mercado', 'sc_aml', 'sc_auditor',
                      'sc_auditor_pld', 'sc_kyc_verificador', 'sc_cliente']) AS r)
  AND NOT has_column_privilege('sc_kyc', 'kyc.sesiones', 'step_up_valido_hasta', 'INSERT, UPDATE, REFERENCES'));

\o
\echo 'tests/db/0022_verificacion_reforzada.sql: todas las pruebas pasaron (ROLLBACK)'
ROLLBACK;
