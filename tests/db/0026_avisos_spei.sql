-- =============================================================================
--  Pruebas de la migración 0026 (la bóveda avisa de cada paso de un SPEI).
--  Corren en una transacción que termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0026_avisos_spei.sql
--
--  Lo que la base de datos garantiza: cada cambio de estado de una orden SPEI
--  y cada fecha de operación nueva suben la versión de la bóveda de su cuenta
--  (y de ninguna otra), también cuando los hace sc_spei, que no actualiza
--  cuentas; reprogramar una orden sin cambiar nada no avisa; un SPEI devuelto
--  al emisor avisa a la cuenta que no pudo recibirlo, y uno abonado no avisa
--  por este camino (ya avisa su saldo).
-- =============================================================================
\set ON_ERROR_STOP on
\set QUIET on
\o /dev/null
SET client_min_messages = notice;

BEGIN;

-- Sin bloque EXCEPTION: las sentencias corren en la transacción principal.
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

CREATE FUNCTION pg_temp.version(cuenta UUID) RETURNS BIGINT
LANGUAGE sql AS $$ SELECT version_ordenes FROM ifpe.cuentas WHERE id = cuenta $$;

-- Una sentencia de backend/src/spei/outbound.rs sobre la orden, como sc_spei.
CREATE FUNCTION pg_temp.como_spei(sentencia TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$ SELECT ARRAY['SET LOCAL ROLE sc_spei', sentencia, 'RESET ROLE'] $$;

-- Un SPEI recibido y devuelto al emisor, como lo registra backend/src/spei/inbound.rs.
CREATE FUNCTION pg_temp.devuelto(id TEXT, rastreo TEXT, clabe TEXT, cuenta TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[format(
      $q$INSERT INTO ifpe.spei_inbound (id, institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id,
                                        monto, estado, causa_devolucion, motivo, origen, raw_payload)
         VALUES (%L, '40012', %L, current_date, %L, %L, 250, 'DEVUELTO', 1, 'cuenta inexistente', 'WEBHOOK', '{}')$q$,
      id, rastreo, clabe, cuenta)]
$$;

-- -----------------------------------------------------------------------------
-- Datos: Ana y Beto verificados; Ana recibe 5 000 por SPEI y retira 1 000 a su
-- BBVA vinculada (la orden queda PENDIENTE, como la deja el retiro).
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ana',  'Prueba', 'ana@example.mx',  'VERIFICADO', now(), 'BAJO'),
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Beto', 'Prueba', 'beto@example.mx', 'VERIFICADO', now(), 'BAJO');
INSERT INTO ifpe.reserva_custodia (id, institucion, tipo, numero_cuenta) VALUES
  ('e0000000-0000-0000-0000-00000000000e', 'Banco de México', 'BANXICO', 'SC-RES-001');
INSERT INTO ifpe.cuentas (id, cliente_id, clabe) VALUES
  ('1a000000-0000-0000-0000-00000000001a', 'a0000000-0000-0000-0000-00000000000a', '646180157000000017'),
  ('1b000000-0000-0000-0000-00000000001b', 'b0000000-0000-0000-0000-00000000000b', '646180157000000020');

CREATE TEMP TABLE versiones AS
SELECT pg_temp.version('1a000000-0000-0000-0000-00000000001a') AS ana,
       pg_temp.version('1b000000-0000-0000-0000-00000000001b') AS beto;

-- Como el dueño del esquema: sc_spei no elige el id del registro, y la prueba lo necesita.
SELECT pg_temp.debe_pasar('un depósito SPEI abonado: 5 000 para Ana', ARRAY[
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 5000 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 5000 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
     VALUES ('7d000000-0000-0000-0000-000000000001', 'DEPOSITO', '1a000000-0000-0000-0000-00000000001a',
             'e0000000-0000-0000-0000-00000000000e', 5000, 'spei:40012:20261120:DEP1', '40012:20261120:DEP1')$q$,
  $q$INSERT INTO ifpe.spei_inbound (id, institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                    estado, transaccion_id, origen, raw_payload)
     VALUES ('d1000000-0000-0000-0000-000000000001', '40012', 'DEP1', '2026-11-20', '646180157000000017',
             '1a000000-0000-0000-0000-00000000001a', 5000, 'ABONADO', '7d000000-0000-0000-0000-000000000001',
             'WEBHOOK', '{}')$q$
]);
SELECT pg_temp.exigir('un SPEI abonado no avisa por este camino: ya avisa su saldo',
  pg_temp.version('1a000000-0000-0000-0000-00000000001a') = (SELECT ana FROM versiones));

SELECT pg_temp.debe_pasar('Ana vincula su BBVA y retira 1 000 a ella: la orden queda PENDIENTE', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$INSERT INTO ifpe.cuentas_vinculadas (id, cliente_id, clabe, banco, titular, origen)
     VALUES ('ca000000-0000-0000-0000-0000000000a1', 'a0000000-0000-0000-0000-00000000000a', '012180001234567899', '012',
             'Ana Prueba', 'DEMO')$q$,
  $q$SELECT ifpe.aml_evaluar('a0000000-0000-0000-0000-00000000000a', 'SALIDA', 'SPEI_ENVIADO', 1000,
                             '7a000000-0000-0000-0000-000000000001', 'cli:0a000000-0000-0000-0000-000000000001',
                             jsonb_build_object('cuenta', '012180001234567899'))$q$,
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 1000 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo - 1000 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia, referencia_externa)
     VALUES ('7a000000-0000-0000-0000-000000000001', 'RETIRO', '1a000000-0000-0000-0000-00000000001a',
             'e0000000-0000-0000-0000-00000000000e', 1000, 'cli:0a000000-0000-0000-0000-000000000001', '012180001234567899')$q$,
  $q$INSERT INTO ifpe.spei_outbound (id, cliente_id, cuenta_id, clave_idempotencia, clave_rastreo, fecha_solicitud,
                                     ordenante_nombre, ordenante_clabe, banco_receptor, beneficiario_clabe,
                                     beneficiario_nombre, monto, concepto, referencia_numerica, tx_retiro_id,
                                     cuenta_vinculada_id)
     VALUES ('0a000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a',
             '1a000000-0000-0000-0000-00000000001a', 'cli:0a000000-0000-0000-0000-000000000001', 'SC26AVISO000001',
             current_date, 'Ana Prueba', '646180157000000017', '012', '012180001234567899', 'Ana Prueba', 1000,
             'RETIRO A CUENTA PROPIA', 1, '7a000000-0000-0000-0000-000000000001',
             'ca000000-0000-0000-0000-0000000000a1')$q$,
  'RESET ROLE'
]);
UPDATE versiones SET ana = pg_temp.version('1a000000-0000-0000-0000-00000000001a');

-- -----------------------------------------------------------------------------
-- Estructura
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('los avisos: después de cambiar el estado o la fecha de una orden, y de registrar un SPEI devuelto', (
  SELECT count(*) = 2 FROM pg_trigger t
   WHERE NOT t.tgisinternal
     AND (t.tgrelid, t.tgname) IN (('ifpe.spei_outbound'::regclass, 'trg_spei_outbound_avisar'),
                                   ('ifpe.spei_inbound'::regclass, 'trg_spei_inbound_avisar'))
     AND t.tgfoid = 'ifpe.fn_avisar_boveda_spei()'::regprocedure));
SELECT pg_temp.exigir('el aviso es SECURITY DEFINER, con search_path fijo y sin EXECUTE para PUBLIC',
  (SELECT prosecdef AND proconfig @> ARRAY['search_path=pg_catalog, pg_temp']
     FROM pg_proc WHERE oid = 'ifpe.fn_avisar_boveda_spei()'::regprocedure)
  AND NOT has_function_privilege('public', 'ifpe.fn_avisar_boveda_spei()', 'EXECUTE'));
SELECT pg_temp.exigir('sc_spei cambia órdenes, pero no actualiza la versión de las cuentas',
  has_column_privilege('sc_spei', 'ifpe.spei_outbound', 'estado', 'UPDATE')
  AND NOT has_column_privilege('sc_spei', 'ifpe.cuentas', 'version_ordenes', 'UPDATE'));

-- -----------------------------------------------------------------------------
-- Una orden de Ana: despachada, con fecha de operación, liquidada
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('el despachador la reprograma sin fecha: nada cambia',
  pg_temp.como_spei($q$UPDATE ifpe.spei_outbound SET fecha_operacion = coalesce(fecha_operacion, NULL),
                          proximo_intento_en = now() + interval '5 seconds'
                     WHERE id = '0a000000-0000-0000-0000-000000000001'$q$));
SELECT pg_temp.exigir('reprogramar sin cambiar nada no avisa',
  pg_temp.version('1a000000-0000-0000-0000-00000000001a') = (SELECT ana FROM versiones));

SELECT pg_temp.debe_pasar('el liquidador (STP) la acepta: ENVIADA',
  pg_temp.como_spei($q$UPDATE ifpe.spei_outbound SET estado = 'ENVIADA', enviada_en = coalesce(enviada_en, now()),
                          id_proveedor = coalesce(id_proveedor, '9001'), ultimo_error = NULL,
                          proximo_intento_en = now() + interval '30 seconds'
                     WHERE id = '0a000000-0000-0000-0000-000000000001'$q$));
SELECT pg_temp.exigir('ENVIADA sube la versión de la bóveda de Ana, una vez, aunque lo haga sc_spei',
  pg_temp.version('1a000000-0000-0000-0000-00000000001a') = (SELECT ana FROM versiones) + 1);

SELECT pg_temp.debe_pasar('Banxico le asigna su fecha de operación',
  pg_temp.como_spei($q$UPDATE ifpe.spei_outbound SET fecha_operacion = coalesce(fecha_operacion, current_date),
                          proximo_intento_en = now() + interval '30 seconds'
                     WHERE id = '0a000000-0000-0000-0000-000000000001'$q$));
SELECT pg_temp.exigir('la fecha de operación nueva avisa (con ella se consulta el CEP)',
  pg_temp.version('1a000000-0000-0000-0000-00000000001a') = (SELECT ana FROM versiones) + 2);

SELECT pg_temp.debe_pasar('otra consulta trae la misma fecha: nada cambia',
  pg_temp.como_spei($q$UPDATE ifpe.spei_outbound SET fecha_operacion = coalesce(fecha_operacion, current_date + 1),
                          proximo_intento_en = now() + interval '30 seconds'
                     WHERE id = '0a000000-0000-0000-0000-000000000001'$q$));
SELECT pg_temp.exigir('la misma fecha no vuelve a avisar',
  pg_temp.version('1a000000-0000-0000-0000-00000000001a') = (SELECT ana FROM versiones) + 2);

SELECT pg_temp.debe_pasar('se cuentan los intentos (otra columna): no avisa',
  pg_temp.como_spei($q$UPDATE ifpe.spei_outbound SET intentos_envio = intentos_envio + 1
                     WHERE id = '0a000000-0000-0000-0000-000000000001'$q$));
SELECT pg_temp.exigir('las columnas internas de la orden no avisan',
  pg_temp.version('1a000000-0000-0000-0000-00000000001a') = (SELECT ana FROM versiones) + 2);

SELECT pg_temp.debe_pasar('el banco receptor la recibe: LIQUIDADA',
  pg_temp.como_spei($q$UPDATE ifpe.spei_outbound SET estado = 'LIQUIDADA', liquidada_en = now(), ultimo_error = NULL,
                          revision_manual = FALSE, fecha_operacion = coalesce(fecha_operacion, NULL),
                          proximo_intento_en = now() + interval '1 hour'
                     WHERE id = '0a000000-0000-0000-0000-000000000001'$q$));
SELECT pg_temp.exigir('LIQUIDADA avisa a la bóveda de Ana',
  pg_temp.version('1a000000-0000-0000-0000-00000000001a') = (SELECT ana FROM versiones) + 3);
SELECT pg_temp.exigir('la bóveda de Beto no se entera de los SPEI de Ana',
  pg_temp.version('1b000000-0000-0000-0000-00000000001b') = (SELECT beto FROM versiones));

-- -----------------------------------------------------------------------------
-- SPEI devueltos al emisor
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('un SPEI a la CLABE de Beto que su cuenta no pudo recibir vuelve al emisor',
  pg_temp.devuelto('d2000000-0000-0000-0000-000000000001', 'DEVB1', '646180157000000020',
                   '1b000000-0000-0000-0000-00000000001b'));
SELECT pg_temp.exigir('el SPEI devuelto avisa a la bóveda de Beto (el saldo no cambió), y a ninguna otra',
  pg_temp.version('1b000000-0000-0000-0000-00000000001b') = (SELECT beto FROM versiones) + 1
  AND pg_temp.version('1a000000-0000-0000-0000-00000000001a') = (SELECT ana FROM versiones) + 3);

UPDATE versiones SET ana = pg_temp.version('1a000000-0000-0000-0000-00000000001a'),
                     beto = pg_temp.version('1b000000-0000-0000-0000-00000000001b');
SELECT pg_temp.debe_pasar('un SPEI a una CLABE que no es de nadie se devuelve sin cuenta',
  pg_temp.devuelto('d2000000-0000-0000-0000-000000000002', 'DEVX1', '646180157000000099', NULL));
SELECT pg_temp.exigir('sin cuenta no avisa a ninguna bóveda',
  pg_temp.version('1a000000-0000-0000-0000-00000000001a') = (SELECT ana FROM versiones)
  AND pg_temp.version('1b000000-0000-0000-0000-00000000001b') = (SELECT beto FROM versiones));


\o
\echo tests/db/0026_avisos_spei.sql: todas las pruebas pasaron (ROLLBACK)
ROLLBACK;
