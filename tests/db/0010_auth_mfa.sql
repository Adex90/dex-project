-- =============================================================================
--  Pruebas de la migración 0010 (segundo factor y restablecimiento). Corren en
--  una transacción que termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0010_auth_mfa.sql
--
--  Lo que la base de datos garantiza por sí sola, sin importar el backend:
--  ningún secreto TOTP en claro ni token de restablecimiento sin hash; no hay
--  MFA activo sin secreto; con MFA activo solo el dueño del esquema lo
--  desactiva o cambia (y queda registrado); solo sc_auth lee los secretos; la
--  bitácora de autenticación es inmutable.
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

-- -----------------------------------------------------------------------------
-- Datos: una oficial de cumplimiento y un cliente
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('0c000000-0000-0000-0000-0000000000c0', 'FISICA', 'Oficial', 'Cumplimiento', 'oficial@example.mx', 'VERIFICADO', now(), 'BAJO'),
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ludwig', 'von Mises', 'ludwig@example.mx', 'VERIFICADO', now(), 'BAJO');
INSERT INTO kyc.credenciales (cliente_id, password_hash, rol) VALUES
  ('0c000000-0000-0000-0000-0000000000c0', 'x', 'CUMPLIMIENTO'),
  ('a0000000-0000-0000-0000-00000000000a', 'x', 'CLIENTE');

-- Un cifrado v1 válido en forma (la base de datos no tiene la clave: solo el formato).
\set cifrado_a '''v1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'''
\set cifrado_b '''v1:BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB'''
\set hash_a '''aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'''

-- -----------------------------------------------------------------------------
-- 1. Nada en claro
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('un secreto TOTP base32 en claro no cabe en totp_secret', ARRAY[
  $q$UPDATE kyc.credenciales SET totp_secret = 'JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP'
      WHERE cliente_id = '0c000000-0000-0000-0000-0000000000c0'$q$
], 'ck_credenciales_totp_cifrado');
SELECT pg_temp.debe_fallar('ni con el prefijo v1 si no mide lo de un cifrado (nonce + secreto + etiqueta)', ARRAY[
  $q$UPDATE kyc.credenciales SET totp_secret = 'v1:JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP'
      WHERE cliente_id = '0c000000-0000-0000-0000-0000000000c0'$q$
], 'ck_credenciales_totp_cifrado');
SELECT pg_temp.debe_fallar('un token de restablecimiento sin hash no cabe en password_reset_token', ARRAY[
  $q$UPDATE kyc.credenciales SET password_reset_token = 'q3Jd9Kx0bW1aTkVr0p8yQm5NcXhLWnRkUkZ1T0Z5', token_expires_at = now() + interval '30 minutes'
      WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a'$q$
], 'ck_credenciales_reset_hash');
SELECT pg_temp.debe_fallar('token de restablecimiento sin vencimiento', ARRAY[
  format($q$UPDATE kyc.credenciales SET password_reset_token = %L WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a'$q$, :hash_a)
], 'ck_credenciales_reset_par');
SELECT pg_temp.debe_fallar('vencimiento sin token', ARRAY[
  $q$UPDATE kyc.credenciales SET token_expires_at = now() WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a'$q$
], 'ck_credenciales_reset_par');
SELECT pg_temp.debe_pasar('hash SHA-256 con su vencimiento', ARRAY[
  format($q$UPDATE kyc.credenciales SET password_reset_token = %L, token_expires_at = now() + interval '30 minutes'
            WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a'$q$, :hash_a)
]);
SELECT pg_temp.debe_fallar('dos personas no comparten un token vigente', ARRAY[
  format($q$UPDATE kyc.credenciales SET password_reset_token = %L, token_expires_at = now() + interval '30 minutes'
            WHERE cliente_id = '0c000000-0000-0000-0000-0000000000c0'$q$, :hash_a)
], 'ux_credenciales_reset');
SELECT pg_temp.debe_fallar('el paso TOTP es positivo', ARRAY[
  $q$UPDATE kyc.credenciales SET totp_ultimo_paso = 0 WHERE cliente_id = '0c000000-0000-0000-0000-0000000000c0'$q$
], 'ck_credenciales_totp_paso');

-- -----------------------------------------------------------------------------
-- 2. Reglas del segundo factor
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('no se activa MFA sin secreto (ni la aplicación ni el dueño)', ARRAY[
  $q$UPDATE kyc.credenciales SET mfa_enabled = TRUE WHERE cliente_id = '0c000000-0000-0000-0000-0000000000c0'$q$
], 'SC_MFA_SIN_SECRETO');
SELECT pg_temp.debe_pasar('sc_auth guarda el secreto cifrado y activa MFA', ARRAY[
  'SET LOCAL ROLE sc_auth',
  format($q$UPDATE kyc.credenciales SET totp_secret = %L WHERE cliente_id = '0c000000-0000-0000-0000-0000000000c0'$q$, :cifrado_a),
  $q$UPDATE kyc.credenciales SET mfa_enabled = TRUE, totp_ultimo_paso = 59000000
      WHERE cliente_id = '0c000000-0000-0000-0000-0000000000c0'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_fallar('con MFA activo, la aplicación no lo desactiva', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.credenciales SET mfa_enabled = FALSE WHERE cliente_id = '0c000000-0000-0000-0000-0000000000c0'$q$
], 'SC_MFA_SOLO_DUENO');
SELECT pg_temp.debe_fallar('con MFA activo, la aplicación no cambia el secreto (sería una reinscripción sin el dispositivo)', ARRAY[
  'SET LOCAL ROLE sc_auth',
  format($q$UPDATE kyc.credenciales SET totp_secret = %L WHERE cliente_id = '0c000000-0000-0000-0000-0000000000c0'$q$, :cifrado_b)
], 'SC_MFA_SOLO_DUENO');
SELECT pg_temp.debe_fallar('con MFA activo, la aplicación no borra el secreto', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.credenciales SET totp_secret = NULL WHERE cliente_id = '0c000000-0000-0000-0000-0000000000c0'$q$
], 'SC_MFA_SIN_SECRETO');
SELECT pg_temp.debe_pasar('con MFA activo, la aplicación sí avanza el último paso aceptado', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.credenciales SET totp_ultimo_paso = 59000001 WHERE cliente_id = '0c000000-0000-0000-0000-0000000000c0'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_pasar('el dueño del esquema (el DBA) desactiva MFA por pérdida del dispositivo', ARRAY[
  $q$UPDATE kyc.credenciales SET mfa_enabled = FALSE, totp_secret = NULL, totp_ultimo_paso = NULL
      WHERE cliente_id = '0c000000-0000-0000-0000-0000000000c0'$q$
]);
SELECT pg_temp.exigir('desactivar MFA queda en la bitácora, a nombre del dueño',
  (SELECT count(*) = 1 AND bool_and(registrado_por = current_user::text)
     FROM kyc.eventos_autenticacion
    WHERE cliente_id = '0c000000-0000-0000-0000-0000000000c0' AND evento = 'MFA_DESACTIVADO'));

-- -----------------------------------------------------------------------------
-- 3. Solo sc_auth lee y escribe los secretos
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('sc_kyc no lee totp_secret', ARRAY[
  'SET LOCAL ROLE sc_kyc', 'SELECT totp_secret FROM kyc.credenciales'
], 'permission denied');
SELECT pg_temp.debe_fallar('sc_kyc no lee password_reset_token', ARRAY[
  'SET LOCAL ROLE sc_kyc', 'SELECT password_reset_token FROM kyc.credenciales'
], 'permission denied');
SELECT pg_temp.debe_fallar('sc_kyc no puede leer la fila completa', ARRAY[
  'SET LOCAL ROLE sc_kyc', 'SELECT * FROM kyc.credenciales'
], 'permission denied');
SELECT pg_temp.debe_fallar('sc_kyc no escribe totp_secret', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  format($q$UPDATE kyc.credenciales SET totp_secret = %L WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a'$q$, :cifrado_a)
], 'permission denied');
SELECT pg_temp.debe_pasar('sc_kyc conserva lo que usan el inicio de sesión y la validación de la sesión', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  'SELECT cliente_id, rol, password_hash, intentos_fallidos, bloqueado_hasta, tokens_validos_desde, mfa_enabled FROM kyc.credenciales',
  'RESET ROLE'
]);
SELECT pg_temp.debe_fallar('sc_kyc_verificador solo ve cliente y rol', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador', 'SELECT mfa_enabled FROM kyc.credenciales'
], 'permission denied');
SELECT pg_temp.debe_fallar('sc_aml no lee secretos', ARRAY[
  'SET LOCAL ROLE sc_aml', 'SELECT totp_secret FROM kyc.credenciales'
], 'permission denied');
SELECT pg_temp.debe_fallar('sc_auth no asigna roles (solo el dueño crea personal)', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$UPDATE kyc.credenciales SET rol = 'CUMPLIMIENTO' WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a'$q$
], 'permission denied');
SELECT pg_temp.debe_pasar('sc_auth lee los secretos y registra eventos', ARRAY[
  'SET LOCAL ROLE sc_auth',
  'SELECT totp_secret, password_reset_token, token_expires_at, totp_ultimo_paso FROM kyc.credenciales',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip)
      VALUES ('a0000000-0000-0000-0000-00000000000a', 'RESTABLECIMIENTO_SOLICITADO', '203.0.113.7')$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('un evento de la aplicación queda a nombre de sc_auth',
  (SELECT registrado_por = 'sc_auth' FROM kyc.eventos_autenticacion
    WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND evento = 'RESTABLECIMIENTO_SOLICITADO'));
SELECT pg_temp.debe_fallar('sc_auth no elige a nombre de quién queda un evento', ARRAY[
  'SET LOCAL ROLE sc_auth',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, registrado_por)
      VALUES ('a0000000-0000-0000-0000-00000000000a', 'MFA_VERIFICADO', 'synthetic_owner')$q$
], 'permission denied');
SELECT pg_temp.debe_fallar('sc_auth no lee la bitácora', ARRAY[
  'SET LOCAL ROLE sc_auth', 'SELECT * FROM kyc.eventos_autenticacion'
], 'permission denied');

-- -----------------------------------------------------------------------------
-- 4. Bitácora inmutable
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('un evento no se corrige', ARRAY[
  $q$UPDATE kyc.eventos_autenticacion SET evento = 'MFA_VERIFICADO'$q$
], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('un evento no se borra', ARRAY[
  'DELETE FROM kyc.eventos_autenticacion'
], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('la bitácora no se vacía', ARRAY[
  'TRUNCATE kyc.eventos_autenticacion'
], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('solo eventos del catálogo', ARRAY[
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento) VALUES ('a0000000-0000-0000-0000-00000000000a', 'LOGIN')$q$
], 'ck_eventos_autenticacion_evento');

ROLLBACK;
\o
\echo 'tests/db/0010_auth_mfa.sql: todas las pruebas pasaron (ROLLBACK)'
