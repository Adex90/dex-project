-- =============================================================================
--  Pruebas de la migración 0017 (declaraciones PLD y contratos del
--  onboarding). Corren en una transacción que termina en ROLLBACK: no dejan
--  datos.
--
--    psql "$DATABASE_URL" -f tests/db/0017_kyc_declaraciones.sql
--
--  Lo que la base de datos garantiza por sí sola: un expediente no se envía
--  sin la ocupación, el origen de los recursos (del catálogo), la declaración
--  PEP y la aceptación de la versión vigente de los contratos, cuyo instante
--  fija ella; nada de eso cambia después del envío; un expediente que declara
--  PEP solo lo verifica un operador; al verificar, la ocupación y el origen
--  pasan al cliente, y una declaración PEP lo marca PEP con riesgo ALTO (y la
--  bitácora PLD registra el aumento). backend/tests/kyc.rs cubre el camino
--  completo desde POST /api/v1/kyc/verificar.
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

-- Sin bloque EXCEPTION: la verificación automática exige un resultado de la
-- misma transacción.
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

-- Identidad, domicilio, INE y evidencia completos (como dueño): solo faltan
-- las declaraciones y los contratos de 0017.
CREATE FUNCTION pg_temp.identidad(cliente UUID, curp TEXT, clave TEXT) RETURNS void
LANGUAGE sql AS $$
    UPDATE kyc.kyc_perfiles SET curp = $2, rfc = substr($2, 1, 10) || 'AB1', nombres = 'Persona', apellido_paterno = 'Prueba',
           fecha_nacimiento = '1990-01-01', sexo = 'H', entidad_nacimiento = 'DF',
           domicilio = '{"calle": "a", "numero_exterior": "1", "colonia": "c", "municipio": "m", "estado": "e", "codigo_postal": "06000"}',
           ine_clave_elector = $3, ine_cic = '123456789', ine_emision = 2022, ine_vigencia = 2032,
           evidencia = '{"ine_frente": "a", "ine_reverso": "b", "selfie": ["c"]}'
     WHERE cliente_id = $1
$$;

-- El resultado del proveedor para el envío en curso, como sc_kyc_verificador.
CREATE FUNCTION pg_temp.resultado(cliente TEXT, resultado TEXT, score TEXT) RETURNS TEXT
LANGUAGE sql AS $$
    SELECT format($q$INSERT INTO kyc.kyc_auditoria_biometrica (cliente_id, verificacion_id, proveedor, score_similitud,
                      prueba_vida_exitosa, documento_valido, curp_ocr, clave_elector_ocr, coincide_identidad, resultado,
                      firma, resultado_crudo)
                   SELECT cliente_id, verificacion_en_curso, 'certificado', %s, true, true, curp, ine_clave_elector, true,
                          %L, 'f', '{}' FROM kyc.kyc_perfiles WHERE cliente_id = %L$q$, score, resultado, cliente)
$$;

-- -----------------------------------------------------------------------------
-- Datos: el operador, Ana y Beto (PENDIENTE, nivel BAJO).
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('0f000000-0000-0000-0000-0000000000f0', 'FISICA', 'Operador', 'Cumplimiento', 'op@example.mx', 'VERIFICADO', now(), 'BAJO');
INSERT INTO kyc.credenciales (cliente_id, password_hash, rol) VALUES ('0f000000-0000-0000-0000-0000000000f0', 'x', 'OPERADOR');
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, nivel_riesgo_pld) VALUES
  ('a1000000-0000-0000-0000-0000000000a1', 'FISICA', 'Ana', 'Prueba', 'ana@example.mx', 'BAJO'),
  ('b1000000-0000-0000-0000-0000000000b1', 'FISICA', 'Beto', 'Prueba', 'beto@example.mx', 'BAJO');
SELECT pg_temp.identidad('a1000000-0000-0000-0000-0000000000a1', 'PEPX900101HDFRRR03', 'PRPRPR90010109H100');
SELECT pg_temp.identidad('b1000000-0000-0000-0000-0000000000b1', 'OTPX900101HDFRRR05', 'OTOTOT90010109H100');

-- -----------------------------------------------------------------------------
-- Estructura y privilegios
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('la versión vigente de los contratos vive en kyc.parametros',
  (SELECT contratos_version = '2026-11' FROM kyc.parametros));

SELECT pg_temp.exigir('sc_kyc escribe las declaraciones y la versión aceptada, no el instante; sc_cliente no lee el expediente',
  has_column_privilege('sc_kyc', 'kyc.kyc_perfiles', 'ocupacion', 'UPDATE')
  AND has_column_privilege('sc_kyc', 'kyc.kyc_perfiles', 'origen_recursos', 'UPDATE')
  AND has_column_privilege('sc_kyc', 'kyc.kyc_perfiles', 'declara_pep', 'UPDATE')
  AND has_column_privilege('sc_kyc', 'kyc.kyc_perfiles', 'contratos_version', 'UPDATE')
  AND NOT has_column_privilege('sc_kyc', 'kyc.kyc_perfiles', 'contratos_aceptados_en', 'UPDATE')
  AND NOT has_column_privilege('sc_kyc_verificador', 'kyc.kyc_perfiles', 'declara_pep', 'UPDATE')
  AND NOT has_table_privilege('sc_cliente', 'kyc.kyc_perfiles', 'SELECT'));

-- -----------------------------------------------------------------------------
-- El envío exige las declaraciones y los contratos vigentes
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('sin declaraciones no hay envío', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = 'a1000000-0000-0000-0000-0000000000a1'$q$
], 'SC_KYC_DECLARACIONES_INCOMPLETAS');

SELECT pg_temp.debe_fallar('ni con una declaración a medias (sin PEP)', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET ocupacion = 'Ingeniera civil', origen_recursos = 'SALARIO',
       contratos_version = '2026-11', estado_kyc = 'EN_REVISION'
     WHERE cliente_id = 'a1000000-0000-0000-0000-0000000000a1'$q$
], 'SC_KYC_DECLARACIONES_INCOMPLETAS');

SELECT pg_temp.debe_fallar('un origen de los recursos fuera del catálogo', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET origen_recursos = 'LOTERIA' WHERE cliente_id = 'a1000000-0000-0000-0000-0000000000a1'$q$
], 'ck_perfiles_origen_recursos');

SELECT pg_temp.debe_fallar('una ocupación vacía', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET ocupacion = ' ' WHERE cliente_id = 'a1000000-0000-0000-0000-0000000000a1'$q$
], 'ck_perfiles_ocupacion');

SELECT pg_temp.debe_pasar('Ana declara: ingeniera civil, salario, no PEP', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET ocupacion = 'Ingeniera civil', origen_recursos = 'SALARIO', declara_pep = FALSE
     WHERE cliente_id = 'a1000000-0000-0000-0000-0000000000a1'$q$,
  'RESET ROLE'
]);

SELECT pg_temp.debe_fallar('sin aceptar los contratos no hay envío', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'EN_REVISION' WHERE cliente_id = 'a1000000-0000-0000-0000-0000000000a1'$q$
], 'SC_KYC_CONTRATOS_SIN_ACEPTAR');

SELECT pg_temp.debe_fallar('ni aceptando una versión anterior', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET contratos_version = '2026-10', estado_kyc = 'EN_REVISION'
     WHERE cliente_id = 'a1000000-0000-0000-0000-0000000000a1'$q$
], 'SC_KYC_CONTRATOS_SIN_ACEPTAR');

SELECT pg_temp.debe_fallar('el instante de la aceptación no lo elige la aplicación', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET contratos_aceptados_en = '2020-01-01' WHERE cliente_id = 'a1000000-0000-0000-0000-0000000000a1'$q$
], 'permission denied');

SELECT pg_temp.debe_pasar('Ana acepta los contratos vigentes y envía', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET contratos_version = '2026-11', estado_kyc = 'EN_REVISION'
     WHERE cliente_id = 'a1000000-0000-0000-0000-0000000000a1'$q$,
  'RESET ROLE'
]);

SELECT pg_temp.exigir('la base de datos fijó el instante de la aceptación', (
  SELECT estado_kyc = 'EN_REVISION' AND contratos_version = '2026-11' AND contratos_aceptados_en IS NOT NULL
         AND contratos_aceptados_en <= clock_timestamp()
    FROM kyc.kyc_perfiles WHERE cliente_id = 'a1000000-0000-0000-0000-0000000000a1'));

SELECT pg_temp.debe_fallar('después del envío las declaraciones no cambian (ni como dueño)', ARRAY[
  $q$UPDATE kyc.kyc_perfiles SET ocupacion = 'Otra cosa' WHERE cliente_id = 'a1000000-0000-0000-0000-0000000000a1'$q$
], 'SC_KYC_IDENTIDAD_CONGELADA');

SELECT pg_temp.debe_fallar('ni la declaración PEP', ARRAY[
  $q$UPDATE kyc.kyc_perfiles SET declara_pep = TRUE WHERE cliente_id = 'a1000000-0000-0000-0000-0000000000a1'$q$
], 'SC_KYC_IDENTIDAD_CONGELADA');

-- -----------------------------------------------------------------------------
-- Verificación automática (no PEP): las declaraciones pasan al cliente
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana: resultado APROBADO y verificación automática', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  pg_temp.resultado('a1000000-0000-0000-0000-0000000000a1', 'APROBADO', '0.95'),
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'VERIFICADO' WHERE cliente_id = 'a1000000-0000-0000-0000-0000000000a1'$q$,
  'RESET ROLE'
]);

SELECT pg_temp.exigir('Ana: ocupación y origen en el cliente; no PEP y su riesgo intacto', (
  SELECT estatus_kyc = 'VERIFICADO' AND ocupacion = 'Ingeniera civil' AND origen_recursos = 'SALARIO'
         AND NOT es_pep AND nivel_riesgo_pld = 'BAJO'
    FROM kyc.clientes WHERE id = 'a1000000-0000-0000-0000-0000000000a1'));

-- -----------------------------------------------------------------------------
-- PEP declarado: diligencia reforzada
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Beto declara: diputado local, salario, PEP; acepta y envía', ARRAY[
  'SET LOCAL ROLE sc_kyc',
  $q$UPDATE kyc.kyc_perfiles SET ocupacion = 'Diputado local', origen_recursos = 'SALARIO', declara_pep = TRUE,
       contratos_version = '2026-11', estado_kyc = 'EN_REVISION'
     WHERE cliente_id = 'b1000000-0000-0000-0000-0000000000b1'$q$,
  'RESET ROLE'
]);

SELECT pg_temp.debe_fallar('un PEP declarado no se verifica de forma automática (aun APROBADO)', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  pg_temp.resultado('b1000000-0000-0000-0000-0000000000b1', 'APROBADO', '0.97'),
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'VERIFICADO' WHERE cliente_id = 'b1000000-0000-0000-0000-0000000000b1'$q$
], 'SC_KYC_PEP_REQUIERE_APROBACION');

SELECT pg_temp.debe_pasar('el resultado del proveedor lo deja en revisión', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  pg_temp.resultado('b1000000-0000-0000-0000-0000000000b1', 'REVISION', '0.97'),
  'RESET ROLE'
]);

SELECT pg_temp.debe_pasar('un operador lo aprueba', ARRAY[
  'SET LOCAL ROLE sc_kyc_verificador',
  $q$SELECT set_config('sc.operador_id', '0f000000-0000-0000-0000-0000000000f0', true)$q$,
  $q$UPDATE kyc.kyc_perfiles SET estado_kyc = 'VERIFICADO' WHERE cliente_id = 'b1000000-0000-0000-0000-0000000000b1'$q$,
  $q$SELECT set_config('sc.operador_id', '', true)$q$,
  'RESET ROLE'
]);

SELECT pg_temp.exigir('Beto: verificado por el operador, PEP con riesgo ALTO y sus declaraciones', (
  SELECT c.estatus_kyc = 'VERIFICADO' AND c.es_pep AND c.nivel_riesgo_pld = 'ALTO' AND c.riesgo_evaluado_en IS NOT NULL
         AND c.ocupacion = 'Diputado local' AND c.origen_recursos = 'SALARIO'
         AND p.revisado_por = '0f000000-0000-0000-0000-0000000000f0'
    FROM kyc.clientes c JOIN kyc.kyc_perfiles p ON p.cliente_id = c.id
   WHERE c.id = 'b1000000-0000-0000-0000-0000000000b1'));

SELECT pg_temp.exigir('la bitácora PLD registra el aumento de riesgo (BAJO -> ALTO)', (
  SELECT count(*) = 1 FROM aml.bitacora
   WHERE evento = 'RIESGO_CLIENTE_ELEVADO' AND cliente_id = 'b1000000-0000-0000-0000-0000000000b1'
     AND estado_anterior = 'BAJO' AND estado_nuevo = 'ALTO'));

\o
\echo 'tests/db/0017_kyc_declaraciones.sql: todas las pruebas pasaron (ROLLBACK)'
ROLLBACK;
