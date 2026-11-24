-- =============================================================================
--  Pruebas de la migración 0023 (planes de ahorro). Corren en una transacción
--  que termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0023_planes_ahorro.sql
--
--  Lo que la base de datos garantiza por sí sola: un plan nace activo, de la
--  bóveda del cliente verificado, sobre un activo que opera y con un monto que
--  compra al menos un incremento; su ancla, su primera compra (ya) y cada fecha
--  siguiente (9:00 de la Ciudad de México, cada 7 o 14 días o el mismo día de
--  cada mes) son de la base de datos; sus términos no cambian; solo pausa,
--  reanuda o cancela (definitivo); una compra vencida se registra hecha (con
--  la orden de su periodo y de los términos del plan) u omitida (con su
--  motivo), y la siguiente es la primera que aún no llega; una orden con clave
--  de plan existe solo con su registro; cada cambio deja su evento inmutable y
--  avisa a la bóveda. backend/tests/planes.rs cubre la API y el programador.
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

-- Como debe_fallar, pero revisa solo una restricción diferida.
CREATE FUNCTION pg_temp.debe_fallar_en(prueba TEXT, sentencias TEXT[], restriccion TEXT, patron TEXT) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    s TEXT;
BEGIN
    BEGIN
        FOREACH s IN ARRAY sentencias LOOP
            EXECUTE s;
        END LOOP;
        EXECUTE format('SET CONSTRAINTS %s IMMEDIATE', restriccion);
        RAISE EXCEPTION 'SC_PRUEBA_ACEPTADA';
    EXCEPTION WHEN OTHERS THEN
        SET CONSTRAINTS ALL DEFERRED;
        IF SQLERRM = 'SC_PRUEBA_ACEPTADA' THEN
            RAISE EXCEPTION 'FALLO [%]: se esperaba "%" de % pero las sentencias se aceptaron', prueba, patron, restriccion;
        END IF;
        IF strpos(SQLERRM, patron) = 0 THEN
            RAISE EXCEPTION 'FALLO [%]: error inesperado "%" (se esperaba "%")', prueba, SQLERRM, patron;
        END IF;
        RAISE NOTICE 'OK  rechazado  %', prueba;
    END;
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

-- El alta de un plan como la hace el backend (sc_mercado): solo los términos y la clave.
CREATE FUNCTION pg_temp.alta(id TEXT, cliente TEXT, cuenta TEXT, ticker TEXT, monto TEXT, frecuencia TEXT,
                             clave TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
        'SET LOCAL ROLE sc_mercado',
        format($q$INSERT INTO mercado.planes_ahorro (id, cliente_id, cuenta_ifpe_id, ticker, monto, frecuencia,
                                                      clave_idempotencia)
                  VALUES (%L, %L, %L, %L, %s, %L, %L)$q$, id, cliente, cuenta, ticker, monto, frecuencia, clave),
        'RESET ROLE']
$$;

-- Una compra del plan, como la escribe el programador: PLD, saldos, el
-- movimiento IFPE, la orden de mercado con la clave del periodo y su registro.
CREATE FUNCTION pg_temp.compra(plan TEXT, periodo INTEGER, orden TEXT, tx TEXT, cliente TEXT, cuenta TEXT,
                               ticker TEXT, solicitado TEXT, cantidad TEXT, precio TEXT, monto TEXT,
                               registrar BOOLEAN DEFAULT TRUE) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
        'SET LOCAL ROLE sc_mercado',
        format($q$SELECT ifpe.aml_evaluar(%L, 'SALIDA', 'COMPRA_MERCADO', %s, %L, %L, '{}'::jsonb)$q$,
               cliente, monto, tx, 'plan:' || plan || ':' || periodo),
        format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - %s WHERE id = %L$q$, monto, cuenta),
        format($q$UPDATE ifpe.reserva_custodia SET saldo = saldo - %s WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
               monto),
        format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, concepto,
                                                   clave_idempotencia, ref_movimiento_espejo)
                  VALUES (%L, 'COMPRA_MERCADO', %L, 'e0000000-0000-0000-0000-00000000000e', %s, %L, %L, %L)$q$,
               tx, cuenta, monto, 'Compra de ' || ticker, 'plan:' || plan || ':' || periodo, orden),
        format($q$INSERT INTO mercado.ordenes (id, cliente_id, cuenta_ifpe_id, ticker, lado, monto_solicitado, cantidad,
                                                precio, monto, fuente_precio, tx_ifpe_id, clave_idempotencia)
                  VALUES (%L, %L, %L, %L, 'COMPRA', %s, %s, %s, %s, 'SIMULADO', %L, %L)$q$,
               orden, cliente, cuenta, ticker, solicitado, cantidad, precio, monto, tx, 'plan:' || plan || ':' || periodo),
        CASE WHEN registrar THEN
            format($q$UPDATE mercado.planes_ahorro SET ultimo_resultado = 'EJECUTADA', motivo = NULL, ultima_orden_id = %L
                      WHERE id = %L$q$, orden, plan)
        ELSE 'SELECT 1' END,
        'RESET ROLE']
$$;

-- Una compra omitida, con el código de la regla que la impidió.
CREATE FUNCTION pg_temp.omision(plan TEXT, motivo TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
        'SET LOCAL ROLE sc_mercado',
        format($q$UPDATE mercado.planes_ahorro SET ultimo_resultado = 'OMITIDA', motivo = %L, ultima_orden_id = NULL
                  WHERE id = %L$q$, motivo, plan),
        'RESET ROLE']
$$;

-- Como si ya hubiera llegado la compra siguiente (aquí `now()` no avanza): el
-- dueño del esquema adelanta su fecha sin el trigger que la protege.
CREATE FUNCTION pg_temp.vencer(plan TEXT) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
        'ALTER TABLE mercado.planes_ahorro DISABLE TRIGGER trg_planes_b_transicion',
        format($q$UPDATE mercado.planes_ahorro SET proxima_ejecucion_en = now() WHERE id = %L$q$, plan),
        'ALTER TABLE mercado.planes_ahorro ENABLE TRIGGER trg_planes_b_transicion']
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
-- Datos: Ana y Beto verificados, Carla sin KYC; la reserva; 20 000 para Ana
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ana',   'Prueba', 'ana@example.mx',   'VERIFICADO', now(), 'BAJO'),
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Beto',  'Prueba', 'beto@example.mx',  'VERIFICADO', now(), 'BAJO');
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, nivel_riesgo_pld) VALUES
  ('c0000000-0000-0000-0000-00000000000c', 'FISICA', 'Carla', 'Prueba', 'carla@example.mx', 'BAJO');
INSERT INTO ifpe.reserva_custodia (id, institucion, tipo, numero_cuenta) VALUES
  ('e0000000-0000-0000-0000-00000000000e', 'Banco de México', 'BANXICO', 'SC-RES-001');
INSERT INTO ifpe.cuentas (id, cliente_id, clabe) VALUES
  ('1a000000-0000-0000-0000-00000000001a', 'a0000000-0000-0000-0000-00000000000a', '646180157000000017'),
  ('1b000000-0000-0000-0000-00000000001b', 'b0000000-0000-0000-0000-00000000000b', '646180157000000020'),
  ('1c000000-0000-0000-0000-00000000001c', 'c0000000-0000-0000-0000-00000000000c', '646180157000000033');

SELECT pg_temp.debe_pasar('un depósito SPEI de 20 000 para Ana, como sc_spei', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 20000 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 20000 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
     VALUES ('7d000000-0000-0000-0000-000000000001', 'DEPOSITO', '1a000000-0000-0000-0000-00000000001a',
             'e0000000-0000-0000-0000-00000000000e', 20000, 'spei:40012:20261101:PLN0001', '40012:20261101:PLN0001')$q$,
  $q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                    estado, transaccion_id, origen, raw_payload)
     VALUES ('40012', 'PLN0001', '2026-11-01', '646180157000000017', '1a000000-0000-0000-0000-00000000001a', 20000,
             'ABONADO', '7d000000-0000-0000-0000-000000000001', 'WEBHOOK', '{}')$q$,
  'RESET ROLE'
]);

-- -----------------------------------------------------------------------------
-- 1. El calendario
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('9:00 de la Ciudad de México: cada 7 días, cada 14 o el mismo día de cada mes', (
  mercado.momento_plan('SEMANAL', '2026-11-24', 0) = '2026-11-24 15:00:00+00'
  AND mercado.momento_plan('SEMANAL', '2026-11-24', 1) = '2026-12-01 15:00:00+00'
  AND mercado.momento_plan('QUINCENAL', '2026-11-24', 2) = '2026-12-22 15:00:00+00'
  AND mercado.momento_plan('MENSUAL', '2026-11-24', 2) = '2027-01-24 15:00:00+00'));
SELECT pg_temp.exigir('el 31 cae en el último día de los meses cortos, sin correrse después', (
  mercado.momento_plan('MENSUAL', '2026-01-31', 1) = '2026-02-28 15:00:00+00'
  AND mercado.momento_plan('MENSUAL', '2026-01-31', 2) = '2026-03-31 15:00:00+00'
  AND mercado.momento_plan('MENSUAL', '2028-01-31', 1) = '2028-02-29 15:00:00+00'));
SELECT pg_temp.exigir('la siguiente compra es la primera que aún no llega', (
  mercado.momento_plan('SEMANAL', current_date - 22,
                       mercado.siguiente_periodo('SEMANAL', current_date - 22, 0)) > now()
  AND mercado.momento_plan('SEMANAL', current_date - 22,
                           mercado.siguiente_periodo('SEMANAL', current_date - 22, 0) - 1) <= now()));

-- -----------------------------------------------------------------------------
-- 2. El alta
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana da de alta un plan semanal de 500 MXN de AUG1', pg_temp.alta(
  '9a000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a',
  '1a000000-0000-0000-0000-00000000001a', 'AUG1', '500', 'SEMANAL', 'cli:a0000000-0000-0000-0000-00000000000a:plan-1'));
SELECT pg_temp.exigir('nace activo, anclado en el día de la Ciudad de México y con la primera compra ya', (
  SELECT estado = 'ACTIVO' AND periodo = 0 AND fecha_ancla = (now() AT TIME ZONE 'America/Mexico_City')::date
     AND proxima_ejecucion_en = now() AND creado_en = now() AND ultimo_resultado IS NULL
     AND ultima_ejecucion_en IS NULL AND cancelado_en IS NULL
    FROM mercado.planes_ahorro WHERE id = '9a000000-0000-0000-0000-000000000001'));
SELECT pg_temp.exigir('su evento CREADO, con lo que quedó programado, y la bóveda avisa', (
  SELECT count(*) = 1 AND bool_and(evento = 'CREADO' AND monto = 500 AND proxima_ejecucion_en = now()
                                   AND registrado_por = session_user AND periodo IS NULL)
    FROM mercado.eventos_plan WHERE plan_id = '9a000000-0000-0000-0000-000000000001')
  AND pg_temp.version('1a000000-0000-0000-0000-00000000001a') = 1);
SELECT pg_temp.debe_fallar('sc_mercado no elige el ancla, el estado ni la primera fecha', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$INSERT INTO mercado.planes_ahorro (id, cliente_id, cuenta_ifpe_id, ticker, monto, frecuencia, clave_idempotencia,
                                        fecha_ancla)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
             'AUG1', 500, 'SEMANAL', 'cli:x:plan-ancla', '2020-01-01')$q$
], 'permission denied');
SELECT pg_temp.debe_fallar('ni el dueño del esquema da de alta un plan pausado', ARRAY[
  $q$INSERT INTO mercado.planes_ahorro (id, cliente_id, cuenta_ifpe_id, ticker, monto, frecuencia, clave_idempotencia,
                                        fecha_ancla, estado)
     VALUES (gen_random_uuid(), 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a',
             'AUG1', 500, 'SEMANAL', 'cli:x:plan-pausado', current_date, 'PAUSADO')$q$
], 'SC_PLAN_INCOHERENTE');
SELECT pg_temp.debe_fallar('Carla, sin KYC verificado, no da de alta un plan', pg_temp.alta(
  '9c000000-0000-0000-0000-000000000001', 'c0000000-0000-0000-0000-00000000000c',
  '1c000000-0000-0000-0000-00000000001c', 'AUG1', '500', 'SEMANAL', 'cli:c:plan-1'), 'row-level security');
SELECT pg_temp.debe_fallar('un plan se paga con la bóveda del propio cliente', pg_temp.alta(
  '9a000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a',
  '1b000000-0000-0000-0000-00000000001b', 'AUG1', '500', 'SEMANAL', 'cli:a:plan-ajena'), 'SC_PLAN_INCOHERENTE');
SELECT pg_temp.debe_fallar('solo semanal, quincenal o mensual', pg_temp.alta(
  '9a000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a',
  '1a000000-0000-0000-0000-00000000001a', 'AUG1', '500', 'DIARIO', 'cli:a:plan-diario'), 'planes_ahorro_frecuencia_check');
SELECT pg_temp.debe_fallar('el monto es de pesos con centavos', pg_temp.alta(
  '9a000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a',
  '1a000000-0000-0000-0000-00000000001a', 'AUG1', '500.005', 'SEMANAL', 'cli:a:plan-fraccion'), 'planes_ahorro_monto_check');
SELECT pg_temp.debe_fallar('y compra al menos un incremento al precio de hoy (0.10 MXN de oro no alcanzan)', pg_temp.alta(
  '9a000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a',
  '1a000000-0000-0000-0000-00000000001a', 'AUG1', '0.10', 'SEMANAL', 'cli:a:plan-poco'), 'SC_MONTO_INSUFICIENTE');
SELECT pg_temp.debe_fallar('sobre un activo que opera', ARRAY[
  $q$UPDATE mercado.activos SET operable = FALSE WHERE ticker = 'AGT1'$q$] || pg_temp.alta(
  '9a000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a',
  '1a000000-0000-0000-0000-00000000001a', 'AGT1', '500', 'MENSUAL', 'cli:a:plan-cerrado'), 'SC_MERCADO_CERRADO');
SELECT pg_temp.debe_fallar('con la bóveda congelada por un operador, no', ARRAY[
  $q$UPDATE ifpe.cuentas SET estado = 'CONGELADA' WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$] || pg_temp.alta(
  '9a000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a',
  '1a000000-0000-0000-0000-00000000001a', 'AUG1', '500', 'SEMANAL', 'cli:a:plan-congelada'), 'SC_CUENTA_CONGELADA');
SELECT pg_temp.debe_fallar('ni congelada por PLD: el error constante, sin aviso', ARRAY[
  $q$UPDATE ifpe.cuentas SET estado = 'CONGELADO_PREVENTIVO' WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$]
  || pg_temp.alta(
  '9a000000-0000-0000-0000-000000000009', 'a0000000-0000-0000-0000-00000000000a',
  '1a000000-0000-0000-0000-00000000001a', 'AUG1', '500', 'SEMANAL', 'cli:a:plan-pld'), 'SC_OPERACION_NO_DISPONIBLE');
SELECT pg_temp.debe_pasar('Beto da de alta 20 planes, el máximo', (
  SELECT array_agg(s) FROM (
    SELECT unnest(pg_temp.alta(format('9b000000-0000-0000-0000-%s', lpad(i::text, 12, '0')),
                               'b0000000-0000-0000-0000-00000000000b', '1b000000-0000-0000-0000-00000000001b',
                               'CETES28', '10', 'MENSUAL', 'cli:b:plan-' || i)) AS s
      FROM generate_series(1, 20) AS i) x));
SELECT pg_temp.debe_fallar('el plan 21 no cabe', pg_temp.alta(
  '9b000000-0000-0000-0000-000000000021', 'b0000000-0000-0000-0000-00000000000b',
  '1b000000-0000-0000-0000-00000000001b', 'CETES28', '10', 'MENSUAL', 'cli:b:plan-21'), 'SC_LIMITE_PLANES');
SELECT pg_temp.debe_pasar('Beto cancela uno (como sc_mercado)', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE mercado.planes_ahorro SET estado = 'CANCELADO' WHERE id = '9b000000-0000-0000-0000-000000000020'$q$,
  'RESET ROLE']);
SELECT pg_temp.debe_pasar('y entonces sí cabe otro: los cancelados no cuentan', pg_temp.alta(
  '9b000000-0000-0000-0000-000000000021', 'b0000000-0000-0000-0000-00000000000b',
  '1b000000-0000-0000-0000-00000000001b', 'CETES28', '10', 'MENSUAL', 'cli:b:plan-21'));

-- -----------------------------------------------------------------------------
-- 3. Las compras
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('la primera compra del plan de Ana: 500 MXN de AUG1 (0.3100 g por 499.84)', pg_temp.compra(
  '9a000000-0000-0000-0000-000000000001', 0, '0e000000-0000-0000-0000-000000000001',
  '7f000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a',
  '1a000000-0000-0000-0000-00000000001a', 'AUG1', '500', '0.31', '1612.40', '499.84'));
SELECT pg_temp.exigir('registrada: la siguiente es la del periodo 1, en una semana a las 9:00', (
  SELECT periodo = 1 AND ultimo_resultado = 'EJECUTADA' AND motivo IS NULL
     AND ultima_orden_id = '0e000000-0000-0000-0000-000000000001' AND ultima_ejecucion_en = now()
     AND proxima_ejecucion_en = mercado.momento_plan('SEMANAL', fecha_ancla, 1) AND proxima_ejecucion_en > now()
    FROM mercado.planes_ahorro WHERE id = '9a000000-0000-0000-0000-000000000001'));
SELECT pg_temp.exigir('su evento EJECUTADO: el periodo 0, para cuándo estaba, su orden y la fecha siguiente', (
  SELECT count(*) = 1 AND bool_and(periodo = 0 AND programado_para = now()
                                   AND orden_id = '0e000000-0000-0000-0000-000000000001' AND motivo IS NULL
                                   AND monto = 500 AND proxima_ejecucion_en > now())
    FROM mercado.eventos_plan WHERE plan_id = '9a000000-0000-0000-0000-000000000001' AND evento = 'EJECUTADO'));
SELECT pg_temp.exigir('la orden es una compra de mercado más, en el ledger encadenado y con su posición', (
  SELECT clave_idempotencia = 'plan:9a000000-0000-0000-0000-000000000001:0' AND lado = 'COMPRA' AND cantidad = 0.31
    FROM mercado.ordenes WHERE id = '0e000000-0000-0000-0000-000000000001')
  AND (SELECT cantidad = 0.31 FROM mercado.posiciones
        WHERE cliente_id = 'a0000000-0000-0000-0000-00000000000a' AND ticker = 'AUG1'));
SELECT pg_temp.debe_fallar('una compra que aún no vence no se registra', pg_temp.omision(
  '9a000000-0000-0000-0000-000000000001', 'SALDO_INSUFICIENTE'), 'SC_PLAN_NO_VENCIDO');
SELECT pg_temp.debe_pasar('(llega la segunda compra)', pg_temp.vencer('9a000000-0000-0000-0000-000000000001'));
SELECT pg_temp.debe_fallar('ni con la orden de otro periodo', pg_temp.compra(
  '9a000000-0000-0000-0000-000000000001', 0, '0e000000-0000-0000-0000-000000000002',
  '7f000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a',
  '1a000000-0000-0000-0000-00000000001a', 'AUG1', '500', '0.31', '1612.40', '499.84'), 'clave_idempotencia');
SELECT pg_temp.debe_fallar('ni con una orden de otros términos', pg_temp.compra(
  '9a000000-0000-0000-0000-000000000001', 1, '0e000000-0000-0000-0000-000000000002',
  '7f000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a',
  '1a000000-0000-0000-0000-00000000001a', 'AUG1', '400', '0.248', '1612.40', '399.88'), 'SC_PLAN_INCOHERENTE');
SELECT pg_temp.debe_fallar_en('una orden con clave de plan no existe sin su registro', pg_temp.compra(
  '9a000000-0000-0000-0000-000000000001', 1, '0e000000-0000-0000-0000-000000000002',
  '7f000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a',
  '1a000000-0000-0000-0000-00000000001a', 'AUG1', '500', '0.31', '1612.40', '499.84', FALSE),
  'mercado.trg_ordenes_de_plan', 'SC_PLAN_SIN_EJECUCION');
SELECT pg_temp.debe_fallar('una compra omitida lleva su motivo', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE mercado.planes_ahorro SET ultimo_resultado = 'OMITIDA', motivo = NULL
      WHERE id = '9a000000-0000-0000-0000-000000000001'$q$], 'SC_PLAN_INCOHERENTE');
SELECT pg_temp.debe_fallar('y un motivo es un código, no texto libre', pg_temp.omision(
  '9a000000-0000-0000-0000-000000000001', 'no había dinero'), 'planes_ahorro_motivo_check');
SELECT pg_temp.debe_fallar('una hecha no lleva motivo', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE mercado.planes_ahorro SET ultimo_resultado = 'EJECUTADA', motivo = 'SALDO_INSUFICIENTE',
         ultima_orden_id = '0e000000-0000-0000-0000-000000000001'
      WHERE id = '9a000000-0000-0000-0000-000000000001'$q$], 'SC_PLAN_INCOHERENTE');
SELECT pg_temp.debe_pasar('la segunda se omite: no había efectivo', pg_temp.omision(
  '9a000000-0000-0000-0000-000000000001', 'SALDO_INSUFICIENTE'));
SELECT pg_temp.exigir('el plan sigue activo, con la tercera programada, y la omisión en su bitácora', (
  SELECT estado = 'ACTIVO' AND periodo = 2 AND ultimo_resultado = 'OMITIDA' AND motivo = 'SALDO_INSUFICIENTE'
     AND ultima_orden_id IS NULL AND proxima_ejecucion_en = mercado.momento_plan('SEMANAL', fecha_ancla, 2)
    FROM mercado.planes_ahorro WHERE id = '9a000000-0000-0000-0000-000000000001')
  AND (SELECT count(*) = 1 FROM mercado.eventos_plan
        WHERE plan_id = '9a000000-0000-0000-0000-000000000001' AND evento = 'OMITIDO' AND periodo = 1
          AND motivo = 'SALDO_INSUFICIENTE' AND orden_id IS NULL));
-- Un plan anclado hace 22 días (como si llevara tiempo): el dueño del esquema
-- lo crea sin sus triggers, con la segunda compra pendiente desde hace dos semanas.
SELECT pg_temp.debe_pasar('(un plan semanal anclado hace 22 días, con la compra 1 sin procesar)', ARRAY[
  'ALTER TABLE mercado.planes_ahorro DISABLE TRIGGER USER',
  $q$INSERT INTO mercado.planes_ahorro (id, cliente_id, cuenta_ifpe_id, ticker, monto, frecuencia, fecha_ancla, periodo,
                                        proxima_ejecucion_en, clave_idempotencia)
     VALUES ('9a000000-0000-0000-0000-000000000003', 'a0000000-0000-0000-0000-00000000000a',
             '1a000000-0000-0000-0000-00000000001a', 'AUG1', 500, 'SEMANAL', current_date - 22, 1,
             mercado.momento_plan('SEMANAL', current_date - 22, 1), 'cli:a:plan-viejo')$q$,
  'ALTER TABLE mercado.planes_ahorro ENABLE TRIGGER USER']);
SELECT pg_temp.debe_pasar('se procesa la compra vencida (omitida: el mercado estaba cerrado)', pg_temp.omision(
  '9a000000-0000-0000-0000-000000000003', 'MERCADO_CERRADO'));
SELECT pg_temp.exigir('las perdidas no se reponen: la siguiente es la 4, que aún no llega', (
  SELECT periodo = 4 AND proxima_ejecucion_en = mercado.momento_plan('SEMANAL', current_date - 22, 4)
     AND proxima_ejecucion_en > now()
    FROM mercado.planes_ahorro WHERE id = '9a000000-0000-0000-0000-000000000003')
  AND (SELECT count(*) = 1 FROM mercado.eventos_plan
        WHERE plan_id = '9a000000-0000-0000-0000-000000000003' AND evento = 'OMITIDO' AND periodo = 1));

-- -----------------------------------------------------------------------------
-- 4. Pausar, reanudar, cancelar
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Ana pausa su plan semanal', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE mercado.planes_ahorro SET estado = 'PAUSADO' WHERE id = '9a000000-0000-0000-0000-000000000001'$q$,
  'RESET ROLE']);
SELECT pg_temp.exigir('pausado no tiene compra programada; su evento PAUSADO', (
  SELECT estado = 'PAUSADO' AND proxima_ejecucion_en IS NULL AND periodo = 2
    FROM mercado.planes_ahorro WHERE id = '9a000000-0000-0000-0000-000000000001')
  AND (SELECT count(*) = 1 FROM mercado.eventos_plan
        WHERE plan_id = '9a000000-0000-0000-0000-000000000001' AND evento = 'PAUSADO'
          AND proxima_ejecucion_en IS NULL));
SELECT pg_temp.debe_fallar('un plan pausado no compra', pg_temp.omision(
  '9a000000-0000-0000-0000-000000000001', 'SALDO_INSUFICIENTE'), 'SC_PLAN_NO_ACTIVO');
SELECT pg_temp.debe_pasar('Ana lo reanuda', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE mercado.planes_ahorro SET estado = 'ACTIVO' WHERE id = '9a000000-0000-0000-0000-000000000001'$q$,
  'RESET ROLE']);
SELECT pg_temp.exigir('reanudado, sigue en la primera compra que aún no llega; su evento REANUDADO', (
  SELECT estado = 'ACTIVO' AND periodo = 2 AND proxima_ejecucion_en = mercado.momento_plan('SEMANAL', fecha_ancla, 2)
    FROM mercado.planes_ahorro WHERE id = '9a000000-0000-0000-0000-000000000001')
  AND (SELECT count(*) = 1 FROM mercado.eventos_plan
        WHERE plan_id = '9a000000-0000-0000-0000-000000000001' AND evento = 'REANUDADO'
          AND proxima_ejecucion_en > now()));
SELECT pg_temp.debe_pasar('las fechas son de la base de datos: lo que el dueño del esquema escriba se ignora', ARRAY[
  $q$UPDATE mercado.planes_ahorro SET proxima_ejecucion_en = now() + interval '1 year', periodo = 99
      WHERE id = '9a000000-0000-0000-0000-000000000001'$q$]);
SELECT pg_temp.exigir('(la compra siguiente no cambió)', (
  SELECT periodo = 2 AND proxima_ejecucion_en = mercado.momento_plan('SEMANAL', fecha_ancla, 2)
    FROM mercado.planes_ahorro WHERE id = '9a000000-0000-0000-0000-000000000001'));
SELECT pg_temp.debe_fallar('los términos no cambian (ni para el dueño del esquema)', ARRAY[
  $q$UPDATE mercado.planes_ahorro SET monto = 600 WHERE id = '9a000000-0000-0000-0000-000000000001'$q$],
  'SC_PLAN_INMUTABLE');
SELECT pg_temp.debe_fallar('sc_mercado no tiene permiso para cambiarlos', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE mercado.planes_ahorro SET frecuencia = 'MENSUAL' WHERE id = '9a000000-0000-0000-0000-000000000001'$q$],
  'permission denied');
SELECT pg_temp.debe_pasar('Ana cancela su plan', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE mercado.planes_ahorro SET estado = 'CANCELADO' WHERE id = '9a000000-0000-0000-0000-000000000001'$q$,
  'RESET ROLE']);
SELECT pg_temp.exigir('cancelado: sin compra programada, con su fecha y su evento CANCELADO', (
  SELECT estado = 'CANCELADO' AND proxima_ejecucion_en IS NULL AND cancelado_en = now()
    FROM mercado.planes_ahorro WHERE id = '9a000000-0000-0000-0000-000000000001')
  AND (SELECT count(*) = 1 FROM mercado.eventos_plan
        WHERE plan_id = '9a000000-0000-0000-0000-000000000001' AND evento = 'CANCELADO'));
SELECT pg_temp.debe_fallar('cancelar es definitivo: no se reanuda', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$UPDATE mercado.planes_ahorro SET estado = 'ACTIVO' WHERE id = '9a000000-0000-0000-0000-000000000001'$q$],
  'SC_PLAN_CANCELADO');
SELECT pg_temp.debe_fallar('ni compra', pg_temp.omision('9a000000-0000-0000-0000-000000000001', 'SALDO_INSUFICIENTE'),
  'SC_PLAN_CANCELADO');
SELECT pg_temp.exigir('la bitácora completa de Ana, en orden', (
  SELECT array_agg(evento ORDER BY id) = ARRAY['CREADO', 'EJECUTADO', 'OMITIDO', 'PAUSADO', 'REANUDADO', 'CANCELADO']
    FROM mercado.eventos_plan WHERE plan_id = '9a000000-0000-0000-0000-000000000001'));
SELECT pg_temp.exigir('cada evento avisó a la bóveda: los 7 de Ana (6 de este plan y la omisión del otro)', (
  pg_temp.version('1a000000-0000-0000-0000-00000000001a') = 7));

-- -----------------------------------------------------------------------------
-- 5. Inmutables, privilegios y RLS
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('un evento no se corrige', ARRAY[
  $q$UPDATE mercado.eventos_plan SET motivo = 'OTRO' WHERE evento = 'OMITIDO'$q$], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('ni se borra', ARRAY[$q$DELETE FROM mercado.eventos_plan$q$], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('un plan no se borra', ARRAY[$q$DELETE FROM mercado.planes_ahorro$q$], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('ni se vacía la tabla', ARRAY['TRUNCATE mercado.planes_ahorro CASCADE'], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('sc_mercado no escribe en la bitácora: la escriben los triggers', ARRAY[
  'SET LOCAL ROLE sc_mercado',
  $q$INSERT INTO mercado.eventos_plan (plan_id, cliente_id, evento, monto)
     VALUES ('9a000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a', 'CREADO', 500)$q$],
  'permission denied');
SELECT pg_temp.exigir('sc_mercado: alta de los términos y cambio de estado y de resultado; nada más', (
  SELECT array_agg(column_name::text ORDER BY column_name::text COLLATE "C")
         = ARRAY['clave_idempotencia', 'cliente_id', 'cuenta_ifpe_id', 'frecuencia', 'id', 'monto', 'ticker']
    FROM information_schema.column_privileges
   WHERE grantee = 'sc_mercado' AND table_schema = 'mercado' AND table_name = 'planes_ahorro'
     AND privilege_type = 'INSERT')
  AND (SELECT array_agg(column_name::text ORDER BY column_name::text COLLATE "C")
         = ARRAY['estado', 'motivo', 'ultima_orden_id', 'ultimo_resultado']
    FROM information_schema.column_privileges
   WHERE grantee = 'sc_mercado' AND table_schema = 'mercado' AND table_name = 'planes_ahorro'
     AND privilege_type = 'UPDATE')
  AND NOT has_table_privilege('sc_mercado', 'mercado.planes_ahorro', 'DELETE')
  AND NOT has_table_privilege('sc_mercado', 'mercado.eventos_plan', 'INSERT'));
SELECT pg_temp.exigir('el cliente solo lee; ningún otro rol de la aplicación toca los planes', (
  SELECT bool_and(privilege_type = 'SELECT') FROM information_schema.role_table_grants
   WHERE grantee = 'sc_cliente' AND table_schema = 'mercado' AND table_name IN ('planes_ahorro', 'eventos_plan'))
  AND (SELECT bool_and(NOT has_any_column_privilege(r, t, 'SELECT, INSERT, UPDATE, REFERENCES'))
         FROM unnest(ARRAY['sc_ifpe', 'sc_sofom', 'sc_puente', 'sc_spei', 'sc_aml', 'sc_auditor', 'sc_auditor_pld',
                           'sc_kyc', 'sc_kyc_verificador', 'sc_auth']) AS r,
              unnest(ARRAY['mercado.planes_ahorro', 'mercado.eventos_plan']) AS t));
SELECT pg_temp.exigir('RLS en las dos tablas, con la política RESTRICTIVE de sc_cliente', (
  SELECT count(*) = 2 FROM pg_class
   WHERE relrowsecurity AND oid IN ('mercado.planes_ahorro'::regclass, 'mercado.eventos_plan'::regclass))
  AND (SELECT count(*) = 2 FROM pg_policies
        WHERE schemaname = 'mercado' AND tablename IN ('planes_ahorro', 'eventos_plan')
          AND policyname = 'solo_del_cliente' AND permissive = 'RESTRICTIVE' AND roles = '{sc_cliente}'));
SELECT pg_temp.exigir('Ana ve sus 2 planes y su bitácora; Beto, sus 21; sin cliente, nada', (
  pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM mercado.planes_ahorro') = 2
  AND pg_temp.visibles('a0000000-0000-0000-0000-00000000000a', 'SELECT count(*) FROM mercado.eventos_plan') = 7
  AND pg_temp.visibles('b0000000-0000-0000-0000-00000000000b', 'SELECT count(*) FROM mercado.planes_ahorro') = 21
  AND pg_temp.visibles('', 'SELECT count(*) FROM mercado.planes_ahorro') = 0
  AND pg_temp.visibles('', 'SELECT count(*) FROM mercado.eventos_plan') = 0));

\o
\echo 'tests/db/0023_planes_ahorro.sql: todas las pruebas pasaron (ROLLBACK)'
ROLLBACK;
