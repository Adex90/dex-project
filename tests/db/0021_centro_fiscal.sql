-- =============================================================================
--  Pruebas de la migración 0021 (centro fiscal: W-8BEN y parámetros del
--  ejercicio). Corren en una transacción que termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0021_centro_fiscal.sql
--
--  Lo que la base de datos garantiza por sí sola: las tasas de cada ejercicio
--  no cambian; solo una persona física verificada, residente en México y que
--  no es persona de EUA certifica el W-8BEN; la Parte I sale del expediente,
--  la firma es el nombre legal, las tres certificaciones son verdaderas, y la
--  versión, la fecha, la vigencia (31 de diciembre del tercer año siguiente)
--  y la cadena de hashes las pone la base de datos; nada se corrige ni se
--  borra; cada versión deja su evento en la bitácora de seguridad, el único
--  que sc_kyc puede escribir además del de 0020; el cliente lee lo suyo.
--  backend/tests/impuestos.rs cubre la API y la cadena recalculada en Rust.
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

-- Una certificación como la guarda el backend (sc_kyc): las tres
-- certificaciones, la firma y quién firmó; lo demás lo pone la base de datos.
CREATE FUNCTION pg_temp.w8ben(id TEXT, cliente TEXT, firma TEXT,
                              beneficiario BOOLEAN DEFAULT true, no_eua BOOLEAN DEFAULT true,
                              residente BOOLEAN DEFAULT true) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY['SET LOCAL ROLE sc_kyc',
        format('INSERT INTO kyc.formularios_w8ben (id, cliente_id, beneficiario_efectivo, no_persona_eua, residente_tratado, firma, ip, dispositivo) VALUES (%L, %L, %L, %L, %L, %L, %L, %L)',
               id, cliente, beneficiario, no_eua, residente, firma, '203.0.113.7', 'Chrome on macOS'),
        'RESET ROLE']
$$;

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
-- Datos: Ana puede certificar; los demás, por una razón cada uno, no
-- -----------------------------------------------------------------------------
\set domicilio '''{"calle": "Av. Reforma", "numero_exterior": "222", "numero_interior": "4B", "colonia": "Juárez", "municipio": "Cuauhtémoc", "estado": "Ciudad de México", "codigo_postal": "06600"}'''
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, apellido_materno, email, rfc, curp, fecha_nacimiento,
                          nacionalidad, pais_residencia, domicilio, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ana', 'Rojas', 'Núñez', 'ana@example.mx', 'RONA850315AB1',
   'RONA850315MDFJXN03', '1985-03-15', 'MX', 'MX', :domicilio, 'VERIFICADO', now(), 'BAJO'),
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Beto', 'Ibarra', NULL, 'beto@example.mx', 'IABE800101AB1',
   NULL, '1980-01-01', 'US', 'MX', :domicilio, 'VERIFICADO', now(), 'BAJO'),
  ('c0000000-0000-0000-0000-00000000000c', 'FISICA', 'Carla', 'Mena', NULL, 'carla@example.mx', 'MECA900101AB1',
   NULL, '1990-01-01', 'MX', 'MX', :domicilio, 'PENDIENTE', NULL, 'BAJO'),
  ('d0000000-0000-0000-0000-00000000000d', 'MORAL', 'Dorado SA de CV', NULL, NULL, 'dorado@example.mx', 'DOR100101AB1',
   NULL, NULL, 'MX', 'MX', :domicilio, 'VERIFICADO', now(), 'BAJO'),
  ('e0000000-0000-0000-0000-00000000000e', 'FISICA', 'Eva', 'Soto', NULL, 'eva@example.mx', 'SOEV900101AB1',
   NULL, '1990-01-01', 'MX', 'ES', :domicilio, 'VERIFICADO', now(), 'BAJO'),
  ('f0000000-0000-0000-0000-00000000000f', 'FISICA', 'Fer', 'Luna', NULL, 'fer@example.mx', NULL,
   NULL, '1990-01-01', 'MX', 'MX', :domicilio, 'VERIFICADO', now(), 'BAJO');

-- -----------------------------------------------------------------------------
-- 1. Parámetros fiscales de cada ejercicio
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('2025: 0.50% anual sobre el capital; 2026: 0.90% (LIF); ganancia en acciones 10%; EUA 10% con W-8BEN, 30% sin', (
  SELECT array_agg(format('%s:%s:%s:%s:%s', ejercicio, tasa_retencion_intereses, tasa_ganancia_acciones,
                          tasa_dividendos_eua_tratado, tasa_dividendos_eua_sin_w8ben) ORDER BY ejercicio)
         = ARRAY['2025:0.5000:10.0000:10.0000:30.0000', '2026:0.9000:10.0000:10.0000:30.0000']
    FROM kyc.parametros_fiscales));
SELECT pg_temp.debe_fallar('un ejercicio publicado no cambia (ni para el dueño del esquema)',
  ARRAY['UPDATE kyc.parametros_fiscales SET tasa_retencion_intereses = 0.15 WHERE ejercicio = 2026'], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('ni se borra',
  ARRAY['DELETE FROM kyc.parametros_fiscales WHERE ejercicio = 2025'], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('el tratado nunca retiene más que la tasa legal',
  ARRAY[$q$INSERT INTO kyc.parametros_fiscales VALUES (2027, 0.9, 'LIF 2027', 10, 'LISR art. 129', 35, 30, 'Tratado')$q$],
  'ck_parametros_fiscales_beneficio');
SELECT pg_temp.debe_fallar('sc_kyc no publica tasas',
  ARRAY['SET LOCAL ROLE sc_kyc',
        $q$INSERT INTO kyc.parametros_fiscales VALUES (2027, 0.9, 'LIF 2027', 10, 'LISR art. 129', 10, 30, 'Tratado')$q$],
  'permission denied');
SELECT pg_temp.exigir('el cliente lee las tasas de cada ejercicio', (
  pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM kyc.parametros_fiscales') = 2));

-- -----------------------------------------------------------------------------
-- 2. La Parte I sale del expediente
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('el nombre legal: espacios simples, NFC y mayúsculas con acentos', (
  kyc.nombre_legal('  ana ', 'rojas', 'núñez') = 'ANA ROJAS NÚÑEZ'
  AND kyc.nombre_legal('Ana', 'Rojas', NULL) = 'ANA ROJAS'
  AND kyc.nombre_legal(normalize('Ana', NFD), normalize('Núñez', NFD), NULL) = 'ANA NÚÑEZ'));
SELECT pg_temp.exigir('el domicilio permanente en una línea, con el país', (
  kyc.domicilio_w8ben(:domicilio) = 'Av. Reforma 222 Int. 4B, Juárez, Cuauhtémoc, Ciudad de México, C.P. 06600, México'));

-- -----------------------------------------------------------------------------
-- 3. Una certificación válida
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana certifica; su firma se normaliza (minúsculas, espacios dobles)',
  pg_temp.w8ben('f1000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a', '  ana  rojas núñez '));
SELECT pg_temp.exigir('versión 1: la Parte I copiada del expediente, firmada hoy por la base de datos, vigente hasta el 31 de diciembre del tercer año siguiente', (
  SELECT version = 1
     AND nombre_legal = 'ANA ROJAS NÚÑEZ' AND firma = nombre_legal
     AND pais_ciudadania = 'MX' AND pais_residencia = 'MX' AND pais_tratado = 'MX'
     AND domicilio = 'Av. Reforma 222 Int. 4B, Juárez, Cuauhtémoc, Ciudad de México, C.P. 06600, México'
     AND tin_extranjero = 'RONA850315AB1' AND fecha_nacimiento = '1985-03-15'
     AND firmado_en = now()
     AND vigente_hasta = make_date(extract(year FROM now() AT TIME ZONE 'America/Mexico_City')::int + 3, 12, 31)
     AND hash_anterior = repeat('0', 64) AND hash = nucleo.sha256_hex(kyc.payload_w8ben(f))
    FROM kyc.formularios_w8ben f WHERE id = 'f1000000-0000-0000-0000-000000000001')
  AND kyc.verificar_w8ben('a0000000-0000-0000-0000-00000000000a') IS NULL);
SELECT pg_temp.exigir('y su evento W8BEN_CERTIFICADO en la bitácora, con la IP, el dispositivo y la versión, escrito por sc_kyc', (
  SELECT count(*) = 1 AND bool_and(detalle = '1' AND ip = '203.0.113.7' AND dispositivo = 'Chrome on macOS'
                                    AND registrado_por = 'sc_kyc')
    FROM kyc.eventos_autenticacion
   WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND evento = 'W8BEN_CERTIFICADO'));

-- -----------------------------------------------------------------------------
-- 4. La firma y las certificaciones
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('la firma es el nombre legal completo',
  pg_temp.w8ben('f4000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a', 'Ana Rojas'),
  'SC_W8BEN_FIRMA');
SELECT pg_temp.debe_fallar('sin firma no hay certificación',
  pg_temp.w8ben('f4000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a', ''),
  'SC_W8BEN_FIRMA');
SELECT pg_temp.debe_fallar('el cliente certifica que es el beneficiario efectivo',
  pg_temp.w8ben('f4000000-0000-0000-0000-000000000003', 'a0000000-0000-0000-0000-00000000000a', 'Ana Rojas Núñez',
                false, true, true),
  'ck_w8ben_beneficiario');
SELECT pg_temp.debe_fallar('que no es persona de EUA',
  pg_temp.w8ben('f4000000-0000-0000-0000-000000000004', 'a0000000-0000-0000-0000-00000000000a', 'Ana Rojas Núñez',
                true, false, true),
  'ck_w8ben_no_persona_eua');
SELECT pg_temp.debe_fallar('y que reside en México para el tratado',
  pg_temp.w8ben('f4000000-0000-0000-0000-000000000005', 'a0000000-0000-0000-0000-00000000000a', 'Ana Rojas Núñez',
                true, true, false),
  'ck_w8ben_residente_tratado');

-- -----------------------------------------------------------------------------
-- 5. Quién puede certificar
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('un ciudadano de EUA presenta el W-9',
  pg_temp.w8ben('f5000000-0000-0000-0000-000000000001', 'b0000000-0000-0000-0000-00000000000b', 'Beto Ibarra'),
  'SC_W8BEN_PERSONA_EUA');
SELECT pg_temp.debe_fallar('sin identidad verificada no se certifica',
  pg_temp.w8ben('f5000000-0000-0000-0000-000000000002', 'c0000000-0000-0000-0000-00000000000c', 'Carla Mena'),
  'SC_KYC_NO_VERIFICADO');
SELECT pg_temp.debe_fallar('una empresa presenta el W-8BEN-E',
  pg_temp.w8ben('f5000000-0000-0000-0000-000000000003', 'd0000000-0000-0000-0000-00000000000d', 'Dorado SA de CV'),
  'SC_W8BEN_PERSONA_MORAL');
SELECT pg_temp.debe_fallar('aquí certifica un residente de México',
  pg_temp.w8ben('f5000000-0000-0000-0000-000000000004', 'e0000000-0000-0000-0000-00000000000e', 'Eva Soto'),
  'SC_W8BEN_RESIDENCIA');
SELECT pg_temp.debe_fallar('sin RFC (el TIN extranjero) no se certifica',
  pg_temp.w8ben('f5000000-0000-0000-0000-000000000005', 'f0000000-0000-0000-0000-00000000000f', 'Fer Luna'),
  'SC_W8BEN_DATOS_INCOMPLETOS');
SELECT pg_temp.debe_fallar('sc_kyc no escribe la Parte I',
  ARRAY['SET LOCAL ROLE sc_kyc',
        $q$INSERT INTO kyc.formularios_w8ben (id, cliente_id, nombre_legal, beneficiario_efectivo, no_persona_eua, residente_tratado, firma)
           VALUES ('f5000000-0000-0000-0000-000000000006', 'a0000000-0000-0000-0000-00000000000a', 'OTRA PERSONA', true, true, true, 'Otra Persona')$q$],
  'permission denied');
SELECT pg_temp.debe_fallar('ni la vigencia',
  ARRAY['SET LOCAL ROLE sc_kyc',
        $q$INSERT INTO kyc.formularios_w8ben (id, cliente_id, beneficiario_efectivo, no_persona_eua, residente_tratado, firma, vigente_hasta)
           VALUES ('f5000000-0000-0000-0000-000000000007', 'a0000000-0000-0000-0000-00000000000a', true, true, true, 'Ana Rojas Núñez', '2099-12-31')$q$],
  'permission denied');
SELECT pg_temp.debe_fallar('ni la versión',
  ARRAY['SET LOCAL ROLE sc_kyc',
        $q$INSERT INTO kyc.formularios_w8ben (id, cliente_id, version, beneficiario_efectivo, no_persona_eua, residente_tratado, firma)
           VALUES ('f5000000-0000-0000-0000-000000000008', 'a0000000-0000-0000-0000-00000000000a', 9, true, true, true, 'Ana Rojas Núñez')$q$],
  'permission denied');

-- -----------------------------------------------------------------------------
-- 6. Renovar: una versión nueva, encadenada
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana renueva',
  pg_temp.w8ben('f6000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a', 'ANA ROJAS NÚÑEZ'));
SELECT pg_temp.exigir('dos versiones encadenadas, cada una con su evento; las rechazadas no dejaron ninguno', (
  SELECT array_agg(version ORDER BY version) = ARRAY[1, 2]
     AND (SELECT hash FROM kyc.formularios_w8ben WHERE version = 1 AND cliente_id = f.cliente_id)
         = max(hash_anterior) FILTER (WHERE version = 2)
    FROM kyc.formularios_w8ben f WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' GROUP BY cliente_id)
  AND kyc.verificar_w8ben('a0000000-0000-0000-0000-00000000000a') IS NULL
  AND (SELECT array_agg(detalle ORDER BY id) = ARRAY['1', '2'] FROM kyc.eventos_autenticacion
        WHERE evento = 'W8BEN_CERTIFICADO')
  AND NOT EXISTS (SELECT 1 FROM kyc.formularios_w8ben WHERE cliente_id <> 'a0000000-0000-0000-0000-00000000000a'));

-- -----------------------------------------------------------------------------
-- 7. Inmutable
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('una certificación no se corrige (ni por el dueño del esquema)',
  ARRAY[$q$UPDATE kyc.formularios_w8ben SET vigente_hasta = '2099-12-31' WHERE version = 1$q$], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('ni se borra',
  ARRAY['DELETE FROM kyc.formularios_w8ben WHERE version = 2'], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('ni se vacía',
  ARRAY['TRUNCATE kyc.formularios_w8ben'], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('sc_kyc no tiene permiso para corregir',
  ARRAY['SET LOCAL ROLE sc_kyc', $q$UPDATE kyc.formularios_w8ben SET firma = 'X'$q$], 'permission denied');

-- -----------------------------------------------------------------------------
-- 8. Bitácora de seguridad: sc_kyc escribe solo estos eventos
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('sc_kyc no escribe el evento de una versión que no existe',
  ARRAY['SET LOCAL ROLE sc_kyc',
        $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, dispositivo, detalle)
           VALUES ('a0000000-0000-0000-0000-00000000000a', 'W8BEN_CERTIFICADO', '203.0.113.7', 'Chrome on macOS', '9')$q$],
  'row-level security');
SELECT pg_temp.debe_fallar('ni uno de otro dispositivo',
  ARRAY['SET LOCAL ROLE sc_kyc',
        $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, dispositivo, detalle)
           VALUES ('a0000000-0000-0000-0000-00000000000a', 'W8BEN_CERTIFICADO', '203.0.113.7', 'Edge on Windows', '1')$q$],
  'row-level security');
SELECT pg_temp.debe_fallar('ni un segundo evento de la misma versión',
  ARRAY['SET LOCAL ROLE sc_kyc',
        $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, dispositivo, detalle)
           VALUES ('a0000000-0000-0000-0000-00000000000a', 'W8BEN_CERTIFICADO', '203.0.113.7', 'Chrome on macOS', '1')$q$],
  'ux_eventos_autenticacion_w8ben');
SELECT pg_temp.debe_fallar('ni un evento de otra clase',
  ARRAY['SET LOCAL ROLE sc_kyc',
        $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento)
           VALUES ('a0000000-0000-0000-0000-00000000000a', 'INICIO_SESION')$q$],
  'row-level security');

-- -----------------------------------------------------------------------------
-- 9. Privilegios y RLS
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('RLS en las versiones, con la política RESTRICTIVE de sc_cliente', (
  SELECT relrowsecurity FROM pg_class WHERE oid = 'kyc.formularios_w8ben'::regclass)
  AND (SELECT count(*) = 1 FROM pg_policies
        WHERE schemaname = 'kyc' AND tablename = 'formularios_w8ben' AND policyname = 'solo_del_cliente'
          AND permissive = 'RESTRICTIVE' AND roles = '{sc_cliente}'));
SELECT pg_temp.exigir('Ana ve sus 2 versiones y sus 2 eventos; Beto, nada', (
  pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM kyc.formularios_w8ben') = 2
  AND pg_temp.visibles('a0000000-0000-0000-0000-00000000000a',
        $q$SELECT count(*) FROM kyc.eventos_autenticacion WHERE evento = 'W8BEN_CERTIFICADO'$q$) = 2
  AND pg_temp.visibles('b0000000-0000-0000-0000-00000000000b', 'SELECT count(*) FROM kyc.formularios_w8ben') = 0
  AND pg_temp.visibles('', 'SELECT count(*) FROM kyc.formularios_w8ben') = 0));
SELECT pg_temp.debe_fallar('el cliente no certifica directamente',
  ARRAY[$q$SELECT set_config('sc.cliente_actual', 'a0000000-0000-0000-0000-00000000000a', true)$q$,
        'SET LOCAL ROLE sc_cliente',
        $q$INSERT INTO kyc.formularios_w8ben (id, cliente_id, beneficiario_efectivo, no_persona_eua, residente_tratado, firma)
           VALUES ('f9000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a', true, true, true, 'Ana Rojas Núñez')$q$],
  'permission denied');
SELECT pg_temp.exigir('ningún otro rol de la aplicación toca el W-8BEN ni las tasas', (
  SELECT bool_and(NOT has_any_column_privilege(r, t, 'SELECT, INSERT, UPDATE, REFERENCES'))
    FROM unnest(ARRAY['sc_ifpe', 'sc_sofom', 'sc_puente', 'sc_spei', 'sc_mercado', 'sc_aml', 'sc_auditor',
                      'sc_auditor_pld', 'sc_kyc_verificador', 'sc_auth']) AS r,
         unnest(ARRAY['kyc.formularios_w8ben', 'kyc.parametros_fiscales']) AS t));

-- -----------------------------------------------------------------------------
-- 10. La cadena delata una alteración por fuera de la aplicación
-- -----------------------------------------------------------------------------
SET CONSTRAINTS ALL IMMEDIATE;
ALTER TABLE kyc.formularios_w8ben DISABLE TRIGGER trg_w8ben_inmutable;
UPDATE kyc.formularios_w8ben SET vigente_hasta = '2099-12-31' WHERE version = 1;
ALTER TABLE kyc.formularios_w8ben ENABLE TRIGGER trg_w8ben_inmutable;
SELECT pg_temp.exigir('alargar la vigencia de una versión guardada rompe la cadena en esa versión',
  kyc.verificar_w8ben('a0000000-0000-0000-0000-00000000000a') = 1);

\o
\echo 'tests/db/0021_centro_fiscal.sql: todas las pruebas pasaron (ROLLBACK)'
ROLLBACK;
