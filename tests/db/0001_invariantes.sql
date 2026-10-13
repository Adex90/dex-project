-- =============================================================================
--  Pruebas de invariantes del esquema 0001 (se ejecutan dentro de una
--  transacción que termina en ROLLBACK: no dejan datos).
--
--    psql "$DATABASE_URL" -f tests/db/0001_invariantes.sql
--
--  Cada intento de violar una regla de negocio debe ser rechazado por la base
--  de datos, sin importar lo que haga el backend.
-- =============================================================================
\set ON_ERROR_STOP on
\set QUIET on
\o /dev/null
SET client_min_messages = notice;

BEGIN;

-- Ejecuta sentencias (más SET CONSTRAINTS ALL IMMEDIATE para disparar los
-- triggers diferidos) y exige que fallen con un mensaje que contenga `patron`.
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

-- Ejecuta sentencias y exige que pasen todas las verificaciones (incluidas las diferidas).
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

-- -----------------------------------------------------------------------------
-- Datos base (como dueño del esquema)
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, curp, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ludwig', 'von Mises', 'ludwig@example.mx', 'MISL810929HDFSSD09', 'VERIFICADO', now(), 'BAJO'),
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Murray', 'Rothbard',  'murray@example.mx', 'ROTM260302HDFTHR02', 'VERIFICADO', now(), 'BAJO'),
  ('c0000000-0000-0000-0000-00000000000c', 'MORAL',  'Taller Hayek SA de CV', NULL, 'credito@hayek.mx', NULL, 'VERIFICADO', now(), 'MEDIO');

INSERT INTO ifpe.reserva_custodia (id, institucion, tipo, numero_cuenta) VALUES
  ('e0000000-0000-0000-0000-00000000000e', 'Banco de México', 'BANXICO', 'SC-RES-001');
INSERT INTO ifpe.cuentas (id, cliente_id, clabe) VALUES
  ('1a000000-0000-0000-0000-00000000001a', 'a0000000-0000-0000-0000-00000000000a', '646180157000000017'),
  ('1b000000-0000-0000-0000-00000000001b', 'b0000000-0000-0000-0000-00000000000b', '646180157000000020');
INSERT INTO sofom.caja (id, institucion, numero_cuenta) VALUES
  ('f0000000-0000-0000-0000-00000000000f', 'Banco Custodio SOFOM', 'SC-SOFOM-001');
INSERT INTO sofom.cuentas (id, cliente_id) VALUES
  ('2a000000-0000-0000-0000-00000000002a', 'a0000000-0000-0000-0000-00000000000a');

-- -----------------------------------------------------------------------------
-- REGLA 1 — IFPE reserva 100%
-- -----------------------------------------------------------------------------
-- Desde 0005 el efectivo solo entra como SPEI registrado (rol sc_spei).
SELECT pg_temp.debe_pasar('IFPE: depósito de 1000 respaldado por reserva', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 1000 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 1000 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
     VALUES ('7d000000-0000-0000-0000-000000000001', 'DEPOSITO', '1a000000-0000-0000-0000-00000000001a',
             'e0000000-0000-0000-0000-00000000000e', 1000, 'spei:40012:20261001:SPEI0001', '40012:20261001:SPEI0001')$q$,
  $q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                    estado, transaccion_id, origen, raw_payload)
     VALUES ('40012', 'SPEI0001', '2026-10-01', '646180157000000017', '1a000000-0000-0000-0000-00000000001a', 1000,
             'ABONADO', '7d000000-0000-0000-0000-000000000001', 'WEBHOOK', '{}')$q$,
  'RESET ROLE'
]);

SELECT pg_temp.debe_fallar('IFPE: crear saldo sin reserva (reserva fraccionaria)', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 500 WHERE id = '1b000000-0000-0000-0000-00000000001b'$q$
], 'SC_SALDO_SIN_LEDGER');

-- Segunda línea de defensa: aun sin la liga saldo-ledger (desactivada solo
-- aquí, como dueño), el agregado SC_RESERVA_100 rechaza la reserva fraccionaria.
SELECT pg_temp.debe_fallar('IFPE: reserva fraccionaria con la liga saldo-ledger desactivada', ARRAY[
  'ALTER TABLE ifpe.cuentas DISABLE TRIGGER trg_ifpe_cuentas_saldo_ledger',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 500 WHERE id = '1b000000-0000-0000-0000-00000000001b'$q$
], 'SC_RESERVA_100');

SELECT pg_temp.debe_fallar('IFPE: retirar reserva dejando depósitos sin respaldo', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo - 1 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$
], 'SC_SALDO_SIN_LEDGER');

SELECT pg_temp.debe_fallar('IFPE: acuñar saldo y reserva cuadrados sin movimiento en el ledger', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 5000000 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 5000000 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$
], 'SC_SALDO_SIN_LEDGER');

SELECT pg_temp.debe_fallar('IFPE: sobregiro (saldo_disponible negativo)', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 5000 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 5000 WHERE id = '1b000000-0000-0000-0000-00000000001b'$q$
], 'ck_ifpe_cuentas_saldo_disponible_no_negativo');

SELECT pg_temp.debe_fallar('IFPE: reserva en instrumentos no líquidos', ARRAY[
  $q$INSERT INTO ifpe.reserva_custodia (institucion, tipo, numero_cuenta) VALUES ('Casa de Bolsa', 'BONOS_GUBERNAMENTALES', 'X')$q$
], 'reserva_custodia_tipo_check');

SELECT pg_temp.debe_fallar('IFPE: CLABE con dígito verificador inválido', ARRAY[
  $q$INSERT INTO ifpe.cuentas (cliente_id, clabe) VALUES ('c0000000-0000-0000-0000-00000000000c', '646180157000000018')$q$
], 'cuentas_clabe_check');

SELECT pg_temp.debe_pasar('IFPE: transferencia A -> B de 100 (reserva intacta)', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 100 WHERE id = '1b000000-0000-0000-0000-00000000001b'$q$,
  $q$INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia)
     VALUES ('TRANSFERENCIA', '1a000000-0000-0000-0000-00000000001a', '1b000000-0000-0000-0000-00000000001b', 100, 'trf-1')$q$,
  'RESET ROLE'
]);

-- -----------------------------------------------------------------------------
-- REGLA 4 — Ledger inmutable y encadenado
-- -----------------------------------------------------------------------------
DO $$
DECLARE
    t1 ifpe.transacciones;
    t2 ifpe.transacciones;
BEGIN
    SELECT * INTO t1 FROM ifpe.transacciones WHERE secuencia = 1;
    SELECT * INTO t2 FROM ifpe.transacciones WHERE secuencia = 2;
    ASSERT t1.hash_anterior = nucleo.hash_genesis(), 'la primera tx debe encadenar al hash genesis';
    ASSERT t1.monto::text = '1000.00000000', 'monto canónico con 8 decimales';
    ASSERT t1.tx_hash = nucleo.sha256_hex(ifpe.payload_canonico(t1)), 'tx_hash debe ser SHA-256 del payload canónico';
    ASSERT t2.hash_anterior = t1.tx_hash, 'la cadena debe enlazar tx2 -> tx1';
    ASSERT (SELECT ultimo_hash FROM ifpe.ledger_cabeza) = t2.tx_hash, 'la cabeza apunta al último hash';
    RAISE NOTICE 'OK  cadena     payload tx1 = %', ifpe.payload_canonico(t1);
END
$$;

SELECT pg_temp.debe_fallar('Ledger: UPDATE de un movimiento (incluso como dueño)', ARRAY[
  $q$UPDATE ifpe.transacciones SET monto = 1 WHERE secuencia = 1$q$
], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('Ledger: DELETE de un movimiento', ARRAY[
  $q$DELETE FROM ifpe.transacciones WHERE secuencia = 1$q$
], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('Ledger: TRUNCATE', ARRAY[
  'TRUNCATE ifpe.transacciones CASCADE'
], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('Ledger: forma inválida (depósito sin reserva)', ARRAY[
  $q$INSERT INTO ifpe.transacciones (tipo, cuenta_destino_id, monto, clave_idempotencia)
     VALUES ('DEPOSITO', '1a000000-0000-0000-0000-00000000001a', 1, 'dep-x')$q$
], 'ck_ifpe_tx_forma');
SELECT pg_temp.debe_fallar('Ledger: clave de idempotencia repetida', ARRAY[
  $q$INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia)
     VALUES ('TRANSFERENCIA', '1b000000-0000-0000-0000-00000000001b', '1a000000-0000-0000-0000-00000000001a', 1, 'trf-1')$q$
], 'transacciones_clave_idempotencia_key');

-- -----------------------------------------------------------------------------
-- REGLA 3 — Aislamiento IFPE / SOFOM por rol
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('Aislamiento: sc_ifpe no puede leer la SOFOM', ARRAY[
  'SET LOCAL ROLE sc_ifpe', 'SELECT count(*) FROM sofom.contratos_mutuo'
], 'permission denied for schema sofom');
SELECT pg_temp.debe_fallar('Aislamiento: sc_sofom no puede leer la IFPE', ARRAY[
  'SET LOCAL ROLE sc_sofom', 'SELECT count(*) FROM ifpe.cuentas'
], 'permission denied for schema ifpe');
SELECT pg_temp.debe_fallar('Aislamiento: el puente no puede originar créditos', ARRAY[
  'SET LOCAL ROLE sc_puente',
  $q$INSERT INTO sofom.creditos (acreditado_id, principal, tasa_anual, plazo_dias, fecha_desembolso)
     VALUES ('c0000000-0000-0000-0000-00000000000c', 1, 0.1, 10, current_date)$q$
], 'permission denied for table creditos');
SELECT pg_temp.debe_fallar('Aislamiento: ningún rol modifica estado_reserva directamente', ARRAY[
  'SET LOCAL ROLE sc_ifpe', 'UPDATE ifpe.estado_reserva SET activo_reserva = 0'
], 'permission denied for table estado_reserva');

-- -----------------------------------------------------------------------------
-- Puente IFPE -> SOFOM: dos contratos de mutuo de A (600 a 180 días, 300 a 30 días)
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Puente: mutuo #1 de 600 a 180 días', ARRAY[
  'SET LOCAL ROLE sc_puente',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 600 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo - 600 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia, ref_movimiento_espejo)
     VALUES ('9a000000-0000-0000-0000-000000000001', 'TRASPASO_A_SOFOM', '1a000000-0000-0000-0000-00000000001a',
             'e0000000-0000-0000-0000-00000000000e', 600, 'mutuo-1', '9b000000-0000-0000-0000-000000000001')$q$,
  $q$UPDATE sofom.cuentas SET saldo_bloqueado = saldo_bloqueado + 600 WHERE id = '2a000000-0000-0000-0000-00000000002a'$q$,
  $q$UPDATE sofom.caja SET saldo = saldo + 600 WHERE id = 'f0000000-0000-0000-0000-00000000000f'$q$,
  $q$INSERT INTO sofom.contratos_mutuo (id, cliente_id, cuenta_sofom_id, cuenta_ifpe_origen_id, tx_aportacion_ifpe_id,
                                       principal, tasa_anual, plazo_dias, fecha_inicio, hash_terminos)
     VALUES ('3a000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a', '2a000000-0000-0000-0000-00000000002a',
             '1a000000-0000-0000-0000-00000000001a', '9a000000-0000-0000-0000-000000000001',
             600, 0.08, 180, current_date, repeat('a', 64))$q$,
  $q$INSERT INTO sofom.transacciones (id, tipo, cuenta_id, contrato_id, caja_id, monto, clave_idempotencia, ref_movimiento_espejo)
     VALUES ('9b000000-0000-0000-0000-000000000001', 'APORTACION_MUTUO', '2a000000-0000-0000-0000-00000000002a',
             '3a000000-0000-0000-0000-000000000001', 'f0000000-0000-0000-0000-00000000000f', 600, 'mutuo-1',
             '9a000000-0000-0000-0000-000000000001')$q$,
  'RESET ROLE'
]);

SELECT pg_temp.debe_pasar('Puente: mutuo #2 de 300 a 30 días', ARRAY[
  'SET LOCAL ROLE sc_puente',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 300 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo - 300 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia, ref_movimiento_espejo)
     VALUES ('9a000000-0000-0000-0000-000000000002', 'TRASPASO_A_SOFOM', '1a000000-0000-0000-0000-00000000001a',
             'e0000000-0000-0000-0000-00000000000e', 300, 'mutuo-2', '9b000000-0000-0000-0000-000000000002')$q$,
  $q$UPDATE sofom.cuentas SET saldo_bloqueado = saldo_bloqueado + 300 WHERE id = '2a000000-0000-0000-0000-00000000002a'$q$,
  $q$UPDATE sofom.caja SET saldo = saldo + 300 WHERE id = 'f0000000-0000-0000-0000-00000000000f'$q$,
  $q$INSERT INTO sofom.contratos_mutuo (id, cliente_id, cuenta_sofom_id, cuenta_ifpe_origen_id, tx_aportacion_ifpe_id,
                                       principal, tasa_anual, plazo_dias, fecha_inicio, hash_terminos)
     VALUES ('3a000000-0000-0000-0000-000000000002', 'a0000000-0000-0000-0000-00000000000a', '2a000000-0000-0000-0000-00000000002a',
             '1a000000-0000-0000-0000-00000000001a', '9a000000-0000-0000-0000-000000000002',
             300, 0.05, 30, current_date, repeat('b', 64))$q$,
  $q$INSERT INTO sofom.transacciones (id, tipo, cuenta_id, contrato_id, caja_id, monto, clave_idempotencia, ref_movimiento_espejo)
     VALUES ('9b000000-0000-0000-0000-000000000002', 'APORTACION_MUTUO', '2a000000-0000-0000-0000-00000000002a',
             '3a000000-0000-0000-0000-000000000002', 'f0000000-0000-0000-0000-00000000000f', 300, 'mutuo-2',
             '9a000000-0000-0000-0000-000000000002')$q$,
  'RESET ROLE'
]);

DO $$
BEGIN
    ASSERT (SELECT pasivo_clientes FROM ifpe.estado_reserva) = 100, 'pasivo IFPE = 1000 - 900';
    ASSERT (SELECT activo_reserva  FROM ifpe.estado_reserva) = 100, 'reserva IFPE = 1000 - 900';
    ASSERT (SELECT coeficiente_reserva FROM ifpe.estado_reserva) = 1, 'coeficiente de reserva = 1';
    RAISE NOTICE 'OK  reserva    pasivo = reserva = 100 tras el puente; coeficiente 1.0';
END
$$;

SELECT pg_temp.debe_fallar('Puente: no puede ajustar principal_asignado a mano', ARRAY[
  'SET LOCAL ROLE sc_puente',
  $q$UPDATE sofom.contratos_mutuo SET principal_asignado = 0 WHERE id = '3a000000-0000-0000-0000-000000000001'$q$
], 'permission denied for table contratos_mutuo');

-- -----------------------------------------------------------------------------
-- REGLA 2 — SOFOM: fondeo a plazo y calce de vencimientos
-- -----------------------------------------------------------------------------
INSERT INTO sofom.creditos (id, acreditado_id, principal, tasa_anual, plazo_dias, fecha_desembolso) VALUES
  ('4c000000-0000-0000-0000-000000000001', 'c0000000-0000-0000-0000-00000000000c', 500, 0.15,  90, current_date),
  ('4c000000-0000-0000-0000-000000000002', 'c0000000-0000-0000-0000-00000000000c', 100, 0.15, 365, current_date);

SELECT pg_temp.debe_fallar('Calce: mutuo de 30 días no puede fondear crédito de 90 días', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$INSERT INTO sofom.asignaciones_fondeo (credito_id, contrato_id, monto)
     VALUES ('4c000000-0000-0000-0000-000000000001', '3a000000-0000-0000-0000-000000000002', 100)$q$
], 'SC_CALCE_PLAZOS');

SELECT pg_temp.debe_fallar('Calce: mutuo de 180 días no puede fondear crédito de 365 días', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$INSERT INTO sofom.asignaciones_fondeo (credito_id, contrato_id, monto)
     VALUES ('4c000000-0000-0000-0000-000000000002', '3a000000-0000-0000-0000-000000000001', 100)$q$
], 'SC_CALCE_PLAZOS');

SELECT pg_temp.debe_fallar('Fondeo: prestar más de lo que el mutuo tiene', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$UPDATE sofom.creditos SET principal = 700 WHERE id = '4c000000-0000-0000-0000-000000000001'$q$,
  $q$INSERT INTO sofom.asignaciones_fondeo (credito_id, contrato_id, monto)
     VALUES ('4c000000-0000-0000-0000-000000000001', '3a000000-0000-0000-0000-000000000001', 700)$q$
], 'SC_SOBREASIGNACION');

SELECT pg_temp.debe_fallar('Fondeo: desembolsar un crédito fondeado parcialmente', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$INSERT INTO sofom.asignaciones_fondeo (credito_id, contrato_id, monto)
     VALUES ('4c000000-0000-0000-0000-000000000001', '3a000000-0000-0000-0000-000000000001', 400)$q$,
  $q$UPDATE sofom.creditos SET estado = 'VIGENTE', saldo_insoluto = 500 WHERE id = '4c000000-0000-0000-0000-000000000001'$q$
], 'SC_FONDEO_INCOMPLETO');

SELECT pg_temp.debe_fallar('Fondeo: crédito creado directamente VIGENTE sin mutuos', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$INSERT INTO sofom.creditos (acreditado_id, principal, tasa_anual, plazo_dias, fecha_desembolso, estado, saldo_insoluto)
     VALUES ('c0000000-0000-0000-0000-00000000000c', 50, 0.15, 30, current_date, 'VIGENTE', 50)$q$
], 'SC_FONDEO_INCOMPLETO');

SELECT pg_temp.debe_fallar('Fondeo: desembolsar más efectivo del que hay en caja SOFOM', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$UPDATE sofom.caja SET saldo = saldo - 10000 WHERE id = 'f0000000-0000-0000-0000-00000000000f'$q$
], 'ck_sofom_caja_saldo_no_negativo');

SELECT pg_temp.debe_pasar('SOFOM: crédito de 500 a 90 días fondeado por mutuo de 180 días y desembolsado', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$INSERT INTO sofom.asignaciones_fondeo (credito_id, contrato_id, monto)
     VALUES ('4c000000-0000-0000-0000-000000000001', '3a000000-0000-0000-0000-000000000001', 500)$q$,
  $q$UPDATE sofom.creditos SET estado = 'VIGENTE', saldo_insoluto = 500 WHERE id = '4c000000-0000-0000-0000-000000000001'$q$,
  $q$UPDATE sofom.caja SET saldo = saldo - 500 WHERE id = 'f0000000-0000-0000-0000-00000000000f'$q$,
  $q$INSERT INTO sofom.transacciones (tipo, credito_id, caja_id, monto, clave_idempotencia)
     VALUES ('DESEMBOLSO_CREDITO', '4c000000-0000-0000-0000-000000000001', 'f0000000-0000-0000-0000-00000000000f', 500, 'desembolso-1')$q$,
  'RESET ROLE'
]);

DO $$
BEGIN
    ASSERT (SELECT principal_asignado FROM sofom.contratos_mutuo WHERE id = '3a000000-0000-0000-0000-000000000001') = 500;
    RAISE NOTICE 'OK  fondeo     mutuo #1 tiene 500 de 600 comprometidos';
END
$$;

SELECT pg_temp.debe_fallar('Mutuo: retiro anticipado (VIGENTE -> VENCIDO antes de plazo)', ARRAY[
  'SET LOCAL ROLE sc_puente',
  $q$UPDATE sofom.contratos_mutuo SET estado = 'VENCIDO' WHERE id = '3a000000-0000-0000-0000-000000000001'$q$
], 'SC_MUTUO_RETIRO_ANTICIPADO');
SELECT pg_temp.debe_fallar('Mutuo: acortar el plazo (incluso como dueño)', ARRAY[
  $q$UPDATE sofom.contratos_mutuo SET plazo_dias = 30 WHERE id = '3a000000-0000-0000-0000-000000000001'$q$
], 'SC_MUTUO_INMUTABLE');
SELECT pg_temp.debe_fallar('Mutuo: borrar un contrato', ARRAY[
  $q$DELETE FROM sofom.contratos_mutuo WHERE id = '3a000000-0000-0000-0000-000000000002'$q$
], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('Crédito: extender plazo tras el desembolso', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$UPDATE sofom.creditos SET plazo_dias = 400 WHERE id = '4c000000-0000-0000-0000-000000000001'$q$
], 'SC_CREDITO_INMUTABLE');
SELECT pg_temp.debe_fallar('Crédito: reprogramar en fondeo más allá del vencimiento del mutuo', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$UPDATE sofom.creditos SET plazo_dias = 100 WHERE id = '4c000000-0000-0000-0000-000000000002'$q$,
  $q$INSERT INTO sofom.asignaciones_fondeo (credito_id, contrato_id, monto)
     VALUES ('4c000000-0000-0000-0000-000000000002', '3a000000-0000-0000-0000-000000000001', 100)$q$,
  $q$UPDATE sofom.creditos SET plazo_dias = 200 WHERE id = '4c000000-0000-0000-0000-000000000002'$q$
], 'SC_CALCE_PLAZOS');
SELECT pg_temp.debe_fallar('Asignación: no se puede quitar fondeo a un crédito desembolsado', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$DELETE FROM sofom.asignaciones_fondeo WHERE credito_id = '4c000000-0000-0000-0000-000000000001'$q$
], 'SC_ASIGNACION_INMUTABLE');

SELECT pg_temp.debe_pasar('SOFOM: cobro total y liquidación libera el fondeo del mutuo', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$UPDATE sofom.caja SET saldo = saldo + 500 WHERE id = 'f0000000-0000-0000-0000-00000000000f'$q$,
  $q$INSERT INTO sofom.transacciones (id, tipo, credito_id, caja_id, monto, clave_idempotencia)
     VALUES ('9c000000-0000-0000-0000-000000000001', 'COBRO_CREDITO', '4c000000-0000-0000-0000-000000000001',
             'f0000000-0000-0000-0000-00000000000f', 500, 'cobro-1')$q$,
  $q$INSERT INTO sofom.cobros (transaccion_id, credito_id, principal, interes)
     VALUES ('9c000000-0000-0000-0000-000000000001', '4c000000-0000-0000-0000-000000000001', 500, 0)$q$,
  $q$UPDATE sofom.creditos SET saldo_insoluto = 0, estado = 'LIQUIDADO' WHERE id = '4c000000-0000-0000-0000-000000000001'$q$,
  'RESET ROLE'
]);

DO $$
BEGIN
    ASSERT (SELECT principal_asignado FROM sofom.contratos_mutuo WHERE id = '3a000000-0000-0000-0000-000000000001') = 0;
    ASSERT (SELECT bool_and(liberada) FROM sofom.asignaciones_fondeo WHERE credito_id = '4c000000-0000-0000-0000-000000000001');
    ASSERT (SELECT t.tx_hash = nucleo.sha256_hex(sofom.payload_canonico(t)) FROM sofom.transacciones t WHERE secuencia = 4);
    RAISE NOTICE 'OK  liquidación principal_asignado regresa a 0; ledger SOFOM encadenado';
END
$$;

\echo
\echo '>>> Todas las invariantes del esquema 0001 se cumplen.'
ROLLBACK;
