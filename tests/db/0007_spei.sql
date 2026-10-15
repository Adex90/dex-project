-- =============================================================================
--  Pruebas de la migración 0007 (endurecimiento SPEI). Corren en una
--  transacción que termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0007_spei.sql
--
--  Lo que la base de datos garantiza por sí sola, sin importar el backend:
--  resoluciones del operador ligadas a su reembolso, incidentes inmutables,
--  devoluciones que nunca se abonan como depósito, pagos "distintos" solo por
--  el operador, anulación con corrección total y reemplazo de la identidad,
--  abonos LEGADO solo por la migración, intentos monótonos y la conciliación.
-- =============================================================================
\set ON_ERROR_STOP on
\set QUIET on
\o /dev/null
SET client_min_messages = notice;

BEGIN;

-- Las sentencias corren en una subtransacción (bloque EXCEPTION): sus filas
-- tienen otro xmin que las de la transacción principal.
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
-- (las ligas con el ledger exigen el mismo xmin).
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

-- Abono SPEI (DEPOSITO + registro ABONADO) como lo registra sc_spei, con la
-- identidad 40012:20261001:<rastreo>. `sufijo` es una expresión SQL que se
-- agrega a la referencia del DEPOSITO (reemplazos); `cols`/`vals`, columnas
-- adicionales del registro.
CREATE FUNCTION pg_temp.abono(tx UUID, cuenta UUID, clabe TEXT, rastreo TEXT, monto NUMERIC, origen TEXT,
                              por UUID DEFAULT NULL, sufijo TEXT DEFAULT $s$''$s$, cols TEXT DEFAULT '',
                              vals TEXT DEFAULT '')
RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
      'SET LOCAL ROLE sc_spei',
      format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + %s WHERE id = %L$q$, monto, cuenta),
      format($q$UPDATE ifpe.reserva_custodia SET saldo = saldo + %s WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
             monto),
      format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia,
                                               referencia_externa)
         VALUES (%L, 'DEPOSITO', %L, 'e0000000-0000-0000-0000-00000000000e', %s,
                 'spei:40012:20261001:' || %L || %s, '40012:20261001:' || %L || %s)$q$,
             tx, cuenta, monto, rastreo, sufijo, rastreo, sufijo),
      format($q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe,
                                              cuenta_id, monto, estado, transaccion_id, origen, registrado_por,
                                              raw_payload%s)
         VALUES ('40012', %L, '2026-10-01', %L, %L, %s, 'ABONADO', %L, %L, %L, '{}'%s)$q$,
             cols, rastreo, clabe, cuenta, monto, tx, origen, por, vals),
      'RESET ROLE'
    ]
$$;

-- Corrección (RETIRO 'spei-correccion:<id>') de un abono de Beto, como sc_spei.
CREATE FUNCTION pg_temp.correccion(id UUID, tx UUID, rastreo TEXT, monto NUMERIC) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
      'SET LOCAL ROLE sc_spei',
      format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - %s
                 WHERE id = '1b000000-0000-0000-0000-00000000001b'$q$, monto),
      format($q$UPDATE ifpe.reserva_custodia SET saldo = saldo - %s WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
             monto),
      format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia)
         VALUES (%L, 'RETIRO', '1b000000-0000-0000-0000-00000000001b', 'e0000000-0000-0000-0000-00000000000e', %s,
                 'spei-correccion:' || %L)$q$, tx, monto, id),
      format($q$INSERT INTO ifpe.spei_correcciones (id, spei_inbound_id, cuenta_id, monto, motivo, autorizado_por,
                                                   clave_idempotencia, tx_id)
         VALUES (%L, (SELECT id FROM ifpe.spei_inbound WHERE clave_rastreo = %L AND reemplaza_a IS NULL),
                 '1b000000-0000-0000-0000-00000000001b', %s, 'abonado a la cuenta equivocada',
                 '0f000000-0000-0000-0000-0000000000f0', 'op:' || %L, %L)$q$, id, rastreo, monto, id, tx),
      'RESET ROLE'
    ]
$$;

-- Orden SPEI de Ana (RETIRO + orden PENDIENTE), como la crea sc_ifpe.
CREATE FUNCTION pg_temp.orden(id UUID, tx UUID, rastreo TEXT, monto NUMERIC) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
      'SET LOCAL ROLE sc_ifpe',
      format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - %s
                 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$, monto),
      format($q$UPDATE ifpe.reserva_custodia SET saldo = saldo - %s WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
             monto),
      format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia,
                                               referencia_externa)
         VALUES (%L, 'RETIRO', '1a000000-0000-0000-0000-00000000001a', 'e0000000-0000-0000-0000-00000000000e', %s,
                 'cli:' || %L, '012180001234567899')$q$, tx, monto, rastreo),
      format($q$INSERT INTO ifpe.spei_outbound (id, cliente_id, cuenta_id, clave_idempotencia, clave_rastreo,
                 fecha_solicitud, ordenante_nombre, ordenante_clabe, banco_receptor, beneficiario_clabe,
                 beneficiario_nombre, monto, concepto, referencia_numerica, tx_retiro_id)
         VALUES (%L, 'a0000000-0000-0000-0000-00000000000a', '1a000000-0000-0000-0000-00000000001a', 'cli:' || %L, %L,
                 '2026-10-01', 'LUDWIG VON MISES', '646180157000000017', '012', '012180001234567899',
                 'BENEFICIARIO', %s, 'PAGO', 1, %L)$q$, id, rastreo, rastreo, monto, tx),
      'RESET ROLE'
    ]
$$;

-- Reembolso de una orden de Ana (reverso 'spei-reverso:<orden>' y RECHAZADA), como sc_spei.
CREATE FUNCTION pg_temp.reembolso(orden UUID, tx UUID, monto NUMERIC) RETURNS TEXT[]
LANGUAGE sql AS $$
    SELECT ARRAY[
      'SET LOCAL ROLE sc_spei',
      format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + %s
                 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$, monto),
      format($q$UPDATE ifpe.reserva_custodia SET saldo = saldo + %s WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
             monto),
      format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia)
         VALUES (%L, 'DEPOSITO', '1a000000-0000-0000-0000-00000000001a', 'e0000000-0000-0000-0000-00000000000e', %s,
                 'spei-reverso:' || %L)$q$, tx, monto, orden),
      format($q$UPDATE ifpe.spei_outbound SET estado = 'RECHAZADA', tx_reverso_id = %L, causa_devolucion = 99,
                 revision_manual = FALSE, finalizada_en = now() WHERE id = %L$q$, tx, orden),
      'RESET ROLE'
    ]
$$;

CREATE FUNCTION pg_temp.resolucion(orden UUID, por UUID, clave TEXT) RETURNS TEXT
LANGUAGE sql AS $$
    SELECT format($q$INSERT INTO ifpe.spei_resoluciones (spei_outbound_id, accion, motivo, error_previo, autorizado_por,
                                                        clave_idempotencia)
        SELECT id, 'REEMBOLSO', 'STP confirma que nunca la recibió', coalesce(ultimo_error, ''), %L, %L
          FROM ifpe.spei_outbound WHERE id = %L$q$, por, clave, orden)
$$;

-- -----------------------------------------------------------------------------
-- Datos base (como dueño del esquema)
-- -----------------------------------------------------------------------------
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, curp, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld) VALUES
  ('a0000000-0000-0000-0000-00000000000a', 'FISICA', 'Ludwig', 'von Mises', 'ludwig@example.mx', 'MISL810929HDFSSD09', 'VERIFICADO', now(), 'BAJO'),
  ('b0000000-0000-0000-0000-00000000000b', 'FISICA', 'Murray', 'Rothbard',  'murray@example.mx', 'ROTM260302HDFTHR02', 'VERIFICADO', now(), 'BAJO'),
  ('0f000000-0000-0000-0000-0000000000f0', 'FISICA', 'Operador', 'Cumplimiento', 'op@example.mx', NULL, 'VERIFICADO', now(), 'BAJO');
INSERT INTO kyc.credenciales (cliente_id, password_hash, rol) VALUES ('0f000000-0000-0000-0000-0000000000f0', 'x', 'OPERADOR');
INSERT INTO ifpe.reserva_custodia (id, institucion, tipo, numero_cuenta) VALUES
  ('e0000000-0000-0000-0000-00000000000e', 'Banco de México', 'BANXICO', 'SC-RES-001');
INSERT INTO ifpe.cuentas (id, cliente_id, clabe) VALUES
  ('1a000000-0000-0000-0000-00000000001a', 'a0000000-0000-0000-0000-00000000000a', '646180157000000017'),
  ('1b000000-0000-0000-0000-00000000001b', 'b0000000-0000-0000-0000-00000000000b', '646180157000000020');

SELECT pg_temp.debe_pasar('Base: abono SPEI de 1000 a Ana (webhook)',
  pg_temp.abono('7d000000-0000-0000-0000-000000000001', '1a000000-0000-0000-0000-00000000001a', '646180157000000017',
                'BASE0001', 1000, 'WEBHOOK'));

-- -----------------------------------------------------------------------------
-- 1. Una devolución nunca se abona como depósito nuevo (inbound-security-1)
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('Devolución: ABONADO con tipo de pago 17',
  pg_temp.abono(gen_random_uuid(), '1a000000-0000-0000-0000-00000000001a', '646180157000000017', 'DEV17', 300,
                'WEBHOOK', NULL, $s$''$s$, ', tipo_pago', ', 17'),
  'ck_spei_inbound_abonado_no_es_devolucion');
SELECT pg_temp.debe_fallar('Devolución: ABONADO que trae claveRastreoDevolucion',
  ARRAY[
    'SET LOCAL ROLE sc_spei',
    $q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, tipo_pago, receptor_clabe,
                                      cuenta_id, monto, estado, transaccion_id, origen, raw_payload)
       VALUES ('40012', 'DEVCONRASTREO', '2026-10-01', 1, '646180157000000017', '1a000000-0000-0000-0000-00000000001a',
               300, 'ABONADO', gen_random_uuid(), 'WEBHOOK', '{"claveRastreoDevolucion": " sc2026100100001 "}')$q$
  ],
  'ck_spei_inbound_abonado_no_es_devolucion');

-- -----------------------------------------------------------------------------
-- 2. Pago distinto declarado por el operador (inbound-security-2)
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('Distinto: un webhook no se declara distinto de otro abono',
  pg_temp.abono(gen_random_uuid(), '1a000000-0000-0000-0000-00000000001a', '646180157000000017', 'BASE0001X', 1000,
                'WEBHOOK', NULL, $s$''$s$, ', distinto_de_tx, motivo',
                $v$, '7d000000-0000-0000-0000-000000000001', 'nomina semanal'$v$),
  'ck_spei_inbound_distinto');
SELECT pg_temp.debe_fallar('Distinto: solo de un abono de la misma cuenta, monto y clave de rastreo',
  pg_temp.abono(gen_random_uuid(), '1a000000-0000-0000-0000-00000000001a', '646180157000000017', 'OTRORASTREO', 1000,
                'OPERADOR', '0f000000-0000-0000-0000-0000000000f0', $s$''$s$, ', distinto_de_tx, motivo',
                $v$, '7d000000-0000-0000-0000-000000000001', 'nomina semanal'$v$),
  'SC_SPEI_DISTINTO_INVALIDO');
SELECT pg_temp.debe_pasar('Distinto: captura del operador de otro pago con la misma clave de rastreo (otro día)', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 1000 WHERE id = '1a000000-0000-0000-0000-00000000001a'$q$,
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 1000 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  $q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
     VALUES ('7d000000-0000-0000-0000-000000000002', 'DEPOSITO', '1a000000-0000-0000-0000-00000000001a',
             'e0000000-0000-0000-0000-00000000000e', 1000, 'spei:40012:20261008:BASE0001', '40012:20261008:BASE0001')$q$,
  $q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                    estado, transaccion_id, origen, registrado_por, raw_payload, distinto_de_tx, motivo)
     VALUES ('40012', 'BASE0001', '2026-10-08', '646180157000000017', '1a000000-0000-0000-0000-00000000001a', 1000,
             'ABONADO', '7d000000-0000-0000-0000-000000000002', 'OPERADOR', '0f000000-0000-0000-0000-0000000000f0', '{}',
             '7d000000-0000-0000-0000-000000000001', 'nomina semanal')$q$,
  'RESET ROLE'
]);

-- Notificación retenida: siempre con su identidad.
SELECT pg_temp.debe_fallar('Retención: un posible duplicado sin identidad', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$INSERT INTO ifpe.spei_cuarentena (tipo, cuerpo, error) VALUES ('POSIBLE_DUPLICADO', 'x', 'x')$q$
], 'ck_spei_cuarentena_identidad');
SELECT pg_temp.debe_pasar('Retención: posible duplicado con identidad; un reintento idéntico no se acumula', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$INSERT INTO ifpe.spei_cuarentena (tipo, cuerpo, error, identidad)
     VALUES ('POSIBLE_DUPLICADO', 'cuerpo', 'x', '40012:20261009:RETENIDO1') ON CONFLICT DO NOTHING$q$,
  $q$INSERT INTO ifpe.spei_cuarentena (tipo, cuerpo, error, identidad)
     VALUES ('POSIBLE_DUPLICADO', 'cuerpo', 'x', '40012:20261009:RETENIDO1') ON CONFLICT DO NOTHING$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('Retención: una fila en la cola de revisión',
  (SELECT count(*) = 1 FROM ifpe.v_spei_en_revision WHERE identidad = '40012:20261009:RETENIDO1'));

-- -----------------------------------------------------------------------------
-- 3. Abonos anteriores a 0005: solo los registra la migración (inbound-security-3)
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('Legado: sc_spei no registra un abono LEGADO', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, monto, estado,
                                    causa_devolucion, origen, raw_payload)
     VALUES ('40012', 'FALSOLEGADO', '2026-10-01', '646180157000000017', 1, 'DEVUELTO', 1, 'LEGADO', '{}')$q$
], 'SC_SPEI_LEGADO');
SELECT pg_temp.debe_fallar('Legado: sc_spei no ejecuta el registro de abonos anteriores', ARRAY[
  'SET LOCAL ROLE sc_spei',
  'SELECT ifpe.registrar_abonos_legados()'
], 'permission denied');

-- -----------------------------------------------------------------------------
-- 4. Anulación de una captura errónea y reemplazo de su identidad (inbound-security-4)
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Anulación: el operador captura 500 a Beto (debía ser Ana)',
  pg_temp.abono('7d000000-0000-0000-0000-000000000010', '1b000000-0000-0000-0000-00000000001b', '646180157000000020',
                'MBAN01REAL', 500, 'OPERADOR', '0f000000-0000-0000-0000-0000000000f0'));
SELECT pg_temp.debe_fallar('Anulación: la identidad ocupada no admite otro registro',
  pg_temp.abono(gen_random_uuid(), '1a000000-0000-0000-0000-00000000001a', '646180157000000017', 'MBAN01REAL', 500,
                'WEBHOOK', NULL, $s$'/x'$s$),
  'ux_spei_inbound_identidad');
SELECT pg_temp.debe_fallar('Anulación: lo que informó el proveedor no se anula', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$INSERT INTO ifpe.spei_anulaciones (spei_inbound_id, motivo, autorizado_por, clave_idempotencia)
     SELECT id, 'no es una captura', '0f000000-0000-0000-0000-0000000000f0', 'op:anul-0' FROM ifpe.spei_inbound
      WHERE clave_rastreo = 'BASE0001' AND fecha_operacion = '2026-10-01'$q$
], 'SC_SPEI_NO_ANULABLE');
SELECT pg_temp.debe_fallar('Anulación: exige corregir el abono completo',
  pg_temp.correccion('c0000000-0000-0000-0000-0000000000c1', '7d000000-0000-0000-0000-000000000011', 'MBAN01REAL', 200)
  || ARRAY[
    'SET LOCAL ROLE sc_spei',
    $q$INSERT INTO ifpe.spei_anulaciones (spei_inbound_id, correccion_id, motivo, autorizado_por, clave_idempotencia)
       SELECT id, 'c0000000-0000-0000-0000-0000000000c1', 'abonado a la cuenta equivocada',
              '0f000000-0000-0000-0000-0000000000f0', 'op:anul-1'
         FROM ifpe.spei_inbound WHERE clave_rastreo = 'MBAN01REAL'$q$
  ],
  'exige corregir el abono completo');
SELECT pg_temp.debe_fallar('Anulación: el titular de la cuenta no anula su propio abono',
  pg_temp.correccion('c0000000-0000-0000-0000-0000000000c2', '7d000000-0000-0000-0000-000000000012', 'MBAN01REAL', 500)
  || ARRAY[
    'SET LOCAL ROLE sc_spei',
    $q$INSERT INTO ifpe.spei_anulaciones (spei_inbound_id, correccion_id, motivo, autorizado_por, clave_idempotencia)
       SELECT id, 'c0000000-0000-0000-0000-0000000000c2', 'abonado a la cuenta equivocada',
              'b0000000-0000-0000-0000-00000000000b', 'op:anul-2'
         FROM ifpe.spei_inbound WHERE clave_rastreo = 'MBAN01REAL'$q$
  ],
  'SC_SPEI_AUTORESOLUCION');
SELECT pg_temp.debe_fallar('Reemplazo: solo de un registro anulado',
  pg_temp.abono(gen_random_uuid(), '1a000000-0000-0000-0000-00000000001a', '646180157000000017', 'MBAN01REAL', 500,
                'WEBHOOK', NULL,
                $s$'/' || (SELECT id FROM ifpe.spei_inbound WHERE clave_rastreo = 'MBAN01REAL')::text$s$,
                ', reemplaza_a', $v$, (SELECT id FROM ifpe.spei_inbound WHERE clave_rastreo = 'MBAN01REAL')$v$),
  'solo puede reemplazar un registro anulado');
SELECT pg_temp.debe_pasar('Anulación: corrección total y anulación en la misma transacción',
  pg_temp.correccion('c0000000-0000-0000-0000-0000000000c3', '7d000000-0000-0000-0000-000000000013', 'MBAN01REAL', 500)
  || ARRAY[
    'SET LOCAL ROLE sc_spei',
    $q$INSERT INTO ifpe.spei_anulaciones (spei_inbound_id, correccion_id, motivo, autorizado_por, clave_idempotencia)
       SELECT id, 'c0000000-0000-0000-0000-0000000000c3', 'abonado a la cuenta equivocada',
              '0f000000-0000-0000-0000-0000000000f0', 'op:anul-3'
         FROM ifpe.spei_inbound WHERE clave_rastreo = 'MBAN01REAL'$q$,
    'RESET ROLE'
  ]);
SELECT pg_temp.debe_fallar('Reemplazo: la referencia del DEPOSITO lleva el id del anulado',
  pg_temp.abono(gen_random_uuid(), '1a000000-0000-0000-0000-00000000001a', '646180157000000017', 'MBAN01REAL', 500,
                'WEBHOOK', NULL, $s$'/otro'$s$,
                ', reemplaza_a', $v$, (SELECT id FROM ifpe.spei_inbound WHERE clave_rastreo = 'MBAN01REAL')$v$),
  'SC_SPEI_DESCUADRE');
SELECT pg_temp.debe_pasar('Reemplazo: el pago real se abona a Ana con la misma identidad',
  pg_temp.abono('7d000000-0000-0000-0000-000000000014', '1a000000-0000-0000-0000-00000000001a', '646180157000000017',
                'MBAN01REAL', 500, 'WEBHOOK', NULL,
                $s$'/' || (SELECT id FROM ifpe.spei_inbound WHERE clave_rastreo = 'MBAN01REAL' AND reemplaza_a IS NULL)::text$s$,
                ', reemplaza_a',
                $v$, (SELECT id FROM ifpe.spei_inbound WHERE clave_rastreo = 'MBAN01REAL' AND reemplaza_a IS NULL)$v$));
SELECT pg_temp.debe_fallar('Reemplazo: un anulado tiene a lo sumo un reemplazo',
  pg_temp.abono(gen_random_uuid(), '1a000000-0000-0000-0000-00000000001a', '646180157000000017', 'MBAN01REAL', 500,
                'WEBHOOK', NULL, $s$'/bis'$s$, ', reemplaza_a',
                $v$, (SELECT id FROM ifpe.spei_inbound WHERE clave_rastreo = 'MBAN01REAL' AND reemplaza_a IS NULL)$v$),
  'spei_inbound_reemplaza_a_key');
SELECT pg_temp.exigir('Reemplazo: Beto 0, Ana 1000 + 1000 + 500',
  (SELECT saldo_disponible = 0 FROM ifpe.cuentas WHERE id = '1b000000-0000-0000-0000-00000000001b')
  AND (SELECT saldo_disponible = 2500 FROM ifpe.cuentas WHERE id = '1a000000-0000-0000-0000-00000000001a'));
SELECT pg_temp.debe_fallar('Anulación: inmutable', ARRAY[
  'DELETE FROM ifpe.spei_anulaciones'
], 'SC_LEDGER_INMUTABLE');

-- -----------------------------------------------------------------------------
-- 5. Órdenes: intentos monótonos y resolución del operador (outbound-money-1/2)
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Orden: Ana envía 100 (PENDIENTE)',
  pg_temp.orden('0a000000-0000-0000-0000-000000000001', '7d000000-0000-0000-0000-000000000020', 'SCORDEN0001', 100));
SELECT pg_temp.debe_pasar('Orden: Ana envía 200 (PENDIENTE)',
  pg_temp.orden('0a000000-0000-0000-0000-000000000002', '7d000000-0000-0000-0000-000000000021', 'SCORDEN0002', 200));
SELECT pg_temp.debe_fallar('Orden: intentos_envio no disminuye', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$UPDATE ifpe.spei_outbound SET intentos_envio = 5 WHERE id = '0a000000-0000-0000-0000-000000000001'$q$,
  $q$UPDATE ifpe.spei_outbound SET intentos_envio = 3 WHERE id = '0a000000-0000-0000-0000-000000000001'$q$
], 'no puede disminuir');
SELECT pg_temp.debe_fallar('Resolución: sin reembolso en la misma transacción', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$UPDATE ifpe.spei_outbound SET revision_manual = TRUE, ultimo_error = 'REVISION_MANUAL: sin resultado'
      WHERE id = '0a000000-0000-0000-0000-000000000001'$q$,
  pg_temp.resolucion('0a000000-0000-0000-0000-000000000001', '0f000000-0000-0000-0000-0000000000f0', 'op:res-0')
], 'SC_SPEI_DESCUADRE');
SELECT pg_temp.debe_fallar('Resolución: solo de una orden en revisión manual',
  pg_temp.reembolso('0a000000-0000-0000-0000-000000000001', '7d000000-0000-0000-0000-000000000022', 100)
  || pg_temp.resolucion('0a000000-0000-0000-0000-000000000001', '0f000000-0000-0000-0000-0000000000f0', 'op:res-1'),
  'SC_SPEI_DESCUADRE');
SELECT pg_temp.debe_fallar('Resolución: nunca de una orden que el proveedor aceptó',
  ARRAY[
    'SET LOCAL ROLE sc_spei',
    $q$UPDATE ifpe.spei_outbound SET revision_manual = TRUE, ultimo_error = 'REVISION_MANUAL: rechazo en reenvío',
           id_proveedor = 'STP1' WHERE id = '0a000000-0000-0000-0000-000000000001'$q$
  ]
  || pg_temp.reembolso('0a000000-0000-0000-0000-000000000001', '7d000000-0000-0000-0000-000000000023', 100)
  || pg_temp.resolucion('0a000000-0000-0000-0000-000000000001', '0f000000-0000-0000-0000-0000000000f0', 'op:res-2'),
  'SC_SPEI_DESCUADRE');
SELECT pg_temp.debe_fallar('Resolución: el operador no resuelve sus propias órdenes',
  ARRAY[
    'SET LOCAL ROLE sc_spei',
    $q$UPDATE ifpe.spei_outbound SET revision_manual = TRUE, ultimo_error = 'REVISION_MANUAL: sin resultado'
        WHERE id = '0a000000-0000-0000-0000-000000000001'$q$
  ]
  || pg_temp.reembolso('0a000000-0000-0000-0000-000000000001', '7d000000-0000-0000-0000-000000000024', 100)
  || pg_temp.resolucion('0a000000-0000-0000-0000-000000000001', 'a0000000-0000-0000-0000-00000000000a', 'op:res-3'),
  'SC_SPEI_AUTORESOLUCION');
SELECT pg_temp.debe_fallar('Resolución: sc_ifpe no registra resoluciones', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  pg_temp.resolucion('0a000000-0000-0000-0000-000000000001', '0f000000-0000-0000-0000-0000000000f0', 'op:res-4')
], 'permission denied');
SELECT pg_temp.debe_pasar('Resolución: reembolso de una orden en revisión que el proveedor nunca aceptó',
  ARRAY[
    'SET LOCAL ROLE sc_spei',
    $q$UPDATE ifpe.spei_outbound SET revision_manual = TRUE, ultimo_error = 'REVISION_MANUAL: sin resultado'
        WHERE id = '0a000000-0000-0000-0000-000000000001'$q$
  ]
  || pg_temp.reembolso('0a000000-0000-0000-0000-000000000001', '7d000000-0000-0000-0000-000000000025', 100)
  || pg_temp.resolucion('0a000000-0000-0000-0000-000000000001', '0f000000-0000-0000-0000-0000000000f0', 'op:res-5'));
SELECT pg_temp.debe_fallar('Resolución: una sola por orden',
  ARRAY['SET LOCAL ROLE sc_spei',
        pg_temp.resolucion('0a000000-0000-0000-0000-000000000001', '0f000000-0000-0000-0000-0000000000f0', 'op:res-6')],
  'spei_resoluciones_spei_outbound_id_key');
SELECT pg_temp.debe_fallar('Resolución: inmutable', ARRAY[
  $q$UPDATE ifpe.spei_resoluciones SET motivo = 'otro motivo cualquiera'$q$
], 'SC_LEDGER_INMUTABLE');

-- -----------------------------------------------------------------------------
-- 6. Incidentes de conciliación (outbound-money-6)
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Incidente: "liquidada" de una orden ya reembolsada', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$INSERT INTO ifpe.spei_incidentes (spei_outbound_id, estado_orden, evento, detalle)
     VALUES ('0a000000-0000-0000-0000-000000000001', 'RECHAZADA', 'LIQUIDADA', 'Liquidada { fecha_operacion: None }')$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_fallar('Incidente: los reintentos no lo duplican', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$INSERT INTO ifpe.spei_incidentes (spei_outbound_id, estado_orden, evento, detalle)
     VALUES ('0a000000-0000-0000-0000-000000000001', 'RECHAZADA', 'LIQUIDADA', 'otra vez')$q$
], 'ux_spei_incidentes');
SELECT pg_temp.debe_fallar('Incidente: sc_ifpe no registra incidentes', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  $q$INSERT INTO ifpe.spei_incidentes (spei_outbound_id, estado_orden, evento, detalle)
     VALUES ('0a000000-0000-0000-0000-000000000002', 'RECHAZADA', 'ACEPTADA', 'x')$q$
], 'permission denied');
SELECT pg_temp.debe_fallar('Incidente: inmutable (ni el dueño lo borra)', ARRAY[
  'DELETE FROM ifpe.spei_incidentes'
], 'SC_LEDGER_INMUTABLE');

-- -----------------------------------------------------------------------------
-- 7. Conciliación (outbound-money-5/6)
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_pasar('Conciliación: la orden de 200 se liquida y queda en revisión (incidente)', ARRAY[
  'SET LOCAL ROLE sc_spei',
  $q$UPDATE ifpe.spei_outbound SET estado = 'ENVIADA', enviada_en = now() WHERE id = '0a000000-0000-0000-0000-000000000002'$q$,
  $q$UPDATE ifpe.spei_outbound SET estado = 'LIQUIDADA', liquidada_en = now(), revision_manual = TRUE,
         ultimo_error = 'REVISION_MANUAL: el proveedor informa Rechazada de una orden liquidada'
      WHERE id = '0a000000-0000-0000-0000-000000000002'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('Conciliación: cuenta la LIQUIDADA en revisión y las órdenes con incidente',
  (SELECT ordenes_en_revision = 1 AND incidentes_conciliacion = 1 AND ordenes_en_transito = 0
          AND pendiente_de_envio = 0 AND enviado_sin_confirmar = 0 AND saldo_esperado_custodio = reserva_contable
     FROM ifpe.v_spei_conciliacion WHERE reserva_id = 'e0000000-0000-0000-0000-00000000000e'));

ROLLBACK;
\o
\echo 'tests/db/0007_spei.sql: todas las pruebas pasaron (ROLLBACK)'
