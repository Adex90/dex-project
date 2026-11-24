-- =============================================================================
--  Pruebas de la migración 0024 (lista de seguimiento y alertas de precio).
--  Corren en una transacción que termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0024_seguimiento_alertas.sql
--
--  Lo que la base de datos garantiza por sí sola: seguir un activo es un
--  renglón que se agrega o se borra, nunca se edita, con su fecha y dentro del
--  máximo; una alerta de precio nace activa con el precio del catálogo como
--  referencia y un objetivo que aún no se alcanza; sus términos no cambian;
--  se dispara una sola vez, con el precio vigente del catálogo que cumple el
--  objetivo, o se cancela, y ahí se queda; nunca se borra; cada cambio avisa a
--  la bóveda del cliente; escribe sc_mercado y el cliente lee solo lo suyo.
--  backend/tests/seguimiento.rs cubre la API y el motor que las dispara.
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

-- Como lo hace el backend: con el rol sc_mercado.
CREATE FUNCTION pg_temp.como_mercado(sentencia TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$ SELECT ARRAY['SET LOCAL ROLE sc_mercado', sentencia, 'RESET ROLE'] $$;

CREATE FUNCTION pg_temp.seguir(cliente TEXT, ticker TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT pg_temp.como_mercado(format($q$INSERT INTO mercado.watchlist (cliente_id, ticker) VALUES (%L, %L)$q$,
                                       cliente, ticker))
$$;

CREATE FUNCTION pg_temp.alerta(id TEXT, cliente TEXT, ticker TEXT, objetivo TEXT, direccion TEXT, clave TEXT)
RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT pg_temp.como_mercado(format(
        $q$INSERT INTO mercado.alertas_precio (id, cliente_id, ticker, precio_objetivo, direccion, clave_idempotencia)
           VALUES (%L, %L, %L, %s, %L, %L)$q$, id, cliente, ticker, objetivo, direccion, clave))
$$;

CREATE FUNCTION pg_temp.cambiar(id TEXT, asignacion TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT pg_temp.como_mercado(format($q$UPDATE mercado.alertas_precio SET %s WHERE id = %L$q$, asignacion, id))
$$;

-- El oráculo mueve el precio (como dueño del esquema).
CREATE FUNCTION pg_temp.precio(ticker TEXT, nuevo TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[format($q$UPDATE mercado.activos SET precio = %s WHERE ticker = %L$q$, nuevo, ticker)]
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

CREATE FUNCTION pg_temp.version(cuenta UUID) RETURNS BIGINT
LANGUAGE sql AS $$ SELECT version_ordenes FROM ifpe.cuentas WHERE id = cuenta $$;

-- -----------------------------------------------------------------------------
-- Datos: Ana y Beto verificados, Carla sin KYC (seguir y avisar no lo exigen)
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ana',   'Prueba', 'ana@example.mx',   'VERIFICADO', now(), 'BAJO'),
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Beto',  'Prueba', 'beto@example.mx',  'VERIFICADO', now(), 'BAJO');
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, nivel_riesgo_pld) VALUES
  ('c0000000-0000-0000-0000-00000000000c', 'FISICA', 'Carla', 'Prueba', 'carla@example.mx', 'BAJO');
INSERT INTO ifpe.cuentas (id, cliente_id, clabe) VALUES
  ('1a000000-0000-0000-0000-00000000001a', 'a0000000-0000-0000-0000-00000000000a', '646180157000000017'),
  ('1b000000-0000-0000-0000-00000000001b', 'b0000000-0000-0000-0000-00000000000b', '646180157000000020'),
  ('1c000000-0000-0000-0000-00000000001c', 'c0000000-0000-0000-0000-00000000000c', '646180157000000033');

-- -----------------------------------------------------------------------------
-- 1. La lista de seguimiento
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana sigue el oro y la plata', pg_temp.seguir('a0000000-0000-0000-0000-00000000000a', 'AUG1')
  || pg_temp.seguir('a0000000-0000-0000-0000-00000000000a', 'AGT1'));
SELECT pg_temp.exigir('con la fecha de la base de datos, y la bóveda avisa por cada uno', (
  SELECT count(*) = 2 AND bool_and(creado_en = now()) FROM mercado.watchlist
   WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a')
  AND pg_temp.version('1a000000-0000-0000-0000-00000000001a') = 2);
SELECT pg_temp.debe_pasar('Carla, sin KYC, también sigue el oro', pg_temp.seguir('c0000000-0000-0000-0000-00000000000c', 'AUG1'));
SELECT pg_temp.debe_fallar('un activo se sigue una vez', pg_temp.seguir('a0000000-0000-0000-0000-00000000000a', 'AUG1'),
  'watchlist_pkey');
SELECT pg_temp.debe_fallar('solo activos del catálogo', pg_temp.seguir('a0000000-0000-0000-0000-00000000000a', 'XAU'),
  'watchlist_ticker_fkey');
SELECT pg_temp.debe_fallar('sc_mercado no elige la fecha', pg_temp.como_mercado(
  $q$INSERT INTO mercado.watchlist (cliente_id, ticker, creado_en)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'STSLA', '2020-01-01')$q$), 'permission denied');
SELECT pg_temp.debe_fallar('un renglón no se edita (ni el dueño del esquema)', ARRAY[
  $q$UPDATE mercado.watchlist SET ticker = 'STSLA'
      WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND ticker = 'AGT1'$q$], 'SC_SEGUIMIENTO_INMUTABLE');
SELECT pg_temp.debe_pasar('Ana deja de seguir la plata', pg_temp.como_mercado(
  $q$DELETE FROM mercado.watchlist WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND ticker = 'AGT1'$q$));
SELECT pg_temp.exigir('el renglón se va y la bóveda avisa', (
  SELECT count(*) = 1 FROM mercado.watchlist WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a')
  AND pg_temp.version('1a000000-0000-0000-0000-00000000001a') = 3);
-- 50 activos más en el catálogo (se revierten), para el máximo.
INSERT INTO mercado.activos (ticker, nombre, clase, decimales_cantidad, precio, precio_cierre_previo, fuente_precio)
SELECT 'TST' || i, 'Activo de prueba ' || i, 'FONDO', 4, 100, 100, 'SIMULADO' FROM generate_series(1, 50) AS i;
SELECT pg_temp.debe_pasar('Beto sigue 50 activos, el máximo', (
  SELECT array_agg(s) FROM (
    SELECT unnest(pg_temp.seguir('b0000000-0000-0000-0000-00000000000b', 'TST' || i)) AS s
      FROM generate_series(1, 50) AS i) x));
SELECT pg_temp.debe_fallar('el 51 no cabe', pg_temp.seguir('b0000000-0000-0000-0000-00000000000b', 'AUG1'),
  'SC_LIMITE_SEGUIMIENTO');
SELECT pg_temp.debe_pasar('deja uno y cabe otro', pg_temp.como_mercado(
  $q$DELETE FROM mercado.watchlist WHERE cliente_id = 'b0000000-0000-0000-0000-00000000000b' AND ticker = 'TST50'$q$)
  || pg_temp.seguir('b0000000-0000-0000-0000-00000000000b', 'AUG1'));

-- -----------------------------------------------------------------------------
-- 2. El alta de una alerta
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana: avísame si el oro sube a 1 700 (está en 1 612.40)', pg_temp.alerta(
  '7a000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a', 'AUG1', '1700', 'ARRIBA',
  'cli:a:alerta-1'));
SELECT pg_temp.exigir('nace activa, con el precio del catálogo como referencia y la fecha de la base de datos', (
  SELECT estado = 'ACTIVA' AND precio_referencia = 1612.40 AND creada_en = now() AND precio_disparo IS NULL
     AND disparada_en IS NULL AND cancelada_en IS NULL
    FROM mercado.alertas_precio WHERE id = '7a000000-0000-0000-0000-000000000001')
  AND pg_temp.version('1a000000-0000-0000-0000-00000000001a') = 4);
SELECT pg_temp.debe_pasar('y si baja a 1 500', pg_temp.alerta(
  '7a000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a', 'AUG1', '1500', 'ABAJO',
  'cli:a:alerta-2'));
SELECT pg_temp.debe_fallar('un objetivo al alza que ya se alcanzó, no', pg_temp.alerta(
  '7a000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a', 'AUG1', '1612.40', 'ARRIBA',
  'cli:a:alerta-x'), 'SC_AVISO_YA_ALCANZADO');
SELECT pg_temp.debe_fallar('ni uno a la baja por encima del precio', pg_temp.alerta(
  '7a000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a', 'AUG1', '1650', 'ABAJO',
  'cli:a:alerta-x'), 'SC_AVISO_YA_ALCANZADO');
SELECT pg_temp.debe_fallar('ni otra dirección', pg_temp.alerta(
  '7a000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a', 'AUG1', '1700', 'CRUZA',
  'cli:a:alerta-x'), 'alertas_precio_direccion_check');
SELECT pg_temp.debe_fallar('ni un activo que no existe', pg_temp.alerta(
  '7a000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a', 'XAU', '1700', 'ARRIBA',
  'cli:a:alerta-x'), 'SC_AVISO_INCOHERENTE');
SELECT pg_temp.debe_fallar('sc_mercado no pone la referencia ni el estado', pg_temp.como_mercado(
  $q$INSERT INTO mercado.alertas_precio (id, cliente_id, ticker, precio_objetivo, direccion, clave_idempotencia,
                                         precio_referencia)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', 'AUG1', 1700, 'ARRIBA', 'cli:a:ref', 1)$q$),
  'permission denied');
SELECT pg_temp.debe_fallar('ni el dueño del esquema da de alta una disparada', ARRAY[
  $q$INSERT INTO mercado.alertas_precio (id, cliente_id, ticker, precio_objetivo, direccion, clave_idempotencia,
                                         precio_referencia, estado, precio_disparo, disparada_en)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', 'AUG1', 1700, 'ARRIBA', 'cli:a:disp', 1612.40,
             'DISPARADA', 1700, now())$q$], 'SC_AVISO_INCOHERENTE');
SELECT pg_temp.debe_fallar('una clave, una alerta', pg_temp.alerta(
  '7a000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a', 'AUG1', '1800', 'ARRIBA',
  'cli:a:alerta-1'), 'alertas_precio_clave_idempotencia_key');
SELECT pg_temp.debe_pasar('Carla, sin KYC, también pone una', pg_temp.alerta(
  '7c000000-0000-0000-0000-000000000001', 'c0000000-0000-0000-0000-00000000000c', 'AGT1', '600', 'ARRIBA',
  'cli:c:alerta-1'));
SELECT pg_temp.debe_pasar('Beto pone 50 alertas activas, el máximo', (
  SELECT array_agg(s) FROM (
    SELECT unnest(pg_temp.alerta(format('7b000000-0000-0000-0000-%s', lpad(i::text, 12, '0')),
                                 'b0000000-0000-0000-0000-00000000000b', 'TST' || i, '200', 'ARRIBA',
                                 'cli:b:alerta-' || i)) AS s
      FROM generate_series(1, 49) AS i
    UNION ALL
    SELECT unnest(pg_temp.alerta('7b000000-0000-0000-0000-000000000050', 'b0000000-0000-0000-0000-00000000000b',
                                 'AUG1', '1700', 'ARRIBA', 'cli:b:alerta-50'))) x));
SELECT pg_temp.debe_fallar('la 51 no cabe', pg_temp.alerta(
  '7b000000-0000-0000-0000-000000000051', 'b0000000-0000-0000-0000-00000000000b', 'AUG1', '1800', 'ARRIBA',
  'cli:b:alerta-51'), 'SC_LIMITE_AVISOS_PRECIO');
SELECT pg_temp.debe_pasar('cancela una y cabe otra: las cerradas no cuentan',
  pg_temp.cambiar('7b000000-0000-0000-0000-000000000001', $q$estado = 'CANCELADA'$q$)
  || pg_temp.alerta('7b000000-0000-0000-0000-000000000051', 'b0000000-0000-0000-0000-00000000000b', 'AUG1', '1800',
                    'ARRIBA', 'cli:b:alerta-51'));

-- -----------------------------------------------------------------------------
-- 3. El disparo
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('no se dispara antes de llegar al objetivo (se repetiría con el precio nuevo)',
  pg_temp.cambiar('7a000000-0000-0000-0000-000000000001', $q$estado = 'DISPARADA'$q$), 'SC_AVISO_NO_ALCANZADO');
SELECT pg_temp.debe_pasar('(el oro sube a 1 701.20)', pg_temp.precio('AUG1', '1701.20'));
SELECT pg_temp.debe_fallar('sc_mercado no escribe el precio del disparo', pg_temp.cambiar(
  '7a000000-0000-0000-0000-000000000001', $q$estado = 'DISPARADA', precio_disparo = 1750$q$), 'permission denied');
SELECT pg_temp.debe_fallar('la de la baja no se dispara: el oro está arriba', pg_temp.cambiar(
  '7a000000-0000-0000-0000-000000000002', $q$estado = 'DISPARADA'$q$), 'SC_AVISO_NO_ALCANZADO');
SELECT pg_temp.debe_pasar('se dispara: la base de datos anota el precio vigente del catálogo', pg_temp.cambiar(
  '7a000000-0000-0000-0000-000000000001', $q$estado = 'DISPARADA'$q$));
SELECT pg_temp.exigir('disparada, con su precio y su hora, y la bóveda avisa', (
  SELECT estado = 'DISPARADA' AND precio_disparo = 1701.20 AND disparada_en = now() AND cancelada_en IS NULL
    FROM mercado.alertas_precio WHERE id = '7a000000-0000-0000-0000-000000000001')
  AND pg_temp.version('1a000000-0000-0000-0000-00000000001a') = 6);
SELECT pg_temp.debe_fallar('una disparada ya no cambia: ni se cancela', pg_temp.cambiar(
  '7a000000-0000-0000-0000-000000000001', $q$estado = 'CANCELADA'$q$), 'SC_AVISO_CERRADO');
SELECT pg_temp.debe_fallar('una activa no se toca sin dispararse o cancelarse (ni el dueño del esquema)', ARRAY[
  $q$UPDATE mercado.alertas_precio SET precio_disparo = 1500 WHERE id = '7a000000-0000-0000-0000-000000000002'$q$],
  'SC_AVISO_TRANSICION');

-- -----------------------------------------------------------------------------
-- 4. La cancelación y los términos
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana cancela la de la baja', pg_temp.cambiar(
  '7a000000-0000-0000-0000-000000000002', $q$estado = 'CANCELADA'$q$));
SELECT pg_temp.exigir('cancelada, con su hora', (
  SELECT estado = 'CANCELADA' AND cancelada_en = now() AND disparada_en IS NULL AND precio_disparo IS NULL
    FROM mercado.alertas_precio WHERE id = '7a000000-0000-0000-0000-000000000002')
  AND pg_temp.version('1a000000-0000-0000-0000-00000000001a') = 7);
SELECT pg_temp.debe_fallar('cancelar es definitivo', pg_temp.cambiar(
  '7a000000-0000-0000-0000-000000000002', $q$estado = 'ACTIVA'$q$), 'SC_AVISO_CERRADO');
SELECT pg_temp.debe_fallar('los términos no cambian (ni para el dueño del esquema)', ARRAY[
  $q$UPDATE mercado.alertas_precio SET precio_objetivo = 1800 WHERE id = '7c000000-0000-0000-0000-000000000001'$q$],
  'SC_AVISO_INMUTABLE');
SELECT pg_temp.debe_fallar('ni la referencia', ARRAY[
  $q$UPDATE mercado.alertas_precio SET precio_referencia = 1 WHERE id = '7c000000-0000-0000-0000-000000000001'$q$],
  'SC_AVISO_INMUTABLE');
SELECT pg_temp.debe_fallar('sc_mercado no tiene permiso para cambiarlos', pg_temp.cambiar(
  '7c000000-0000-0000-0000-000000000001', 'precio_objetivo = 1800'), 'permission denied');
SELECT pg_temp.debe_pasar('el dueño del esquema cancela la de Carla con otra fecha y un precio', ARRAY[
  $q$UPDATE mercado.alertas_precio SET estado = 'CANCELADA', cancelada_en = '2020-01-01', precio_disparo = 600
      WHERE id = '7c000000-0000-0000-0000-000000000001'$q$]);
SELECT pg_temp.exigir('(la fecha y el precio son de la base de datos: cancelada ahora, sin precio)', (
  SELECT estado = 'CANCELADA' AND cancelada_en = now() AND precio_disparo IS NULL
    FROM mercado.alertas_precio WHERE id = '7c000000-0000-0000-0000-000000000001'));

-- -----------------------------------------------------------------------------
-- 5. Inmutables, privilegios y RLS
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('una alerta no se borra', ARRAY[$q$DELETE FROM mercado.alertas_precio$q$],
  'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('ni se vacía la tabla', ARRAY['TRUNCATE mercado.alertas_precio'], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.exigir('sc_mercado: alta de los términos, disparo o cancelación; seguir y dejar de seguir', (
  SELECT array_agg(column_name::text ORDER BY column_name::text COLLATE "C")
         = ARRAY['clave_idempotencia', 'cliente_id', 'direccion', 'id', 'precio_objetivo', 'ticker']
    FROM information_schema.column_privileges
   WHERE grantee = 'sc_mercado' AND table_schema = 'mercado' AND table_name = 'alertas_precio'
     AND privilege_type = 'INSERT')
  AND (SELECT array_agg(column_name::text ORDER BY column_name::text COLLATE "C") = ARRAY['estado']
    FROM information_schema.column_privileges
   WHERE grantee = 'sc_mercado' AND table_schema = 'mercado' AND table_name = 'alertas_precio'
     AND privilege_type = 'UPDATE')
  AND NOT has_table_privilege('sc_mercado', 'mercado.alertas_precio', 'DELETE')
  AND has_table_privilege('sc_mercado', 'mercado.watchlist', 'DELETE')
  AND NOT has_column_privilege('sc_mercado', 'mercado.watchlist', 'creado_en', 'INSERT')
  AND NOT has_table_privilege('sc_mercado', 'mercado.watchlist', 'UPDATE'));
SELECT pg_temp.exigir('el cliente solo lee; ningún otro rol de la aplicación toca las dos tablas', (
  SELECT bool_and(privilege_type = 'SELECT') FROM information_schema.role_table_grants
   WHERE grantee = 'sc_cliente' AND table_schema = 'mercado' AND table_name IN ('watchlist', 'alertas_precio'))
  AND (SELECT bool_and(NOT has_any_column_privilege(r, t, 'SELECT, INSERT, UPDATE, REFERENCES')
                       AND NOT has_table_privilege(r, t, 'DELETE'))
         FROM unnest(ARRAY['sc_ifpe', 'sc_sofom', 'sc_puente', 'sc_spei', 'sc_aml', 'sc_auditor', 'sc_auditor_pld',
                           'sc_kyc', 'sc_kyc_verificador', 'sc_auth']) AS r,
              unnest(ARRAY['mercado.watchlist', 'mercado.alertas_precio']) AS t));
SELECT pg_temp.exigir('RLS en las dos tablas, con la política RESTRICTIVE de sc_cliente', (
  SELECT count(*) = 2 FROM pg_class
   WHERE relrowsecurity AND oid IN ('mercado.watchlist'::regclass, 'mercado.alertas_precio'::regclass))
  AND (SELECT count(*) = 2 FROM pg_policies
        WHERE schemaname = 'mercado' AND tablename IN ('watchlist', 'alertas_precio')
          AND policyname = 'solo_del_cliente' AND permissive = 'RESTRICTIVE' AND roles = '{sc_cliente}'));
SELECT pg_temp.exigir('cada cliente ve solo lo suyo; sin cliente, nada', (
  pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM mercado.watchlist') = 1
  AND pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM mercado.alertas_precio') = 2
  AND pg_temp.visibles('b0000000-0000-0000-0000-00000000000b', 'SELECT count(*) FROM mercado.watchlist') = 50
  AND pg_temp.visibles('b0000000-0000-0000-0000-00000000000b', 'SELECT count(*) FROM mercado.alertas_precio') = 51
  AND pg_temp.visibles('c0000000-0000-0000-0000-00000000000c', 'SELECT count(*) FROM mercado.alertas_precio') = 1
  AND pg_temp.visibles('', 'SELECT count(*) FROM mercado.watchlist') = 0
  AND pg_temp.visibles('', 'SELECT count(*) FROM mercado.alertas_precio') = 0));

\o
\echo 'tests/db/0024_seguimiento_alertas.sql: todas las pruebas pasaron (ROLLBACK)'
ROLLBACK;
