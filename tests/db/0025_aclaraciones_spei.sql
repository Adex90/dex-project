-- =============================================================================
--  Pruebas de la migración 0025 (aclaraciones SPEI).
--  Corren en una transacción que termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0025_aclaraciones_spei.sql
--
--  Lo que la base de datos garantiza por sí sola: una aclaración nace ABIERTA,
--  de un cliente verificado, sobre un SPEI suyo de a lo más 90 días, con el
--  folio siguiente y la clave de rastreo del movimiento; una activa por
--  movimiento y a lo más 10 por cliente; solo cambia su estado, por la máquina
--  de estados, y al cerrarse lleva una nota que no nombra la prevención de
--  lavado de dinero; nunca se borra; cada paso queda en la bitácora del cliente
--  y avisa a su bóveda; abre sc_ifpe, cambia sc_spei y el cliente lee solo lo
--  suyo. backend/tests/aclaraciones.rs cubre la API.
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

-- Sin bloque EXCEPTION: las sentencias corren en la transacción principal (el
-- cargo y la orden se revisan contra el mismo xmin).
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

CREATE FUNCTION pg_temp.version(cuenta UUID) RETURNS BIGINT
LANGUAGE sql AS $$ SELECT version_ordenes FROM ifpe.cuentas WHERE id = cuenta $$;

-- Abre una aclaración como sc_ifpe (como backend/src/patrimonio/aclaraciones.rs);
-- con sesión, también con la IP y el dispositivo de la petición.
CREATE FUNCTION pg_temp.abrir(id TEXT, cliente TEXT, salida TEXT, entrada TEXT, motivo TEXT, descripcion TEXT,
                              clave TEXT, sesion TEXT DEFAULT NULL) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
      'SET LOCAL ROLE sc_ifpe',
      format($q$INSERT INTO ifpe.aclaraciones_spei (id, cliente_id, spei_outbound_id, spei_inbound_id, motivo, descripcion,
                                                     clave_idempotencia, sesion_id, ip, dispositivo)
                VALUES (%L, %L, %L, %L, %L, %L, %L, %L, %L, %L)$q$,
             id, cliente, salida, entrada, motivo, descripcion, clave, sesion,
             CASE WHEN sesion IS NULL THEN NULL ELSE '203.0.113.7' END,
             CASE WHEN sesion IS NULL THEN NULL ELSE 'Chrome on macOS' END),
      'RESET ROLE']
$$;

-- Cambia una aclaración como sc_spei (el operador SPEI).
CREATE FUNCTION pg_temp.cambiar(id TEXT, asignacion TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY['SET LOCAL ROLE sc_spei',
                 format($q$UPDATE ifpe.aclaraciones_spei SET %s WHERE id = %L$q$, asignacion, id),
                 'RESET ROLE']
$$;

-- Un SPEI devuelto al emisor (nunca abonado) a la cuenta dada, recibido hace
-- `dias` días, como lo registra backend/src/spei/inbound.rs.
CREATE FUNCTION pg_temp.devuelto(id TEXT, rastreo TEXT, cuenta TEXT, clabe TEXT, dias INTEGER) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[format(
      $q$INSERT INTO ifpe.spei_inbound (id, institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id,
                                        monto, estado, causa_devolucion, motivo, origen, raw_payload, recibido_en)
         VALUES (%L, '40012', %L, (now() - make_interval(days => %s))::date, %L, %L, 250, 'DEVUELTO', 2,
                 'cuenta bloqueada', 'WEBHOOK', '{}', now() - make_interval(days => %s))$q$,
      id, rastreo, dias, clabe, cuenta, dias)]
$$;

-- Vincula una cuenta como sc_ifpe (como backend/src/patrimonio).
CREATE FUNCTION pg_temp.vincular(id TEXT, cliente TEXT, clabe TEXT, titular TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
      'SET LOCAL ROLE sc_ifpe',
      format($q$INSERT INTO ifpe.cuentas_vinculadas (id, cliente_id, clabe, banco, titular, origen)
                VALUES (%L, %L, %L, %L, %L, 'DEMO')$q$, id, cliente, clabe, substr(clabe, 1, 3), titular),
      'RESET ROLE']
$$;

-- Un retiro a cuenta propia como lo escribe backend/src/spei/outbound.rs:
-- evaluación PLD, cargo (RETIRO) al disponible y a la reserva, y la orden
-- PENDIENTE ligada a su cuenta vinculada.
CREATE FUNCTION pg_temp.retiro(orden TEXT, tx TEXT, cliente TEXT, cuenta TEXT, ordenante_clabe TEXT,
                               vinculada TEXT, clabe TEXT, beneficiario TEXT, monto TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
      'SET LOCAL ROLE sc_ifpe',
      format($q$SELECT ifpe.aml_evaluar(%L, 'SALIDA', 'SPEI_ENVIADO', %s, %L, %L, jsonb_build_object('cuenta', %L))$q$,
             cliente, monto, tx, 'cli:' || orden, clabe),
      format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - %s WHERE id = %L$q$, monto, cuenta),
      format($q$UPDATE ifpe.reserva_custodia SET saldo = saldo - %s WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
             monto),
      format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia,
                                               referencia_externa)
                VALUES (%L, 'RETIRO', %L, 'e0000000-0000-0000-0000-00000000000e', %s, %L, %L)$q$,
             tx, cuenta, monto, 'cli:' || orden, clabe),
      format($q$INSERT INTO ifpe.spei_outbound (id, cliente_id, cuenta_id, clave_idempotencia, clave_rastreo,
                    fecha_solicitud, ordenante_nombre, ordenante_clabe, banco_receptor, beneficiario_clabe,
                    beneficiario_nombre, monto, concepto, referencia_numerica, tx_retiro_id, cuenta_vinculada_id)
                VALUES (%L, %L, %L, %L, %L, current_date, %L, %L, %L, %L, %L, %s, 'RETIRO A CUENTA PROPIA', 1, %L, %L)$q$,
             orden, cliente, cuenta, 'cli:' || orden, 'SC25' || upper(right(replace(orden, '-', ''), 12)),
             beneficiario, ordenante_clabe, substr(clabe, 1, 3), clabe, beneficiario, monto, tx, vinculada),
      'RESET ROLE']
$$;

-- -----------------------------------------------------------------------------
-- Datos: Ana y Beto verificados, con 5 000 recibidos por SPEI cada uno y una
-- sesión; Carla sin KYC. Ana retira 1 000 a su BBVA vinculada.
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ana',  'Prueba', 'ana@example.mx',  'VERIFICADO', now(), 'BAJO'),
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Beto', 'Prueba', 'beto@example.mx', 'VERIFICADO', now(), 'BAJO');
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, nivel_riesgo_pld) VALUES
  ('c0000000-0000-0000-0000-00000000000c', 'FISICA', 'Carla', 'Prueba', 'carla@example.mx', 'BAJO');
INSERT INTO ifpe.reserva_custodia (id, institucion, tipo, numero_cuenta) VALUES
  ('e0000000-0000-0000-0000-00000000000e', 'Banco de México', 'BANXICO', 'SC-RES-001');
INSERT INTO ifpe.cuentas (id, cliente_id, clabe) VALUES
  ('1a000000-0000-0000-0000-00000000001a', 'a0000000-0000-0000-0000-00000000000a', '646180157000000017'),
  ('1b000000-0000-0000-0000-00000000001b', 'b0000000-0000-0000-0000-00000000000b', '646180157000000020'),
  ('1c000000-0000-0000-0000-00000000001c', 'c0000000-0000-0000-0000-00000000000c', '646180157000000033');
INSERT INTO kyc.sesiones (id, cliente_id, expira_en, mfa, dispositivo, ip) VALUES
  ('5e000000-0000-0000-0000-00000000000a', 'a0000000-0000-0000-0000-00000000000a', now() + interval '60 minutes', FALSE,
   'Chrome on macOS', '203.0.113.7'),
  ('5e000000-0000-0000-0000-00000000000b', 'b0000000-0000-0000-0000-00000000000b', now() + interval '60 minutes', FALSE,
   'Safari on iOS', '198.51.100.4');

-- Como el dueño del esquema: sc_spei no elige el id del registro, y las
-- pruebas lo necesitan.
SELECT pg_temp.debe_pasar('depósitos SPEI: 5 000 para Ana y para Beto', ARRAY[
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 5000
      WHERE id IN ('1a000000-0000-0000-0000-00000000001a', '1b000000-0000-0000-0000-00000000001b')$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 10000 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
     SELECT ('7d000000-0000-0000-0000-00000000000' || n)::uuid, 'DEPOSITO', cuenta::uuid,
            'e0000000-0000-0000-0000-00000000000e', 5000, 'spei:40012:20261120:DEP' || n, '40012:20261120:DEP' || n
       FROM (VALUES (1, '1a000000-0000-0000-0000-00000000001a'), (2, '1b000000-0000-0000-0000-00000000001b')) d (n, cuenta)$q$,
  $q$INSERT INTO ifpe.spei_inbound (id, institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                    estado, transaccion_id, origen, raw_payload)
     SELECT ('d1000000-0000-0000-0000-00000000000' || n)::uuid, '40012', 'DEP' || n, '2026-11-20', c.clabe, c.id, 5000,
            'ABONADO', ('7d000000-0000-0000-0000-00000000000' || n)::uuid, 'WEBHOOK', '{}'
       FROM (VALUES (1, '1a000000-0000-0000-0000-00000000001a'), (2, '1b000000-0000-0000-0000-00000000001b')) d (n, cuenta)
       JOIN ifpe.cuentas c ON c.id = d.cuenta::uuid$q$
]);

SELECT pg_temp.debe_pasar('Ana vincula su BBVA y retira 1 000 a ella: la orden queda PENDIENTE', (
  pg_temp.vincular('ca000000-0000-0000-0000-0000000000a1', 'a0000000-0000-0000-0000-00000000000a', '012180001234567899',
                   'Ana Prueba'))
  || pg_temp.retiro('0a000000-0000-0000-0000-000000000001', '7a000000-0000-0000-0000-000000000001',
                    'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a', '646180157000000017',
                    'ca000000-0000-0000-0000-0000000000a1', '012180001234567899', 'Ana Prueba', '1000'));

-- Devoluciones al emisor: una reciente a Ana, una de hace 100 días, una de
-- hace 89 días, y una a Carla (sin KYC, su CLABE devuelve lo que recibe).
SELECT pg_temp.debe_pasar('SPEI devueltos al emisor: a Ana (hoy, hace 89 y hace 100 días) y a Carla', (
  pg_temp.devuelto('d2000000-0000-0000-0000-000000000001', 'DEVA1', '1a000000-0000-0000-0000-00000000001a',
                   '646180157000000017', 0))
  || pg_temp.devuelto('d2000000-0000-0000-0000-000000000002', 'DEVA2', '1a000000-0000-0000-0000-00000000001a',
                      '646180157000000017', 100)
  || pg_temp.devuelto('d2000000-0000-0000-0000-000000000003', 'DEVA3', '1a000000-0000-0000-0000-00000000001a',
                      '646180157000000017', 89)
  || pg_temp.devuelto('d2000000-0000-0000-0000-000000000004', 'DEVC1', '1c000000-0000-0000-0000-00000000001c',
                      '646180157000000033', 0));

-- -----------------------------------------------------------------------------
-- Estructura, privilegios y RLS
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('RLS en las aclaraciones, con la política RESTRICTIVE de sc_cliente (0011)', (
  SELECT relrowsecurity FROM pg_class WHERE oid = 'ifpe.aclaraciones_spei'::regclass)
  AND (SELECT count(*) = 1 FROM pg_policies
        WHERE schemaname = 'ifpe' AND tablename = 'aclaraciones_spei' AND policyname = 'solo_del_cliente'
          AND permissive = 'RESTRICTIVE' AND cmd = 'ALL' AND roles = '{sc_cliente}'));

SELECT pg_temp.exigir('sc_ifpe abre con los términos (sin folio, estado, clave de rastreo, nota ni fechas) y no cambia nada',
  has_table_privilege('sc_ifpe', 'ifpe.aclaraciones_spei', 'SELECT')
  AND has_column_privilege('sc_ifpe', 'ifpe.aclaraciones_spei', 'motivo', 'INSERT')
  AND has_column_privilege('sc_ifpe', 'ifpe.aclaraciones_spei', 'sesion_id', 'INSERT')
  AND NOT has_column_privilege('sc_ifpe', 'ifpe.aclaraciones_spei', 'numero', 'INSERT')
  AND NOT has_column_privilege('sc_ifpe', 'ifpe.aclaraciones_spei', 'estado', 'INSERT')
  AND NOT has_column_privilege('sc_ifpe', 'ifpe.aclaraciones_spei', 'clave_rastreo', 'INSERT')
  AND NOT has_column_privilege('sc_ifpe', 'ifpe.aclaraciones_spei', 'nota_resolucion', 'INSERT')
  AND NOT has_column_privilege('sc_ifpe', 'ifpe.aclaraciones_spei', 'creada_en', 'INSERT')
  AND NOT has_column_privilege('sc_ifpe', 'ifpe.aclaraciones_spei', 'estado', 'UPDATE')
  AND NOT has_table_privilege('sc_ifpe', 'ifpe.aclaraciones_spei', 'DELETE'));

SELECT pg_temp.exigir('sc_spei cambia solo el estado y la nota; no abre ni borra',
  has_column_privilege('sc_spei', 'ifpe.aclaraciones_spei', 'estado', 'UPDATE')
  AND has_column_privilege('sc_spei', 'ifpe.aclaraciones_spei', 'nota_resolucion', 'UPDATE')
  AND NOT has_column_privilege('sc_spei', 'ifpe.aclaraciones_spei', 'motivo', 'UPDATE')
  AND NOT has_column_privilege('sc_spei', 'ifpe.aclaraciones_spei', 'cerrada_en', 'UPDATE')
  AND NOT has_table_privilege('sc_spei', 'ifpe.aclaraciones_spei', 'INSERT')
  AND NOT has_table_privilege('sc_spei', 'ifpe.aclaraciones_spei', 'DELETE'));

SELECT pg_temp.exigir('el cliente y el auditor solo leen; ningún otro rol de la aplicación toca la tabla', (
  SELECT array_agg(g ORDER BY g COLLATE "C") = ARRAY['sc_auditor:SELECT', 'sc_cliente:SELECT', 'sc_ifpe:SELECT', 'sc_spei:SELECT']
    FROM (SELECT DISTINCT grantee || ':' || privilege_type AS g
            FROM information_schema.role_table_grants
           WHERE table_schema = 'ifpe' AND table_name = 'aclaraciones_spei' AND grantee LIKE 'sc\_%') x));

SELECT pg_temp.exigir('la nota neutral: los hechos del pago pasan; la prevención de lavado, en cualquier idioma, no',
  ifpe.nota_neutral('Tu pago se liquidó el 24 de noviembre; el CEP de Banxico lo confirma.')
  AND ifpe.nota_neutral('The receiving bank returned it: the account is closed.')
  AND ifpe.nota_neutral('Plática con el banco receptor: el abono llegó.')
  AND NOT ifpe.nota_neutral('Revisión por PLD.')
  AND NOT ifpe.nota_neutral('Hubo una alerta en el sistema.')
  AND NOT ifpe.nota_neutral('Your account is frozen.')
  AND NOT ifpe.nota_neutral('Operación con sospecha.')
  AND NOT ifpe.nota_neutral('Lo revisó el área de cumplimiento.')
  AND NOT ifpe.nota_neutral('Saldo retenido de forma preventiva.')
  AND NOT ifpe.nota_neutral('Compliance alerts reviewed.')
  AND NOT ifpe.nota_neutral('Nivel de riesgo alto.'));

-- -----------------------------------------------------------------------------
-- Abrir una aclaración
-- -----------------------------------------------------------------------------
CREATE TEMP TABLE antes AS SELECT pg_temp.version('1a000000-0000-0000-0000-00000000001a') AS v;

SELECT pg_temp.debe_pasar('Ana aclara su retiro que no llega, desde su sesión', pg_temp.abrir(
  'ac000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a', '0a000000-0000-0000-0000-000000000001',
  NULL, 'RETRASO', 'Lo mandé ayer y mi banco no lo ve.', 'cli:a:aclaracion-1', '5e000000-0000-0000-0000-00000000000a'));

SELECT pg_temp.exigir('nace ABIERTA con el folio 1, la clave de rastreo de la orden y la hora de la base de datos', (
  SELECT numero = 1 AND estado = 'ABIERTA' AND nota_resolucion IS NULL AND cerrada_en IS NULL
         AND clave_rastreo = (SELECT clave_rastreo FROM ifpe.spei_outbound WHERE id = '0a000000-0000-0000-0000-000000000001')
         AND creada_en = now() AND actualizada_en = now()
    FROM ifpe.aclaraciones_spei WHERE id = 'ac000000-0000-0000-0000-000000000001'));

SELECT pg_temp.exigir('la bitácora de Ana lo registra con su sesión, IP y dispositivo, escrito por sc_ifpe; la bóveda avisa', (
  SELECT count(*) = 1 FROM kyc.eventos_autenticacion
   WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND evento = 'ACLARACION_SPEI_ABIERTA' AND detalle = '1'
     AND sesion_id = '5e000000-0000-0000-0000-00000000000a' AND ip = '203.0.113.7' AND dispositivo = 'Chrome on macOS'
     AND registrado_por = 'sc_ifpe')
  AND pg_temp.version('1a000000-0000-0000-0000-00000000001a') = (SELECT v + 1 FROM antes));

SELECT pg_temp.debe_fallar('otra aclaración activa del mismo movimiento, no', pg_temp.abrir(
  'ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a', '0a000000-0000-0000-0000-000000000001',
  NULL, 'NO_RECIBIDO', NULL, 'cli:a:aclaracion-x'), 'SC_ACLARACION_ACTIVA');

SELECT pg_temp.debe_fallar('el depósito de Beto no es de Ana', pg_temp.abrir(
  'ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a', NULL,
  'd1000000-0000-0000-0000-000000000002', 'NO_RECONOCIDO', NULL, 'cli:a:aclaracion-x'), 'SC_ACLARACION_AJENA');

SELECT pg_temp.debe_fallar('ni un movimiento que no existe', pg_temp.abrir(
  'ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a', NULL,
  'd1000000-0000-0000-0000-0000000000ff', 'NO_RECONOCIDO', NULL, 'cli:a:aclaracion-x'), 'SC_ACLARACION_AJENA');

SELECT pg_temp.debe_fallar('ni con la sesión de Beto', pg_temp.abrir(
  'ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a', NULL,
  'd1000000-0000-0000-0000-000000000001', 'NO_RECONOCIDO', NULL, 'cli:a:aclaracion-x',
  '5e000000-0000-0000-0000-00000000000b'), 'SC_ACLARACION_INCOHERENTE');

SELECT pg_temp.debe_fallar('un movimiento, ni dos ni ninguno', pg_temp.abrir(
  'ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a', '0a000000-0000-0000-0000-000000000001',
  'd1000000-0000-0000-0000-000000000001', 'OTRO', 'Dos movimientos a la vez.', 'cli:a:aclaracion-x'),
  'SC_ACLARACION_INCOHERENTE');
SELECT pg_temp.debe_fallar('(ninguno)', pg_temp.abrir(
  'ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a', NULL, NULL, 'OTRO',
  'Ningún movimiento.', 'cli:a:aclaracion-x'), 'SC_ACLARACION_INCOHERENTE');

SELECT pg_temp.debe_fallar('Carla, sin KYC, no abre aclaraciones', pg_temp.abrir(
  'ac000000-0000-0000-0000-000000000099', 'c0000000-0000-0000-0000-00000000000c', NULL,
  'd2000000-0000-0000-0000-000000000004', 'DEVOLUCION', NULL, 'cli:c:aclaracion-1'), 'SC_KYC_NO_VERIFICADO');

SELECT pg_temp.debe_fallar('nace ABIERTA (ni el dueño del esquema la crea en revisión)', ARRAY[
  $q$INSERT INTO ifpe.aclaraciones_spei (id, cliente_id, spei_inbound_id, motivo, clave_idempotencia, estado)
     VALUES ('ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a',
             'd1000000-0000-0000-0000-000000000001', 'NO_RECONOCIDO', 'dba:1', 'EN_REVISION')$q$],
  'SC_ACLARACION_INCOHERENTE');
SELECT pg_temp.debe_fallar('ni con nota de resolución', ARRAY[
  $q$INSERT INTO ifpe.aclaraciones_spei (id, cliente_id, spei_inbound_id, motivo, clave_idempotencia, nota_resolucion)
     VALUES ('ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a',
             'd1000000-0000-0000-0000-000000000001', 'NO_RECONOCIDO', 'dba:2', 'Resuelta de antemano.')$q$],
  'SC_ACLARACION_INCOHERENTE');

SELECT pg_temp.debe_fallar('sc_ifpe no elige el folio', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$INSERT INTO ifpe.aclaraciones_spei (id, cliente_id, spei_inbound_id, motivo, clave_idempotencia, numero)
     VALUES ('ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a',
             'd1000000-0000-0000-0000-000000000001', 'NO_RECONOCIDO', 'cli:a:x', 7)$q$],
  'permission denied');
SELECT pg_temp.debe_fallar('ni el estado', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$INSERT INTO ifpe.aclaraciones_spei (id, cliente_id, spei_inbound_id, motivo, clave_idempotencia, estado)
     VALUES ('ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a',
             'd1000000-0000-0000-0000-000000000001', 'NO_RECONOCIDO', 'cli:a:x', 'RESUELTA')$q$],
  'permission denied');

SELECT pg_temp.debe_fallar('"otro" lleva una descripción de al menos 10 caracteres', pg_temp.abrir(
  'ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a', NULL,
  'd1000000-0000-0000-0000-000000000001', 'OTRO', NULL, 'cli:a:aclaracion-x'), 'ck_aclaracion_otro');
SELECT pg_temp.debe_fallar('(una corta, tampoco)', pg_temp.abrir(
  'ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a', NULL,
  'd1000000-0000-0000-0000-000000000001', 'OTRO', '   corto   ', 'cli:a:aclaracion-x'), 'ck_aclaracion_otro');
SELECT pg_temp.debe_fallar('a lo más 500 caracteres', pg_temp.abrir(
  'ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a', NULL,
  'd1000000-0000-0000-0000-000000000001', 'MONTO_INCORRECTO', repeat('x', 501), 'cli:a:aclaracion-x'),
  'ck_aclaracion_descripcion');
SELECT pg_temp.debe_fallar('un motivo del catálogo', pg_temp.abrir(
  'ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a', NULL,
  'd1000000-0000-0000-0000-000000000001', 'FRAUDE', NULL, 'cli:a:aclaracion-x'), 'ck_aclaracion_motivo');

SELECT pg_temp.debe_fallar('después de 90 días, no (LTOSF, art. 23)', pg_temp.abrir(
  'ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a', NULL,
  'd2000000-0000-0000-0000-000000000002', 'DEVOLUCION', NULL, 'cli:a:aclaracion-x'), 'SC_ACLARACION_VENCIDA');

SELECT pg_temp.debe_pasar('Ana aclara su depósito (no lo reconoce) y la devolución de hace 89 días', (
  pg_temp.abrir('ac000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a', NULL,
                'd1000000-0000-0000-0000-000000000001', 'NO_RECONOCIDO', NULL, 'cli:a:aclaracion-2'))
  || pg_temp.abrir('ac000000-0000-0000-0000-000000000003', 'a0000000-0000-0000-0000-00000000000a', NULL,
                   'd2000000-0000-0000-0000-000000000003', 'DEVOLUCION', 'Mi nómina regresó.', 'cli:a:aclaracion-3'));

SELECT pg_temp.exigir('folios 2 y 3, con la clave de rastreo de cada depósito; sin sesión, la bitácora sin ella', (
  SELECT array_agg(numero::text || ':' || clave_rastreo ORDER BY numero) = ARRAY['2:DEP1', '3:DEVA3']
    FROM ifpe.aclaraciones_spei WHERE id IN ('ac000000-0000-0000-0000-000000000002', 'ac000000-0000-0000-0000-000000000003'))
  AND (SELECT count(*) = 2 FROM kyc.eventos_autenticacion
        WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND evento = 'ACLARACION_SPEI_ABIERTA'
          AND detalle IN ('2', '3') AND sesion_id IS NULL AND ip IS NULL));

SELECT pg_temp.debe_fallar('una clave de idempotencia, una aclaración', pg_temp.abrir(
  'ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a', NULL,
  'd2000000-0000-0000-0000-000000000001', 'DEVOLUCION', NULL, 'cli:a:aclaracion-2'),
  'aclaraciones_spei_clave_idempotencia_key');

-- A lo más 10 activas: ocho devoluciones más; siete aclaraciones las llevan a 10.
SELECT pg_temp.debe_pasar('ocho devoluciones más a Ana, de hoy', (
  SELECT array_agg(u.s ORDER BY n, u.i)
    FROM generate_series(1, 8) AS n,
         unnest(pg_temp.devuelto(format('d3000000-0000-0000-0000-%s', lpad(n::text, 12, '0')), 'DEVM' || n,
                                 '1a000000-0000-0000-0000-00000000001a', '646180157000000017', 0))
           WITH ORDINALITY AS u (s, i)));
SELECT pg_temp.debe_pasar('Ana llega a 10 activas', (
  SELECT array_agg(u.s ORDER BY n, u.i)
    FROM generate_series(1, 7) AS n,
         unnest(pg_temp.abrir(format('ac000000-0000-0000-0000-%s', lpad((10 + n)::text, 12, '0')),
                              'a0000000-0000-0000-0000-00000000000a', NULL,
                              format('d3000000-0000-0000-0000-%s', lpad(n::text, 12, '0')), 'DEVOLUCION', NULL,
                              'cli:a:tope-' || n))
           WITH ORDINALITY AS u (s, i)));
SELECT pg_temp.exigir('folios consecutivos del 1 al 10, todas activas', (
  SELECT array_agg(numero ORDER BY numero) = ARRAY[1, 2, 3, 4, 5, 6, 7, 8, 9, 10] AND bool_and(estado = 'ABIERTA')
    FROM ifpe.aclaraciones_spei WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a'));
SELECT pg_temp.debe_fallar('la undécima activa, no', pg_temp.abrir(
  'ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a', NULL,
  'd3000000-0000-0000-0000-000000000008', 'DEVOLUCION', NULL, 'cli:a:tope-8'), 'SC_LIMITE_ACLARACIONES');

-- -----------------------------------------------------------------------------
-- El operador: revisar, resolver, rechazar
-- -----------------------------------------------------------------------------
DELETE FROM antes;
INSERT INTO antes SELECT pg_temp.version('1a000000-0000-0000-0000-00000000001a');

SELECT pg_temp.debe_pasar('el operador toma la 1 (sin nota)',
  pg_temp.cambiar('ac000000-0000-0000-0000-000000000001', $q$estado = 'EN_REVISION'$q$));
SELECT pg_temp.exigir('EN_REVISION, sin cerrar; la bitácora de Ana lo dice sin sesión, IP ni dispositivo; la bóveda avisa', (
  SELECT estado = 'EN_REVISION' AND cerrada_en IS NULL FROM ifpe.aclaraciones_spei
   WHERE id = 'ac000000-0000-0000-0000-000000000001')
  AND (SELECT count(*) = 1 FROM kyc.eventos_autenticacion
        WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND evento = 'ACLARACION_SPEI_EN_REVISION'
          AND detalle = '1' AND sesion_id IS NULL AND ip IS NULL AND dispositivo IS NULL AND registrado_por = 'sc_spei')
  AND pg_temp.version('1a000000-0000-0000-0000-00000000001a') = (SELECT v + 1 FROM antes));

SELECT pg_temp.debe_fallar('en revisión no vuelve a revisión', pg_temp.cambiar(
  'ac000000-0000-0000-0000-000000000001', $q$estado = 'EN_REVISION'$q$), 'SC_ACLARACION_TRANSICION');
SELECT pg_temp.debe_fallar('ni a abierta', pg_temp.cambiar(
  'ac000000-0000-0000-0000-000000000001', $q$estado = 'ABIERTA'$q$), 'SC_ACLARACION_TRANSICION');
SELECT pg_temp.debe_fallar('ni a un estado que no existe', pg_temp.cambiar(
  'ac000000-0000-0000-0000-000000000001', $q$estado = 'ARCHIVADA'$q$), 'SC_ACLARACION_TRANSICION');

SELECT pg_temp.debe_fallar('resolver lleva nota', pg_temp.cambiar(
  'ac000000-0000-0000-0000-000000000001', $q$estado = 'RESUELTA'$q$), 'SC_ACLARACION_NOTA');
SELECT pg_temp.debe_fallar('(de al menos 10 caracteres)', pg_temp.cambiar(
  'ac000000-0000-0000-0000-000000000001', $q$estado = 'RESUELTA', nota_resolucion = '  Listo.  '$q$),
  'SC_ACLARACION_NOTA');
SELECT pg_temp.debe_fallar('una nota que nombra la PLD no llega al cliente', pg_temp.cambiar(
  'ac000000-0000-0000-0000-000000000001', $q$estado = 'RESUELTA', nota_resolucion = 'Se retuvo por revisión de PLD.'$q$),
  'SC_ACLARACION_NOTA');
SELECT pg_temp.debe_fallar('ni una que habla de una alerta', pg_temp.cambiar(
  'ac000000-0000-0000-0000-000000000001', $q$estado = 'RECHAZADA', nota_resolucion = 'Hay una alerta en tu cuenta.'$q$),
  'SC_ACLARACION_NOTA');
SELECT pg_temp.debe_fallar('ni en inglés', pg_temp.cambiar(
  'ac000000-0000-0000-0000-000000000001', $q$estado = 'RECHAZADA', nota_resolucion = 'The account is frozen for now.'$q$),
  'SC_ACLARACION_NOTA');
SELECT pg_temp.debe_fallar('ni el dueño del esquema se la salta (la restricción también lo dice)', ARRAY[
  'ALTER TABLE ifpe.aclaraciones_spei DISABLE TRIGGER trg_aclaraciones_b_transicion',
  $q$UPDATE ifpe.aclaraciones_spei SET estado = 'RECHAZADA', cerrada_en = now(),
            nota_resolucion = 'Operación sospechosa, cuenta congelada.'
      WHERE id = 'ac000000-0000-0000-0000-000000000001'$q$],
  'ck_aclaracion_nota');

SELECT pg_temp.debe_pasar('resuelta con una nota neutral', pg_temp.cambiar(
  'ac000000-0000-0000-0000-000000000001',
  $q$estado = 'RESUELTA', nota_resolucion = '  El banco receptor confirmó el abono del 24 de noviembre.  '$q$));
SELECT pg_temp.exigir('cerrada ahora, con la nota sin espacios de más, y su evento en la bitácora', (
  SELECT estado = 'RESUELTA' AND cerrada_en = now()
         AND nota_resolucion = 'El banco receptor confirmó el abono del 24 de noviembre.'
    FROM ifpe.aclaraciones_spei WHERE id = 'ac000000-0000-0000-0000-000000000001')
  AND (SELECT array_agg(evento ORDER BY id) = ARRAY['ACLARACION_SPEI_ABIERTA', 'ACLARACION_SPEI_EN_REVISION',
                                                    'ACLARACION_SPEI_RESUELTA']
         FROM kyc.eventos_autenticacion
        WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND detalle = '1'
          AND evento LIKE 'ACLARACION_SPEI_%'));

SELECT pg_temp.debe_fallar('cerrada no cambia: ni vuelve a revisión', pg_temp.cambiar(
  'ac000000-0000-0000-0000-000000000001', $q$estado = 'EN_REVISION'$q$), 'SC_ACLARACION_CERRADA');
SELECT pg_temp.debe_fallar('ni cambia su nota', pg_temp.cambiar(
  'ac000000-0000-0000-0000-000000000001', $q$nota_resolucion = 'Otra nota para el cliente.'$q$),
  'SC_ACLARACION_CERRADA');

SELECT pg_temp.debe_pasar('una abierta se rechaza directamente, con nota',
  pg_temp.cambiar('ac000000-0000-0000-0000-000000000002',
                  $q$estado = 'RECHAZADA', nota_resolucion = 'El depósito es de tu nómina del 20 de noviembre.'$q$));
SELECT pg_temp.debe_pasar('una en revisión puede llevar nota antes de cerrarse',
  pg_temp.cambiar('ac000000-0000-0000-0000-000000000003',
                  $q$estado = 'EN_REVISION', nota_resolucion = 'Lo revisamos con el banco emisor.'$q$));

SELECT pg_temp.debe_pasar('cerrada la 1, Ana vuelve a aclarar el mismo retiro (la 11)', pg_temp.abrir(
  'ac000000-0000-0000-0000-000000000004', 'a0000000-0000-0000-0000-00000000000a', '0a000000-0000-0000-0000-000000000001',
  NULL, 'NO_RECIBIDO', 'Sigue sin aparecer en mi banco.', 'cli:a:aclaracion-4'));
SELECT pg_temp.exigir('folio 11, sobre el mismo movimiento que la 1', (
  SELECT numero = 11 AND spei_outbound_id = '0a000000-0000-0000-0000-000000000001'
    FROM ifpe.aclaraciones_spei WHERE id = 'ac000000-0000-0000-0000-000000000004'));

-- -----------------------------------------------------------------------------
-- Inmutables y privilegios
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('los términos no cambian (ni para el dueño del esquema): el motivo', ARRAY[
  $q$UPDATE ifpe.aclaraciones_spei SET motivo = 'OTRO' WHERE id = 'ac000000-0000-0000-0000-000000000004'$q$],
  'SC_ACLARACION_INMUTABLE');
SELECT pg_temp.debe_fallar('ni la clave de rastreo', ARRAY[
  $q$UPDATE ifpe.aclaraciones_spei SET clave_rastreo = 'OTRA' WHERE id = 'ac000000-0000-0000-0000-000000000004'$q$],
  'SC_ACLARACION_INMUTABLE');
SELECT pg_temp.debe_fallar('ni la descripción', ARRAY[
  $q$UPDATE ifpe.aclaraciones_spei SET descripcion = 'Otra historia.' WHERE id = 'ac000000-0000-0000-0000-000000000004'$q$],
  'SC_ACLARACION_INMUTABLE');
SELECT pg_temp.debe_fallar('sc_ifpe no cambia el estado', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$UPDATE ifpe.aclaraciones_spei SET estado = 'RESUELTA' WHERE id = 'ac000000-0000-0000-0000-000000000004'$q$],
  'permission denied');
SELECT pg_temp.debe_fallar('sc_spei no abre', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$INSERT INTO ifpe.aclaraciones_spei (id, cliente_id, spei_inbound_id, motivo, clave_idempotencia)
     VALUES ('ac000000-0000-0000-0000-000000000099', 'a0000000-0000-0000-0000-00000000000a',
             'd2000000-0000-0000-0000-000000000001', 'DEVOLUCION', 'spei:x')$q$],
  'permission denied');
SELECT pg_temp.debe_fallar('ni cambia el motivo', pg_temp.cambiar(
  'ac000000-0000-0000-0000-000000000004', $q$motivo = 'OTRO'$q$), 'permission denied');
SELECT pg_temp.debe_fallar('una aclaración no se borra', ARRAY[
  $q$DELETE FROM ifpe.aclaraciones_spei WHERE id = 'ac000000-0000-0000-0000-000000000002'$q$], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('ni se vacía la tabla', ARRAY['TRUNCATE ifpe.aclaraciones_spei'], 'SC_LEDGER_INMUTABLE');

-- -----------------------------------------------------------------------------
-- La bitácora: cada rol escribe solo sus eventos
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('sc_ifpe no escribe un paso de la institución', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, detalle)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'ACLARACION_SPEI_RESUELTA', '4')$q$],
  'row-level security');
SELECT pg_temp.debe_fallar('ni la apertura de un folio que no existe', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, detalle)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'ACLARACION_SPEI_ABIERTA', '77')$q$],
  'row-level security');
SELECT pg_temp.debe_fallar('ni otro evento de seguridad', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento) VALUES ('a0000000-0000-0000-0000-00000000000a', 'INICIO_SESION')$q$],
  'row-level security');
SELECT pg_temp.debe_fallar('sc_spei no escribe una apertura', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, detalle)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'ACLARACION_SPEI_ABIERTA', '4')$q$],
  'row-level security');
SELECT pg_temp.debe_fallar('ni un paso que no es el estado vigente de la aclaración', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, detalle)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'ACLARACION_SPEI_RESUELTA', '4')$q$],
  'row-level security');
SELECT pg_temp.debe_fallar('ni con una sesión', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, detalle, ip)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'ACLARACION_SPEI_EN_REVISION', '3', '203.0.113.9')$q$],
  'permission denied');
SELECT pg_temp.debe_fallar('un evento por folio y estado (ni el dueño del esquema lo duplica)', ARRAY[
  $q$INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, detalle)
     VALUES ('a0000000-0000-0000-0000-00000000000a', 'ACLARACION_SPEI_RESUELTA', '1')$q$],
  'ux_eventos_autenticacion_aclaracion');

-- -----------------------------------------------------------------------------
-- RLS: el cliente lee solo lo suyo
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('Ana ve sus 11 aclaraciones y sus 15 pasos; Beto, ninguno de ellos',
  pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM ifpe.aclaraciones_spei') = 11
  AND pg_temp.visibles('a0000000-0000-0000-0000-00000000000a',
        $q$SELECT count(*) FROM kyc.eventos_autenticacion WHERE evento LIKE 'ACLARACION_SPEI_%'$q$) = 15
  AND pg_temp.visibles('b0000000-0000-0000-0000-00000000000b', 'SELECT count(*) FROM ifpe.aclaraciones_spei') = 0
  AND pg_temp.visibles('b0000000-0000-0000-0000-00000000000b',
        $q$SELECT count(*) FROM kyc.eventos_autenticacion WHERE evento LIKE 'ACLARACION_SPEI_%'$q$) = 0);
SELECT pg_temp.exigir('sin cliente en la transacción, sc_cliente no ve ninguna',
  pg_temp.visibles('', 'SELECT count(*) FROM ifpe.aclaraciones_spei') = 0);

\o
\echo tests/db/0025_aclaraciones_spei.sql: todas las pruebas pasaron (ROLLBACK)
ROLLBACK;
