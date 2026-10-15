-- =============================================================================
--  Pruebas de la migración 0007 (endurecimiento del KYC). ROLLBACK al final.
--
--    psql "$DATABASE_URL" -f tests/db/0007_kyc.sql
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

-- Expediente listo para enviar a revisión (datos completos, como dueño).
CREATE FUNCTION pg_temp.completar(cliente UUID, curp TEXT, clave TEXT) RETURNS void
LANGUAGE sql AS $$
    UPDATE kyc.kyc_perfiles SET curp = $2, rfc = substr($2, 1, 10) || 'AB1', nombres = 'Persona', apellido_paterno = 'Prueba',
           fecha_nacimiento = '1990-01-01', sexo = 'H', entidad_nacimiento = 'DF',
           domicilio = '{"calle": "a", "numero_exterior": "1", "colonia": "c", "municipio": "m", "estado": "e", "codigo_postal": "06000"}',
           ine_clave_elector = $3, ine_cic = '123456789', ine_emision = 2022, ine_vigencia = 2032,
           evidencia = '{"ine_frente": "a", "ine_reverso": "b", "selfie": ["c"]}'
     WHERE cliente_id = $1
$$;

-- -----------------------------------------------------------------------------
-- Datos base (como dueño del esquema)
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('0f000000-0000-0000-0000-0000000000f0', 'FISICA', 'Operador', 'Cumplimiento', 'op@example.mx', 'VERIFICADO', now(), 'BAJO'),
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Verificada', 'Prueba', 'v@example.mx', 'VERIFICADO', now(), 'BAJO');
INSERT INTO kyc.credenciales (cliente_id, password_hash, rol) VALUES ('0f000000-0000-0000-0000-0000000000f0', 'x', 'OPERADOR');
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email) VALUES
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Pendiente', 'Prueba', 'p@example.mx'),
  ('c0000000-0000-0000-0000-00000000000c', 'FISICA', 'Otra', 'Prueba', 'o@example.mx');
INSERT INTO ifpe.reserva_custodia (id, institucion, tipo, numero_cuenta) VALUES
  ('e0000000-0000-0000-0000-00000000000e', 'Banco de México', 'BANXICO', 'SC-RES-001');
INSERT INTO ifpe.cuentas (id, cliente_id, clabe) VALUES
  ('1a000000-0000-0000-0000-00000000001a', 'a0000000-0000-0000-0000-00000000000a', '646180157000000017'),
  ('1b000000-0000-0000-0000-00000000001b', 'b0000000-0000-0000-0000-00000000000b', '646180157000000020');
INSERT INTO sofom.cuentas (id, cliente_id) VALUES
  ('2a000000-0000-0000-0000-00000000002a', 'a0000000-0000-0000-0000-00000000000a'),
  ('2b000000-0000-0000-0000-00000000002b', 'b0000000-0000-0000-0000-00000000000b');

-- -----------------------------------------------------------------------------
-- 1. Proveedor simulado solo en demostración
-- -----------------------------------------------------------------------------
SELECT pg_temp.completar('b0000000-0000-0000-0000-00000000000b', 'PEPX900101HDFRRR03', 'PRPRPR90010109H100');
UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b';
CREATE TEMP TABLE v AS
  SELECT verificacion_en_curso AS id FROM kyc.kyc_perfiles WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b';
GRANT SELECT ON v TO sc_kyc_verificador;

SELECT pg_temp.exigir('Simulado: deshabilitado por omisión', NOT (SELECT permite_proveedor_simulado FROM kyc.parametros));
SELECT pg_temp.debe_fallar('Simulado: su resultado se rechaza fuera de demostración', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$INSERT INTO kyc.kyc_auditoria_biometrica (cliente_id, verificacion_id, proveedor, score_similitud,
       prueba_vida_exitosa, documento_valido, curp_ocr, clave_elector_ocr, coincide_identidad, resultado, firma, resultado_crudo)
     SELECT 'b0000000-0000-0000-0000-00000000000b', id, 'simulado', 0.99, true, true, 'PEPX900101HDFRRR03',
            'PRPRPR90010109H100', true, 'APROBADO', 'f', '{}' FROM v$q$
], 'SC_KYC_PROVEEDOR_NO_PERMITIDO');

-- -----------------------------------------------------------------------------
-- 5. Falla del proveedor: el intento se devuelve (solo sin resultado)
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Reembolso: falla del proveedor sin resultado devuelve el intento', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$SELECT set_config('sc.reembolsar_intento', 'si', true)$q$,
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'PENDIENTE' WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b'$q$,
  $q$SELECT set_config('sc.reembolsar_intento', '', true)$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('Reembolso: intentos vuelve a 0', (SELECT intentos = 0 AND estado_kyc = 'PENDIENTE'
  FROM kyc.kyc_perfiles WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b'));

UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b';
UPDATE v SET id = (SELECT verificacion_en_curso FROM kyc.kyc_perfiles WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b');
INSERT INTO kyc.kyc_auditoria_biometrica (cliente_id, verificacion_id, proveedor, score_similitud, prueba_vida_exitosa,
       documento_valido, curp_ocr, clave_elector_ocr, coincide_identidad, resultado, firma, resultado_crudo)
SELECT 'b0000000-0000-0000-0000-00000000000b', id, 'certificado', 0.75, true, true, 'PEPX900101HDFRRR03',
       'PRPRPR90010109H100', true, 'REVISION', 'f', '{}' FROM v;
SELECT pg_temp.debe_fallar('Reembolso: un envío con resultado no devuelve su intento', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$SELECT set_config('sc.reembolsar_intento', 'si', true)$q$,
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'PENDIENTE' WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b'$q$
], 'ya tiene resultado');
SELECT pg_temp.exigir('Auditoría: guarda la CURP declarada', (SELECT curp_declarada = 'PEPX900101HDFRRR03'
  FROM kyc.kyc_auditoria_biometrica WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b'));

-- Reapertura de un operador con los intentos agotados: queda uno.
UPDATE kyc.kyc_perfiles SET estado_kyc = 'RECHAZADO', motivo_rechazo = 'prueba' WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b';
UPDATE kyc.kyc_perfiles SET estado_kyc = 'PENDIENTE' WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b';
UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b';
UPDATE kyc.kyc_perfiles SET estado_kyc = 'PENDIENTE' WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b';
UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b';
UPDATE kyc.kyc_perfiles SET estado_kyc = 'PENDIENTE' WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b';
UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b';
SELECT pg_temp.exigir('Reapertura: tres envíos agotan los intentos', (SELECT intentos = 3
  FROM kyc.kyc_perfiles WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b'));
SELECT pg_temp.debe_pasar('Reapertura: un operador pide un nuevo envío', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$SELECT set_config('sc.operador_id', '0f000000-0000-0000-0000-0000000000f0', true)$q$,
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'PENDIENTE' WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b'$q$,
  $q$SELECT set_config('sc.operador_id', '', true)$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('Reapertura: le queda un intento', (SELECT intentos = 2
  FROM kyc.kyc_perfiles WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b'));

-- -----------------------------------------------------------------------------
-- 5. Límites por identidad y por conflictos
-- -----------------------------------------------------------------------------
-- Tres rechazos de una CURP en la cuenta B bloquean esa CURP en la cuenta C.
DO $$
DECLARE
    i INT;
BEGIN
    UPDATE kyc.kyc_perfiles SET estado_kyc = 'RECHAZADO', motivo_rechazo = 'r' WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b';
    UPDATE kyc.kyc_perfiles SET estado_kyc = 'PENDIENTE' WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b';
    FOR i IN 1..3 LOOP
        UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b';
        INSERT INTO kyc.kyc_auditoria_biometrica (cliente_id, verificacion_id, proveedor, score_similitud, prueba_vida_exitosa,
               documento_valido, curp_ocr, clave_elector_ocr, coincide_identidad, resultado, firma, resultado_crudo)
        SELECT cliente_id, verificacion_en_curso, 'certificado', 0.30, false, true, curp, ine_clave_elector, false,
               'RECHAZADO', 'f', '{}'
          FROM kyc.kyc_perfiles WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b';
        UPDATE kyc.kyc_perfiles SET estado_kyc = CASE WHEN i < 3 THEN 'PENDIENTE' ELSE 'RECHAZADO' END::kyc.estado_kyc,
               motivo_rechazo = CASE WHEN i < 3 THEN NULL ELSE 'agotado' END
         WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b';
    END LOOP;
END
$$;
SELECT pg_temp.completar('c0000000-0000-0000-0000-00000000000c', 'PEPX900101HDFRRR03', 'PRPRPR90010109H101');
SELECT pg_temp.debe_fallar('Identidad: rechazos en otra cuenta bloquean la CURP', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = 'c0000000-0000-0000-0000-00000000000c'$q$
], 'SC_KYC_IDENTIDAD_BLOQUEADA');

SELECT pg_temp.completar('c0000000-0000-0000-0000-00000000000c', 'OTRX900101HDFRRR09', 'PRPRPR90010109H101');
SELECT kyc.registrar_conflicto_identidad('c0000000-0000-0000-0000-00000000000c') FROM generate_series(1, 3);
SELECT pg_temp.debe_fallar('Conflictos: tres choques con identidades ajenas agotan los envíos', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = 'c0000000-0000-0000-0000-00000000000c'$q$
], 'SC_KYC_SIN_INTENTOS');

-- -----------------------------------------------------------------------------
-- 2. Vencimientos de mutuo ligados a su contrato
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('Retorno: sc_ifpe no escribe RETORNO_DE_SOFOM', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 50 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 50 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, ref_movimiento_espejo)
     VALUES ('RETORNO_DE_SOFOM', '1a000000-0000-0000-0000-00000000001a', 'e0000000-0000-0000-0000-00000000000e', 50,
             'retorno-falso-1', gen_random_uuid())$q$
], 'row-level security');

SELECT pg_temp.debe_fallar('Retorno: el puente no abona un vencimiento sin contrato (cliente sin KYC)', ARRAY[
  'SET LOCAL ROLE sc_puente',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 5000 WHERE id = '1b000000-0000-0000-0000-00000000001b'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 5000 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, ref_movimiento_espejo)
     VALUES ('RETORNO_DE_SOFOM', '1b000000-0000-0000-0000-00000000001b', 'e0000000-0000-0000-0000-00000000000e', 5000,
             'retorno-falso-2', gen_random_uuid())$q$
], 'SC_RETORNO_SIN_CONTRATO');

-- -----------------------------------------------------------------------------
-- 3. Titular de contratos y cuentas de la SOFOM
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('Titular: una cuenta SOFOM no cambia de dueño', ARRAY[
  'SET LOCAL ROLE sc_puente',
  $q$UPDATE sofom.cuentas SET cliente_id = 'b0000000-0000-0000-0000-00000000000b' WHERE id = '2a000000-0000-0000-0000-00000000002a'$q$
], 'SC_CUENTA_INMUTABLE');

SELECT pg_temp.debe_fallar('Titular: un contrato con la cuenta IFPE de otro cliente', ARRAY[
  'SET LOCAL ROLE sc_puente',
  $q$INSERT INTO sofom.contratos_mutuo (cliente_id, cuenta_sofom_id, cuenta_ifpe_origen_id, tx_aportacion_ifpe_id,
       principal, tasa_anual, plazo_dias, fecha_inicio, hash_terminos)
     VALUES ('a0000000-0000-0000-0000-00000000000a', '2a000000-0000-0000-0000-00000000002a',
             '1b000000-0000-0000-0000-00000000001b', gen_random_uuid(), 100, 0.05, 90, current_date, repeat('0', 64))$q$
], 'SC_MUTUO_TITULAR');

-- -----------------------------------------------------------------------------
-- 4. Créditos
-- -----------------------------------------------------------------------------
INSERT INTO sofom.creditos (id, acreditado_id, principal, tasa_anual, plazo_dias, fecha_desembolso, destino)
VALUES ('3c000000-0000-0000-0000-00000000003c', 'a0000000-0000-0000-0000-00000000000a', 100, 0.2, 30, current_date, 'capital de trabajo');
SELECT pg_temp.debe_fallar('Crédito: el acreditado no se reasigna', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$UPDATE sofom.creditos SET acreditado_id = 'b0000000-0000-0000-0000-00000000000b' WHERE id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'SC_CREDITO_INMUTABLE');
UPDATE kyc.kyc_perfiles SET estado_kyc = 'RECHAZADO', motivo_rechazo = 'revocado' WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a';
SELECT pg_temp.debe_fallar('Crédito: no se desembolsa a un acreditado sin KYC vigente', ARRAY[
  $q$UPDATE sofom.creditos SET estado = 'VIGENTE' WHERE id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'SC_KYC_NO_VERIFICADO');

ROLLBACK;
\o
\echo 'tests/db/0007_kyc.sql: todas las pruebas pasaron (ROLLBACK)'
