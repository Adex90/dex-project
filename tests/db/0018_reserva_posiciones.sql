-- =============================================================================
--  Pruebas de la migración 0018 (posiciones del mercado en el Proof of
--  Reserve). Corren en una transacción que termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0018_reserva_posiciones.sql
--
--  Lo que la base de datos garantiza por sí sola al publicar un corte: el
--  ancla del ledger del mercado es su cabeza y no retrocede; hay un árbol por
--  cada activo con posiciones abiertas y ninguno más; sus hojas son
--  exactamente las posiciones abiertas vivas (activo, cuenta, cantidad), con
--  índices 0..n-1, el número y la suma declarados y el hash que corresponde a
--  su contenido; el desglose de cada hoja del efectivo es el de su cuenta; y
--  nada de eso cambia después. El hash del corte honesto viene del vector de
--  referencia, calculado aparte desde la especificación (el mismo que usan
--  backend/src/crypto/merkle.rs y frontend/src/lib/merkle-verifier.test.ts).
--  backend/tests/reserva.rs cubre los árboles, las raíces y las pruebas.
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

-- Una compra completa, como la escribe el backend (backend/src/mercado; 0013).
CREATE FUNCTION pg_temp.compra(cliente TEXT, cuenta TEXT, orden_id TEXT, tx_id TEXT, clave TEXT,
                               ticker TEXT, solicitado TEXT, cantidad TEXT, precio TEXT, monto TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
        'SET LOCAL ROLE sc_mercado',
        format($q$SELECT ifpe.aml_evaluar(%L, 'SALIDA', 'COMPRA_MERCADO', %s, %L, %L, '{}'::jsonb)$q$,
               cliente, monto, tx_id, clave),
        format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - %s WHERE id = %L$q$, monto, cuenta),
        format($q$UPDATE ifpe.reserva_custodia SET saldo = saldo - %s WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
               monto),
        format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, concepto,
                                                   clave_idempotencia, ref_movimiento_espejo)
                  VALUES (%L, 'COMPRA_MERCADO', %L, 'e0000000-0000-0000-0000-00000000000e', %s, %L, %L, %L)$q$,
               tx_id, cuenta, monto, 'Compra de ' || ticker, clave, orden_id),
        format($q$INSERT INTO mercado.ordenes (id, cliente_id, cuenta_ifpe_id, ticker, lado, monto_solicitado, cantidad,
                                                precio, monto, fuente_precio, tx_ifpe_id, clave_idempotencia)
                  VALUES (%L, %L, %L, %L, 'COMPRA', %s, %s, %s, %s, 'SIMULADO', %L, %L)$q$,
               orden_id, cliente, cuenta, ticker, solicitado, cantidad, precio, monto, tx_id, clave),
        'RESET ROLE']
$$;

-- Las sentencias de un corte de la foto actual, como las escribe el backend
-- (backend/src/reserva.rs) con el rol sc_auditor: el corte con sus dos anclas
-- y las hojas del efectivo con su desglose, y después `extra` (los árboles de
-- posiciones o lo que la prueba altere). `ancla_mercado` sustituye la
-- secuencia del ancla del mercado; `desglose` en falso deja el desglose en NULL.
CREATE FUNCTION pg_temp.corte(id TEXT, extra TEXT[], ancla_mercado TEXT DEFAULT NULL,
                              desglose BOOLEAN DEFAULT true) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
        'SET LOCAL ROLE sc_auditor',
        format($q$INSERT INTO ifpe.cortes_reserva (id, merkle_root, num_hojas, total_pasivo, total_reserva,
                                                    secuencia_ledger, hash_ledger, secuencia_mercado, hash_mercado)
                  SELECT %L, repeat('a', 64), (SELECT count(*) FROM ifpe.v_hojas_reserva), e.pasivo_clientes,
                         e.activo_reserva, l.ultima_secuencia, l.ultimo_hash, %s, m.ultimo_hash
                    FROM ifpe.estado_reserva e, ifpe.ledger_cabeza l, mercado.ledger_cabeza m$q$,
               id, coalesce(ancla_mercado, 'm.ultima_secuencia')),
        format($q$INSERT INTO ifpe.cortes_reserva_hojas (corte_id, indice, cuenta_id, saldo_total, sal, hoja_hash,
                                                          saldo_disponible, saldo_bloqueado, saldo_comprometido)
                  SELECT %L, (row_number() OVER (ORDER BY cuenta_id)) - 1, cuenta_id, saldo_total, repeat('07', 32),
                         encode(sha256(convert_to('SC-POR-v1:hoja', 'UTF8') || '\x00'::bytea || uuid_send(cuenta_id)
                                || '\x0000000000000000'::bytea || int8send((saldo_total * 100000000)::bigint)
                                || decode(repeat('07', 32), 'hex')), 'hex'),
                         %s
                    FROM ifpe.v_hojas_reserva$q$,
               id, CASE WHEN desglose THEN 'saldo_disponible, saldo_bloqueado, saldo_comprometido'
                        ELSE 'NULL::numeric, NULL::numeric, NULL::numeric' END)
    ] || extra || ARRAY['RESET ROLE']
$$;

CREATE FUNCTION pg_temp.arbol(corte TEXT, ticker TEXT, hojas INT, total TEXT) RETURNS TEXT
LANGUAGE sql AS $$
    SELECT format($q$INSERT INTO ifpe.cortes_reserva_activos (corte_id, ticker, merkle_root, num_hojas, total_cantidad)
                     VALUES (%L, %L, repeat('b', 64), %s, %s)$q$, corte, ticker, hojas, total)
$$;

-- hoja = SHA256("SC-POR-v1:posicion" 0x00 || ticker || 0x00 || cuenta_id[16] || cantidad_u128_be[16] || sal[32]),
-- aquí solo para las hojas de las pruebas que alteran otra cosa.
CREATE FUNCTION pg_temp.hash_posicion(ticker TEXT, cuenta UUID, cantidad NUMERIC, sal TEXT) RETURNS TEXT
LANGUAGE sql AS $$
    SELECT encode(sha256(convert_to('SC-POR-v1:posicion', 'UTF8') || '\x00'::bytea || convert_to(ticker, 'UTF8')
                  || '\x00'::bytea || uuid_send(cuenta) || '\x0000000000000000'::bytea
                  || int8send((cantidad * 100000000)::bigint) || decode(sal, 'hex')), 'hex')
$$;

CREATE FUNCTION pg_temp.hoja(corte TEXT, ticker TEXT, indice INT, cuenta TEXT, cantidad TEXT, sal TEXT,
                             hash TEXT DEFAULT NULL) RETURNS TEXT
LANGUAGE sql AS $$
    SELECT format($q$INSERT INTO ifpe.cortes_reserva_posiciones (corte_id, ticker, indice, cuenta_id, cantidad, sal, hoja_hash)
                     VALUES (%L, %L, %s, %L, %s, %L, %L)$q$,
                  corte, ticker, indice, cuenta, cantidad, sal,
                  coalesce(hash, pg_temp.hash_posicion(ticker, cuenta::uuid, cantidad::numeric, sal)))
$$;

-- -----------------------------------------------------------------------------
-- Datos: Ana (su cuenta es la del vector de referencia) y Beto, verificados;
-- la reserva; 20 000 para Ana y 100 para Beto.
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ana',  'Prueba', 'ana@example.mx',  'VERIFICADO', now(), 'BAJO'),
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Beto', 'Prueba', 'beto@example.mx', 'VERIFICADO', now(), 'BAJO');
INSERT INTO ifpe.reserva_custodia (id, institucion, tipo, numero_cuenta) VALUES
  ('e0000000-0000-0000-0000-00000000000e', 'Banco de México', 'BANXICO', 'SC-RES-001');
INSERT INTO ifpe.cuentas (id, cliente_id, clabe) VALUES
  ('11111111-1111-1111-1111-111111111111', 'a0000000-0000-0000-0000-00000000000a', '646180157000000017'),
  ('1b000000-0000-0000-0000-00000000001b', 'b0000000-0000-0000-0000-00000000000b', '646180157000000020');

SELECT pg_temp.debe_pasar('depósitos SPEI de Ana (20 000) y Beto (100), como sc_spei', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 20000 WHERE id = '11111111-1111-1111-1111-111111111111'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 20000 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
     VALUES ('7d000000-0000-0000-0000-000000000001', 'DEPOSITO', '11111111-1111-1111-1111-111111111111',
             'e0000000-0000-0000-0000-00000000000e', 20000, 'spei:40012:20261101:POR0001', '40012:20261101:POR0001')$q$,
  $q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                    estado, transaccion_id, origen, raw_payload)
     VALUES ('40012', 'POR0001', '2026-11-01', '646180157000000017', '11111111-1111-1111-1111-111111111111', 20000,
             'ABONADO', '7d000000-0000-0000-0000-000000000001', 'WEBHOOK', '{}')$q$,
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 100 WHERE id = '1b000000-0000-0000-0000-00000000001b'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 100 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
     VALUES ('7d000000-0000-0000-0000-000000000002', 'DEPOSITO', '1b000000-0000-0000-0000-00000000001b',
             'e0000000-0000-0000-0000-00000000000e', 100, 'spei:40012:20261101:POR0002', '40012:20261101:POR0002')$q$,
  $q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                    estado, transaccion_id, origen, raw_payload)
     VALUES ('40012', 'POR0002', '2026-11-01', '646180157000000020', '1b000000-0000-0000-0000-00000000001b', 100,
             'ABONADO', '7d000000-0000-0000-0000-000000000002', 'WEBHOOK', '{}')$q$,
  'RESET ROLE'
]);

-- -----------------------------------------------------------------------------
-- Estructura y permisos
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('el corte guarda el ancla del mercado; las hojas del efectivo, su desglose', (
  SELECT count(*) = 2 FROM information_schema.columns
   WHERE table_schema = 'ifpe' AND table_name = 'cortes_reserva' AND column_name IN ('secuencia_mercado', 'hash_mercado'))
  AND (SELECT count(*) = 3 FROM information_schema.columns
        WHERE table_schema = 'ifpe' AND table_name = 'cortes_reserva_hojas'
          AND column_name IN ('saldo_disponible', 'saldo_bloqueado', 'saldo_comprometido')));

SELECT pg_temp.exigir('sc_auditor lee las posiciones y la cabeza del mercado, y publica los árboles; nada más', (
  has_table_privilege('sc_auditor', 'mercado.posiciones', 'SELECT')
  AND has_table_privilege('sc_auditor', 'mercado.ledger_cabeza', 'SELECT')
  AND NOT has_table_privilege('sc_auditor', 'mercado.posiciones', 'UPDATE')
  AND NOT has_table_privilege('sc_auditor', 'mercado.ordenes', 'SELECT')
  AND has_table_privilege('sc_auditor', 'ifpe.cortes_reserva_activos', 'INSERT')
  AND has_table_privilege('sc_auditor', 'ifpe.cortes_reserva_posiciones', 'INSERT')
  AND NOT has_table_privilege('sc_auditor', 'ifpe.cortes_reserva_posiciones', 'UPDATE')));

SELECT pg_temp.exigir('sc_cliente no lee los árboles (las pruebas las sirve el backend)', (
  NOT has_table_privilege('sc_cliente', 'ifpe.cortes_reserva_activos', 'SELECT')
  AND NOT has_table_privilege('sc_cliente', 'ifpe.cortes_reserva_posiciones', 'SELECT')));

SELECT pg_temp.debe_pasar('Ana compra 2.5 g de AUG1 (4 031.00), como sc_mercado',
  pg_temp.compra('a0000000-0000-0000-0000-00000000000a', '11111111-1111-1111-1111-111111111111',
                 '0d000000-0000-0000-0000-000000000001', '7e000000-0000-0000-0000-000000000001',
                 'cli:a0000000-0000-0000-0000-00000000000a:compra-1', 'AUG1', '4031', '2.5', '1612.40', '4031.00'));

-- -----------------------------------------------------------------------------
-- Cortes que no reflejan las posiciones
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('un corte sin el árbol del oro', pg_temp.corte('c1000000-0000-0000-0000-000000000001', ARRAY[]::text[]),
  'las hojas de posiciones no coinciden');

SELECT pg_temp.debe_fallar('una cantidad distinta de la viva, aunque su hash sea el de su contenido',
  pg_temp.corte('c1000000-0000-0000-0000-000000000002', ARRAY[
    pg_temp.arbol('c1000000-0000-0000-0000-000000000002', 'AUG1', 1, '2.4'),
    pg_temp.hoja('c1000000-0000-0000-0000-000000000002', 'AUG1', 0, '11111111-1111-1111-1111-111111111111', '2.4',
                 repeat('dd', 32))]),
  'las hojas de posiciones no coinciden');

SELECT pg_temp.debe_fallar('la posición de Ana como si fuera de Beto',
  pg_temp.corte('c1000000-0000-0000-0000-000000000003', ARRAY[
    pg_temp.arbol('c1000000-0000-0000-0000-000000000003', 'AUG1', 1, '2.5'),
    pg_temp.hoja('c1000000-0000-0000-0000-000000000003', 'AUG1', 0, '1b000000-0000-0000-0000-00000000001b', '2.5',
                 repeat('dd', 32))]),
  'las hojas de posiciones no coinciden');

SELECT pg_temp.debe_fallar('el oro de Ana en el árbol de la plata',
  pg_temp.corte('c1000000-0000-0000-0000-000000000004', ARRAY[
    pg_temp.arbol('c1000000-0000-0000-0000-000000000004', 'AGT1', 1, '2.5'),
    pg_temp.hoja('c1000000-0000-0000-0000-000000000004', 'AGT1', 0, '11111111-1111-1111-1111-111111111111', '2.5',
                 repeat('dd', 32))]),
  'las hojas de posiciones no coinciden');

SELECT pg_temp.debe_fallar('un árbol de sobra, sin hojas (plata, que nadie tiene)',
  pg_temp.corte('c1000000-0000-0000-0000-000000000005', ARRAY[
    pg_temp.arbol('c1000000-0000-0000-0000-000000000005', 'AUG1', 1, '2.5'),
    pg_temp.hoja('c1000000-0000-0000-0000-000000000005', 'AUG1', 0, '11111111-1111-1111-1111-111111111111', '2.5',
                 repeat('dd', 32)),
    pg_temp.arbol('c1000000-0000-0000-0000-000000000005', 'AGT1', 1, '1')]),
  'SC_CORTE_INCOMPLETO');

SELECT pg_temp.debe_fallar('un total declarado mayor que la suma de las hojas',
  pg_temp.corte('c1000000-0000-0000-0000-000000000006', ARRAY[
    pg_temp.arbol('c1000000-0000-0000-0000-000000000006', 'AUG1', 1, '3'),
    pg_temp.hoja('c1000000-0000-0000-0000-000000000006', 'AUG1', 0, '11111111-1111-1111-1111-111111111111', '2.5',
                 repeat('dd', 32))]),
  'SC_CORTE_INCOMPLETO');

SELECT pg_temp.debe_fallar('índices que no empiezan en 0',
  pg_temp.corte('c1000000-0000-0000-0000-000000000007', ARRAY[
    pg_temp.arbol('c1000000-0000-0000-0000-000000000007', 'AUG1', 1, '2.5'),
    pg_temp.hoja('c1000000-0000-0000-0000-000000000007', 'AUG1', 1, '11111111-1111-1111-1111-111111111111', '2.5',
                 repeat('dd', 32))]),
  'SC_CORTE_INCOMPLETO');

SELECT pg_temp.debe_fallar('un hash de hoja inventado',
  pg_temp.corte('c1000000-0000-0000-0000-000000000008', ARRAY[
    pg_temp.arbol('c1000000-0000-0000-0000-000000000008', 'AUG1', 1, '2.5'),
    pg_temp.hoja('c1000000-0000-0000-0000-000000000008', 'AUG1', 0, '11111111-1111-1111-1111-111111111111', '2.5',
                 repeat('dd', 32), repeat('c', 64))]),
  'tienen un hash que no corresponde');

SELECT pg_temp.debe_fallar('una hoja sin árbol declarado',
  pg_temp.corte('c1000000-0000-0000-0000-000000000009', ARRAY[
    pg_temp.hoja('c1000000-0000-0000-0000-000000000009', 'AUG1', 0, '11111111-1111-1111-1111-111111111111', '2.5',
                 repeat('dd', 32))]),
  'fk_cortes_posiciones_arbol');

-- -----------------------------------------------------------------------------
-- Anclas y desglose
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('un ancla del mercado que no es su cabeza',
  pg_temp.corte('c2000000-0000-0000-0000-000000000001', ARRAY[
    pg_temp.arbol('c2000000-0000-0000-0000-000000000001', 'AUG1', 1, '2.5'),
    pg_temp.hoja('c2000000-0000-0000-0000-000000000001', 'AUG1', 0, '11111111-1111-1111-1111-111111111111', '2.5',
                 repeat('dd', 32))], ancla_mercado => '0'),
  'el ancla del mercado del corte');

SELECT pg_temp.debe_fallar('un corte sin ancla del mercado',
  pg_temp.corte('c2000000-0000-0000-0000-000000000002', ARRAY[
    pg_temp.arbol('c2000000-0000-0000-0000-000000000002', 'AUG1', 1, '2.5'),
    pg_temp.hoja('c2000000-0000-0000-0000-000000000002', 'AUG1', 0, '11111111-1111-1111-1111-111111111111', '2.5',
                 repeat('dd', 32))], ancla_mercado => 'NULL'),
  'ck_cortes_reserva_ancla_mercado');

SELECT pg_temp.debe_fallar('hojas del efectivo sin desglose',
  pg_temp.corte('c2000000-0000-0000-0000-000000000003', ARRAY[
    pg_temp.arbol('c2000000-0000-0000-0000-000000000003', 'AUG1', 1, '2.5'),
    pg_temp.hoja('c2000000-0000-0000-0000-000000000003', 'AUG1', 0, '11111111-1111-1111-1111-111111111111', '2.5',
                 repeat('dd', 32))], desglose => false),
  'el desglose de');

SELECT pg_temp.debe_fallar('un desglose que no suma el total de la hoja', ARRAY[
  $q$INSERT INTO ifpe.cortes_reserva (id, merkle_root, num_hojas, total_pasivo, total_reserva, secuencia_ledger, hash_ledger)
     VALUES ('c2000000-0000-0000-0000-000000000004', repeat('a', 64), 1, 1, 1, 1, repeat('b', 64))$q$,
  $q$INSERT INTO ifpe.cortes_reserva_hojas (corte_id, indice, cuenta_id, saldo_total, sal, hoja_hash,
                                            saldo_disponible, saldo_bloqueado, saldo_comprometido)
     VALUES ('c2000000-0000-0000-0000-000000000004', 0, '1b000000-0000-0000-0000-00000000001b', 100,
             repeat('07', 32), repeat('c', 64), 99, 0, 0)$q$
], 'ck_cortes_hojas_desglose');

SELECT pg_temp.debe_fallar('un desglose que suma el total pero no es el de la cuenta', ARRAY[
  $q$SET LOCAL ROLE sc_auditor$q$,
  $q$INSERT INTO ifpe.cortes_reserva (id, merkle_root, num_hojas, total_pasivo, total_reserva,
                                      secuencia_ledger, hash_ledger, secuencia_mercado, hash_mercado)
     SELECT 'c2000000-0000-0000-0000-000000000005', repeat('a', 64), 2, e.pasivo_clientes, e.activo_reserva,
            l.ultima_secuencia, l.ultimo_hash, m.ultima_secuencia, m.ultimo_hash
       FROM ifpe.estado_reserva e, ifpe.ledger_cabeza l, mercado.ledger_cabeza m$q$,
  -- Beto: 100 disponibles, declarados como 60 disponibles y 40 retenidos.
  $q$INSERT INTO ifpe.cortes_reserva_hojas (corte_id, indice, cuenta_id, saldo_total, sal, hoja_hash,
                                            saldo_disponible, saldo_bloqueado, saldo_comprometido)
     SELECT 'c2000000-0000-0000-0000-000000000005', (row_number() OVER (ORDER BY cuenta_id)) - 1, cuenta_id, saldo_total,
            repeat('07', 32),
            encode(sha256(convert_to('SC-POR-v1:hoja', 'UTF8') || '\x00'::bytea || uuid_send(cuenta_id)
                   || '\x0000000000000000'::bytea || int8send((saldo_total * 100000000)::bigint)
                   || decode(repeat('07', 32), 'hex')), 'hex'),
            CASE WHEN cuenta_id = '1b000000-0000-0000-0000-00000000001b' THEN 60 ELSE saldo_disponible END,
            CASE WHEN cuenta_id = '1b000000-0000-0000-0000-00000000001b' THEN 40 ELSE saldo_bloqueado END,
            saldo_comprometido
       FROM ifpe.v_hojas_reserva$q$,
  pg_temp.arbol('c2000000-0000-0000-0000-000000000005', 'AUG1', 1, '2.5'),
  pg_temp.hoja('c2000000-0000-0000-0000-000000000005', 'AUG1', 0, '11111111-1111-1111-1111-111111111111', '2.5',
               repeat('dd', 32)),
  'RESET ROLE'
], 'el desglose de 1 hojas');

-- -----------------------------------------------------------------------------
-- El corte honesto, con el hash del vector de referencia
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('vector de referencia: el hash de la hoja que calcula la base de datos', (
  pg_temp.hash_posicion('AUG1', '11111111-1111-1111-1111-111111111111', 2.5, repeat('dd', 32))
    = '1c07aca64ba5dc119b5f606c9c9f7fca0c43285f44405805d40f8990b7721ac2'));

SELECT pg_temp.debe_pasar('el corte honesto, con la hoja del vector de referencia, como sc_auditor',
  pg_temp.corte('c3000000-0000-0000-0000-000000000001', ARRAY[
    pg_temp.arbol('c3000000-0000-0000-0000-000000000001', 'AUG1', 1, '2.5'),
    pg_temp.hoja('c3000000-0000-0000-0000-000000000001', 'AUG1', 0, '11111111-1111-1111-1111-111111111111', '2.5',
                 repeat('dd', 32), '1c07aca64ba5dc119b5f606c9c9f7fca0c43285f44405805d40f8990b7721ac2')]));

SELECT pg_temp.exigir('el corte quedó anclado a la orden de Ana en el ledger del mercado', (
  SELECT c.secuencia_mercado = 1 AND c.hash_mercado = o.tx_hash
    FROM ifpe.cortes_reserva c, mercado.ordenes o
   WHERE c.id = 'c3000000-0000-0000-0000-000000000001' AND o.secuencia = 1));

-- -----------------------------------------------------------------------------
-- Después de publicar
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('una hoja publicada no cambia (ni como dueño)', ARRAY[
  $q$UPDATE ifpe.cortes_reserva_posiciones SET cantidad = 1 WHERE corte_id = 'c3000000-0000-0000-0000-000000000001'$q$
], 'SC_');
SELECT pg_temp.debe_fallar('un árbol publicado no cambia', ARRAY[
  $q$UPDATE ifpe.cortes_reserva_activos SET total_cantidad = 1 WHERE corte_id = 'c3000000-0000-0000-0000-000000000001'$q$
], 'SC_');
SELECT pg_temp.debe_fallar('ni se borra', ARRAY[
  $q$DELETE FROM ifpe.cortes_reserva_posiciones WHERE corte_id = 'c3000000-0000-0000-0000-000000000001'$q$
], 'SC_');
SELECT pg_temp.debe_fallar('ni se vacía la tabla', ARRAY['TRUNCATE ifpe.cortes_reserva_posiciones'], 'SC_');

SELECT pg_temp.debe_fallar('el ancla del mercado no retrocede: un ledger del mercado truncado no publica', ARRAY[
  $q$UPDATE mercado.ledger_cabeza SET ultima_secuencia = 0, ultimo_hash = repeat('0', 64)$q$
] || pg_temp.corte('c3000000-0000-0000-0000-000000000002', ARRAY[
    pg_temp.arbol('c3000000-0000-0000-0000-000000000002', 'AUG1', 1, '2.5'),
    pg_temp.hoja('c3000000-0000-0000-0000-000000000002', 'AUG1', 0, '11111111-1111-1111-1111-111111111111', '2.5',
                 repeat('ee', 32))]),
  'el ancla del mercado retrocede');

SELECT pg_temp.debe_pasar('Beto compra también: el siguiente corte trae las dos hojas del oro',
  pg_temp.compra('b0000000-0000-0000-0000-00000000000b', '1b000000-0000-0000-0000-00000000001b',
                 '0d000000-0000-0000-0000-000000000002', '7e000000-0000-0000-0000-000000000002',
                 'cli:b0000000-0000-0000-0000-00000000000b:compra-1', 'AUG1', '50', '0.031', '1612.40', '49.98')
  || pg_temp.corte('c3000000-0000-0000-0000-000000000003', ARRAY[
       pg_temp.arbol('c3000000-0000-0000-0000-000000000003', 'AUG1', 2, '2.531'),
       pg_temp.hoja('c3000000-0000-0000-0000-000000000003', 'AUG1', 0, '1b000000-0000-0000-0000-00000000001b', '0.031',
                    repeat('01', 32)),
       pg_temp.hoja('c3000000-0000-0000-0000-000000000003', 'AUG1', 1, '11111111-1111-1111-1111-111111111111', '2.5',
                    repeat('02', 32))]));

SELECT pg_temp.exigir('dos cortes publicados con árboles de posiciones, el último con 2.531 g', (
  SELECT count(DISTINCT corte_id) = 2 FROM ifpe.cortes_reserva_activos)
  AND (SELECT total_cantidad = 2.531 AND num_hojas = 2 FROM ifpe.cortes_reserva_activos
        WHERE corte_id = 'c3000000-0000-0000-0000-000000000003'));

\o
\echo 'tests/db/0018_reserva_posiciones.sql: todas las pruebas pasaron (ROLLBACK)'
ROLLBACK;
