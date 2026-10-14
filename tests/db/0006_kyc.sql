-- =============================================================================
--  Pruebas de la migración 0006 (KYC biométrico y bloqueo por RLS). Corren en
--  una transacción que termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0006_kyc.sql
--
--  Un cliente sin KYC VERIFICADO no recibe dinero ni invierte, y nadie más que
--  sc_kyc_verificador, con evidencia biométrica suficiente, lo verifica.
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

-- Sin bloque EXCEPTION: las sentencias corren en la transacción principal
-- (la verificación automática exige el mismo xmin que la transacción).
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

-- Abono SPEI de 100 a la cuenta del cliente como lo registraría sc_spei.
CREATE FUNCTION pg_temp.abono(rastreo TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
      'SET LOCAL ROLE sc_spei',
      $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 100 WHERE id = '3a000000-0000-0000-0000-00000000003a'$q$,
      $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 100 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
      format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
         VALUES (%L, 'DEPOSITO', '3a000000-0000-0000-0000-00000000003a', 'e0000000-0000-0000-0000-00000000000e', 100,
                 %L, %L)$q$, md5(rastreo)::uuid, 'spei:40012:20261001:' || rastreo, '40012:20261001:' || rastreo),
      format($q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                        estado, transaccion_id, origen, raw_payload)
         VALUES ('40012', %L, '2026-10-01', '646180157000000033', '3a000000-0000-0000-0000-00000000003a', 100,
                 'ABONADO', %L, 'WEBHOOK', '{}')$q$, rastreo, md5(rastreo)::uuid),
      'RESET ROLE'
    ]
$$;

-- -----------------------------------------------------------------------------
-- Datos base
-- -----------------------------------------------------------------------------
-- Operador (alta administrativa del dueño del esquema).
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld)
VALUES ('0f000000-0000-0000-0000-0000000000f0', 'FISICA', 'Operador', 'Cumplimiento', 'op@example.mx', 'VERIFICADO', now(), 'BAJO');
INSERT INTO kyc.credenciales (cliente_id, password_hash, rol) VALUES ('0f000000-0000-0000-0000-0000000000f0', 'x', 'OPERADOR');
INSERT INTO ifpe.reserva_custodia (id, institucion, tipo, numero_cuenta) VALUES
  ('e0000000-0000-0000-0000-00000000000e', 'Banco de México', 'BANXICO', 'SC-RES-001');

-- Alta de un cliente como lo hace el backend (sc_kyc).
SELECT pg_temp.debe_pasar('Alta: sc_kyc registra un cliente', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, curp)
     VALUES ('3c000000-0000-0000-0000-00000000003c', 'FISICA', 'Friedrich', 'Hayek', 'fh@example.mx', 'HAXF990508HDFYXR03')$q$,
  'RESET ROLE'
]);
INSERT INTO ifpe.cuentas (id, cliente_id, clabe) VALUES ('3a000000-0000-0000-0000-00000000003a',
  '3c000000-0000-0000-0000-00000000003c', '646180157000000033');
SELECT pg_temp.exigir('Alta: el expediente nace PENDIENTE con los datos declarados',
  (SELECT estado_kyc = 'PENDIENTE' AND curp = 'HAXF990508HDFYXR03' AND nombres = 'Friedrich'
     FROM kyc.kyc_perfiles WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'));

SELECT pg_temp.debe_fallar('Alta: sc_kyc no puede dar de alta un cliente VERIFICADO', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$INSERT INTO kyc.clientes (tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en)
     VALUES ('FISICA', 'Falso', 'Verificado', 'falso@example.mx', 'VERIFICADO', now())$q$
], 'permission denied');

SELECT pg_temp.debe_fallar('Alta: sc_kyc ya no cambia estatus_kyc de kyc.clientes', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.clientes SET estatus_kyc = 'VERIFICADO', kyc_verificado_en = now() WHERE id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'permission denied');

SELECT pg_temp.debe_fallar('Espejo: ni el dueño del esquema verifica por fuera del expediente', ARRAY[
  $q$UPDATE kyc.clientes SET estatus_kyc = 'VERIFICADO', kyc_verificado_en = now() WHERE id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'SC_KYC_ESPEJO');

-- -----------------------------------------------------------------------------
-- Bloqueo: sin KYC verificado no entra dinero
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('RLS: SPEI recibido para un cliente PENDIENTE', pg_temp.abono('KYC0001'),
  'row-level security');

-- -----------------------------------------------------------------------------
-- Captura y envío a revisión (sc_kyc)
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('Envío: expediente incompleto', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'ck_perfil_envio_completo');

SELECT pg_temp.debe_pasar('Captura: datos personales, domicilio, INE y evidencia', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET apellido_materno = NULL, rfc = 'HAXF990508AB1', fecha_nacimiento = '1999-05-08',
       sexo = 'H', entidad_nacimiento = 'DF',
       domicilio = '{"calle": "Reforma", "numero_exterior": "1", "colonia": "Centro", "municipio": "Cuauhtémoc",
                     "estado": "CDMX", "codigo_postal": "06000"}',
       ine_clave_elector = 'HYXXFR99050809H100', ine_cic = '123456789', ine_emision = 2020, ine_vigencia = 2030,
       envio_ip = '203.0.113.7', envio_user_agent = 'prueba',
       evidencia = '{"ine_frente": "aa", "ine_reverso": "bb", "selfie": ["cc", "dd", "ee"]}'
     WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$,
  'RESET ROLE'
]);

SELECT pg_temp.debe_fallar('Dictamen: sc_kyc no puede verificar', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'VERIFICADO' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'SC_KYC_TRANSICION');

-- Segunda línea de defensa: aun sin el trigger (desactivado solo aquí, como
-- dueño), la política RLS impide que sc_kyc deje un expediente VERIFICADO.
SELECT pg_temp.debe_fallar('RLS: sc_kyc no verifica ni con el trigger desactivado', ARRAY[
  'ALTER TABLE kyc.kyc_perfiles DISABLE TRIGGER trg_perfiles_proteger',
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'VERIFICADO' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'row-level security');

SELECT pg_temp.debe_fallar('Envío: sc_kyc no fija intentos ni el id de verificación', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION', intentos = 0,
       verificacion_en_curso = '00000000-0000-0000-0000-000000000001'
     WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'permission denied');

-- Ni el dueño del esquema elige el id de verificación ni el contador.
SELECT pg_temp.debe_pasar('Envío: PENDIENTE -> EN_REVISION', ARRAY[
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION', intentos = 99,
       verificacion_en_curso = '00000000-0000-0000-0000-000000000001'
     WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$
]);
SELECT pg_temp.exigir('Envío: la base de datos cuenta el intento y genera el id de verificación',
  (SELECT intentos = 1 AND verificacion_en_curso IS NOT NULL
          AND verificacion_en_curso <> '00000000-0000-0000-0000-000000000001' AND enviado_en IS NOT NULL
     FROM kyc.kyc_perfiles WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'));

SELECT pg_temp.debe_pasar('RLS: sc_kyc ya no ve como editable un expediente EN_REVISION (0 filas)', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET curp = 'XXXX990508HDFYXR03' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('RLS: la identidad enviada no cambió',
  (SELECT curp = 'HAXF990508HDFYXR03' FROM kyc.kyc_perfiles WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'));

SELECT pg_temp.debe_fallar('Identidad: congelada desde el envío (incluso para el dueño)', ARRAY[
  $q$UPDATE kyc.kyc_perfiles SET curp = 'XXXX990508HDFYXR03' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'SC_KYC_IDENTIDAD_CONGELADA');

-- -----------------------------------------------------------------------------
-- Dictamen: solo sc_kyc_verificador, solo con evidencia
-- -----------------------------------------------------------------------------
CREATE TEMP TABLE v AS
  SELECT verificacion_en_curso AS id FROM kyc.kyc_perfiles WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c';
GRANT SELECT ON v TO sc_kyc_verificador, sc_kyc;

SELECT pg_temp.debe_fallar('Dictamen: sc_kyc no puede escribir resultados biométricos', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$INSERT INTO kyc.kyc_auditoria_biometrica (cliente_id, verificacion_id, proveedor, score_similitud,
       prueba_vida_exitosa, documento_valido, curp_ocr, clave_elector_ocr, coincide_identidad, resultado, firma, resultado_crudo)
     SELECT '3c000000-0000-0000-0000-00000000003c', id, 'x', 0.99, true, true, 'HAXF990508HDFYXR03',
            'HYXXFR99050809H100', true, 'APROBADO', 'f', '{}' FROM v$q$
], 'permission denied');

SELECT pg_temp.debe_fallar('Dictamen: VERIFICADO sin resultado biométrico', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'VERIFICADO' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'SC_KYC_SIN_EVIDENCIA');

SELECT pg_temp.debe_fallar('Auditoría: resultado para otra verificación', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$INSERT INTO kyc.kyc_auditoria_biometrica (cliente_id, verificacion_id, proveedor, score_similitud,
       prueba_vida_exitosa, documento_valido, curp_ocr, clave_elector_ocr, coincide_identidad, resultado, firma, resultado_crudo)
     VALUES ('3c000000-0000-0000-0000-00000000003c', gen_random_uuid(), 'x', 0.99, true, true, 'HAXF990508HDFYXR03',
             'HYXXFR99050809H100', true, 'APROBADO', 'f', '{}')$q$
], 'SC_KYC_VERIFICACION_AJENA');

SELECT pg_temp.debe_fallar('Auditoría: APROBADO con score bajo el umbral', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$INSERT INTO kyc.kyc_auditoria_biometrica (cliente_id, verificacion_id, proveedor, score_similitud,
       prueba_vida_exitosa, documento_valido, curp_ocr, clave_elector_ocr, coincide_identidad, resultado, firma, resultado_crudo)
     SELECT '3c000000-0000-0000-0000-00000000003c', id, 'x', 0.80, true, true, 'HAXF990508HDFYXR03',
            'HYXXFR99050809H100', true, 'APROBADO', 'f', '{}' FROM v$q$
], 'SC_KYC_RESULTADO_INCONSISTENTE');

SELECT pg_temp.debe_fallar('Auditoría: APROBADO con un OCR que no es la CURP del expediente', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$INSERT INTO kyc.kyc_auditoria_biometrica (cliente_id, verificacion_id, proveedor, score_similitud,
       prueba_vida_exitosa, documento_valido, curp_ocr, clave_elector_ocr, coincide_identidad, resultado, firma, resultado_crudo)
     SELECT '3c000000-0000-0000-0000-00000000003c', id, 'x', 0.99, true, true, 'OTRA990508HDFYXR03',
            'HYXXFR99050809H100', true, 'APROBADO', 'f', '{}' FROM v$q$
], 'SC_KYC_RESULTADO_INCONSISTENTE');

-- Resultado de zona gris (0.75): queda para revisión manual.
SELECT pg_temp.debe_pasar('Auditoría: resultado REVISION con score 0.75', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$INSERT INTO kyc.kyc_auditoria_biometrica (cliente_id, verificacion_id, proveedor, score_similitud,
       prueba_vida_exitosa, documento_valido, curp_ocr, clave_elector_ocr, coincide_identidad, resultado, firma, resultado_crudo)
     SELECT '3c000000-0000-0000-0000-00000000003c', id, 'simulado', 0.75, true, true, 'HAXF990508HDFYXR03',
            'HYXXFR99050809H100', true, 'REVISION', 'f', '{}' FROM v$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('Auditoría: el umbral y la IP del envío los fija la base de datos',
  (SELECT umbral_aplicado = 0.85 AND ip_origen = '203.0.113.7' AND evidencia ? 'selfie'
     FROM kyc.kyc_auditoria_biometrica WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'));

SELECT pg_temp.debe_fallar('Auditoría: inmutable', ARRAY[
  $q$UPDATE kyc.kyc_auditoria_biometrica SET score_similitud = 0.99 WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'SC_LEDGER_INMUTABLE');

SELECT pg_temp.debe_fallar('Dictamen: verificación automática con un resultado REVISION', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'VERIFICADO' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'SC_KYC_EVIDENCIA_INSUFICIENTE');

SELECT pg_temp.debe_fallar('Dictamen: un no-operador no dictamina', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$SELECT set_config('sc.operador_id', '3c000000-0000-0000-0000-00000000003c', true)$q$,
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'VERIFICADO' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'SC_KYC_OPERADOR_INVALIDO');

-- Con un umbral de revisión mayor que el score, ni un operador puede verificar.
UPDATE kyc.parametros SET umbral_revision = 0.80;
SELECT pg_temp.debe_fallar('Dictamen: un operador no verifica por debajo del umbral de revisión', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$SELECT set_config('sc.operador_id', '0f000000-0000-0000-0000-0000000000f0', true)$q$,
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'VERIFICADO' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'SC_KYC_EVIDENCIA_INSUFICIENTE');
UPDATE kyc.parametros SET umbral_revision = 0.70;

SELECT pg_temp.debe_pasar('Dictamen: un operador verifica un caso de zona gris', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$SELECT set_config('sc.operador_id', '0f000000-0000-0000-0000-0000000000f0', true)$q$,
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'VERIFICADO' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$,
  $q$SELECT set_config('sc.operador_id', '', true)$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('Dictamen: kyc.clientes refleja la identidad verificada',
  (SELECT c.estatus_kyc = 'VERIFICADO' AND c.kyc_verificado_en IS NOT NULL AND c.rfc = 'HAXF990508AB1'
          AND c.documento_identidad_tipo = 'INE' AND c.documento_identidad_numero = 'HYXXFR99050809H100'
          AND p.revisado_por = '0f000000-0000-0000-0000-0000000000f0'
     FROM kyc.clientes c JOIN kyc.kyc_perfiles p ON p.cliente_id = c.id
    WHERE c.id = '3c000000-0000-0000-0000-00000000003c'));
SELECT pg_temp.exigir('Historial: envío y dictamen registrados con su rol',
  (SELECT array_agg(estado_nuevo::text || '/' || rol ORDER BY id) = ARRAY['EN_REVISION/' || current_user, 'VERIFICADO/sc_kyc_verificador']
     FROM kyc.kyc_historial WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'));

SELECT pg_temp.debe_pasar('RLS: verificado, el SPEI se acredita', pg_temp.abono('KYC0002'));

-- -----------------------------------------------------------------------------
-- Revocación
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('Revocación: sin motivo', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'RECHAZADO' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'ck_perfil_rechazo');

SELECT pg_temp.debe_pasar('Revocación: VERIFICADO -> RECHAZADO', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'RECHAZADO', motivo_rechazo = 'documento apócrifo'
     WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_fallar('RLS: revocado, el siguiente SPEI ya no entra', pg_temp.abono('KYC0003'), 'row-level security');

SELECT pg_temp.debe_fallar('Revocación: RECHAZADO -> VERIFICADO directo', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'VERIFICADO' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'SC_KYC_TRANSICION');

SELECT pg_temp.debe_fallar('Revocación: el cliente no se reabre a sí mismo', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'PENDIENTE' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$,
  $q$SELECT 1/0 WHERE (SELECT estado_kyc FROM kyc.kyc_perfiles WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c') = 'RECHAZADO'$q$
], 'division by zero');

-- -----------------------------------------------------------------------------
-- Verificación automática y reintentos
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Reapertura: RECHAZADO -> PENDIENTE devuelve los intentos', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'PENDIENTE' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$,
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$,
  'RESET ROLE'
]);
UPDATE v SET id = (SELECT verificacion_en_curso FROM kyc.kyc_perfiles WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c');

SELECT pg_temp.debe_pasar('Automática: resultado APROBADO y VERIFICADO en la misma transacción', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$INSERT INTO kyc.kyc_auditoria_biometrica (cliente_id, verificacion_id, proveedor, score_similitud,
       prueba_vida_exitosa, documento_valido, curp_ocr, clave_elector_ocr, coincide_identidad, resultado, firma, resultado_crudo)
     SELECT '3c000000-0000-0000-0000-00000000003c', id, 'simulado', 0.93, true, true, 'HAXF990508HDFYXR03',
            'HYXXFR99050809H100', true, 'APROBADO', 'f', '{}' FROM v$q$,
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'VERIFICADO' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('Automática: sin operador', (SELECT revisado_por IS NULL AND estado_kyc = 'VERIFICADO'
  FROM kyc.kyc_perfiles WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'));

-- Agota los intentos: tres envíos fallidos y el cuarto ya no se admite.
SELECT pg_temp.debe_pasar('Intentos: tres envíos', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'PENDIENTE' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$,
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$,
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'PENDIENTE' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('Intentos: van en 2 (la reapertura los reinició)', (SELECT intentos = 2
  FROM kyc.kyc_perfiles WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'));
SELECT pg_temp.debe_pasar('Intentos: tercer envío', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$,
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'PENDIENTE' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_fallar('Intentos: el cuarto envío se rechaza', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$
], 'SC_KYC_SIN_INTENTOS');

-- -----------------------------------------------------------------------------
-- Identidad única solo entre expedientes enviados o verificados
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Unicidad: otra persona declara la misma CURP sin bloquear a nadie', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, curp)
     VALUES ('4c000000-0000-0000-0000-00000000004c', 'FISICA', 'Impostor', 'X', 'imp@example.mx', 'HAXF990508HDFYXR03')$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_fallar('Unicidad: no puede enviar a revisión una CURP en revisión o verificada', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET rfc = 'HAXF990508AB2', fecha_nacimiento = '1999-05-08', sexo = 'H', entidad_nacimiento = 'DF',
       domicilio = '{"calle": "a", "numero_exterior": "1", "colonia": "c", "municipio": "m", "estado": "e", "codigo_postal": "06000"}',
       ine_clave_elector = 'HYXXFR99050809H101', ine_cic = '123456780', ine_emision = 2020, ine_vigencia = 2030,
       evidencia = '{"ine_frente": "a", "ine_reverso": "b", "selfie": ["c"]}'
     WHERE cliente_id = '4c000000-0000-0000-0000-00000000004c'$q$,
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'RECHAZADO', motivo_rechazo = 'r' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$,
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'PENDIENTE' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$,
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = '3c000000-0000-0000-0000-00000000003c'$q$,
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = '4c000000-0000-0000-0000-00000000004c'$q$
], 'ux_perfiles_curp');

-- -----------------------------------------------------------------------------
-- Inversión (mutuo) y crédito: RLS en la SOFOM
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('RLS: un cliente PENDIENTE no firma un mutuo', ARRAY[
  'SET LOCAL ROLE sc_puente',
  $q$INSERT INTO sofom.contratos_mutuo (cliente_id, cuenta_sofom_id, cuenta_ifpe_origen_id, tx_aportacion_ifpe_id,
       principal, tasa_anual, plazo_dias, fecha_inicio, hash_terminos)
     VALUES ('4c000000-0000-0000-0000-00000000004c', gen_random_uuid(), gen_random_uuid(), gen_random_uuid(),
             100, 0.05, 90, current_date, repeat('0', 64))$q$
], 'row-level security');

SELECT pg_temp.debe_fallar('RLS: un cliente PENDIENTE no recibe crédito', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$INSERT INTO sofom.creditos (acreditado_id, principal, tasa_anual, plazo_dias, destino)
     VALUES ('4c000000-0000-0000-0000-00000000004c', 100, 0.2, 30, 'capital de trabajo')$q$
], 'row-level security');

ROLLBACK;
\o
\echo 'tests/db/0006_kyc.sql: todas las pruebas pasaron (ROLLBACK)'
