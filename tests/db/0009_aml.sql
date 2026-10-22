-- =============================================================================
--  Pruebas de la migración 0009 (PLD/FT). Corren en una transacción que
--  termina en ROLLBACK: no dejan datos.
--
--    psql "$DATABASE_URL" -f tests/db/0009_aml.sql
--
--  Lo que la base de datos garantiza por sí sola, sin importar el backend:
--  permisos y catálogo (nada ejecutable por PUBLIC, sin USAGE en aml para los
--  roles transaccionales), semillas, la tabla de calificación (calibración y
--  bordes de banda), la guardia de débitos de un cliente congelado, las
--  retenciones protegidas, el espejo del estado de la cuenta, la coherencia y
--  la cobertura de las evaluaciones, la retención del despacho, las máquinas
--  de estados e inmutabilidad, las funciones de los oficiales (cuatro ojos,
--  idempotencia y el contrato JSON), la Lista de Personas Bloqueadas, el
--  monitoreo por lotes, el sellado de la bitácora y la reserva del 100%.
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
-- (las ligas con el ledger y las aprobaciones exigen el mismo xmin).
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

-- Ejecuta una sentencia como sc_aml (la consola) en la transacción principal
-- y devuelve su resultado JSON.
CREATE FUNCTION pg_temp.como_aml(sentencia TEXT) RETURNS JSONB
LANGUAGE plpgsql AS $$
DECLARE
    v JSONB;
BEGIN
    SET LOCAL ROLE sc_aml;
    EXECUTE sentencia INTO v;
    RESET ROLE;
    SET CONSTRAINTS ALL IMMEDIATE;
    SET CONSTRAINTS ALL DEFERRED;
    RETURN v;
END
$$;

-- Llaves de un objeto JSON, ordenadas, contra la lista del contrato (§4.2).
CREATE FUNCTION pg_temp.mismas_claves(j JSONB, esperadas TEXT[]) RETURNS BOOLEAN
LANGUAGE sql AS $$
    SELECT jsonb_typeof(j) = 'object'
       AND (SELECT array_agg(k ORDER BY k) FROM jsonb_object_keys(j) k)
           = (SELECT array_agg(k ORDER BY k) FROM unnest(esperadas) k)
$$;

-- CLABE STP válida (dígito verificador módulo 10) para la cuenta número n.
CREATE FUNCTION pg_temp.clabe(n INTEGER) RETURNS TEXT
LANGUAGE sql AS $$
    SELECT base || ((10 - (SELECT sum((substr(base, i, 1)::int * (ARRAY[3, 7, 1])[(i - 1) % 3 + 1]) % 10)
                             FROM generate_series(1, 17) i) % 10) % 10)::text
      FROM (SELECT '646180' || lpad(n::text, 11, '0') AS base) b
$$;

CREATE SEQUENCE pg_temp.cuentas_seq START 100;

-- Cliente verificado (alta administrativa) con cuenta IFPE. Nombres únicos.
CREATE FUNCTION pg_temp.cliente(p_nombre TEXT, p_nivel TEXT DEFAULT 'MEDIO') RETURNS UUID
LANGUAGE plpgsql AS $$
DECLARE
    v_id UUID := gen_random_uuid();
BEGIN
    INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en,
                              nivel_riesgo_pld)
    VALUES (v_id, 'FISICA', p_nombre, 'Prueba', lower(p_nombre) || '@example.mx', 'VERIFICADO', now(), p_nivel);
    INSERT INTO ifpe.cuentas (cliente_id, clabe) VALUES (v_id, pg_temp.clabe(nextval('pg_temp.cuentas_seq')::int));
    RETURN v_id;
END
$$;

CREATE FUNCTION pg_temp.c(p_nombre TEXT) RETURNS UUID
LANGUAGE sql STABLE AS $$ SELECT id FROM kyc.clientes WHERE nombre = p_nombre $$;

CREATE FUNCTION pg_temp.cta(p_nombre TEXT) RETURNS UUID
LANGUAGE sql STABLE AS $$
    SELECT i.id FROM ifpe.cuentas i JOIN kyc.clientes k ON k.id = i.cliente_id WHERE k.nombre = p_nombre
$$;

-- Personal del banco (oficial de cumplimiento por omisión).
CREATE FUNCTION pg_temp.oficial(p_nombre TEXT, p_rol TEXT DEFAULT 'CUMPLIMIENTO') RETURNS UUID
LANGUAGE plpgsql AS $$
DECLARE
    v_id UUID := gen_random_uuid();
BEGIN
    INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en,
                              nivel_riesgo_pld)
    VALUES (v_id, 'FISICA', p_nombre, 'Personal', lower(p_nombre) || '@banco.mx', 'VERIFICADO', now(), 'BAJO');
    INSERT INTO kyc.credenciales (cliente_id, password_hash, rol) VALUES (v_id, 'x', p_rol);
    RETURN v_id;
END
$$;

-- Caminos del dinero en el mismo orden que el backend: evaluar por la
-- envoltura, escribir y, si corresponde, retener. Corren en la transacción
-- principal (las ligas con el ledger exigen el mismo xmin).
CREATE FUNCTION pg_temp.deposito_spei(p_cuenta UUID, p_monto NUMERIC, p_origen TEXT DEFAULT 'WEBHOOK',
                                      p_ordenante TEXT DEFAULT '012180001234567899')
RETURNS TABLE (decision TEXT, puntaje NUMERIC, nivel TEXT)
LANGUAGE plpgsql AS $$
DECLARE
    v_cliente UUID;
    v_clabe   TEXT;
    v_tx      UUID := gen_random_uuid();
    v_rastreo TEXT := 'R' || upper(substr(replace(v_tx::text, '-', ''), 1, 20));
    v_ref     TEXT;
    v_canal   TEXT := CASE p_origen WHEN 'OPERADOR' THEN 'SPEI_OPERADOR' WHEN 'DEMO' THEN 'SPEI_DEMO' ELSE 'SPEI_WEBHOOK' END;
    d         RECORD;
BEGIN
    SELECT cliente_id, clabe INTO v_cliente, v_clabe FROM ifpe.cuentas WHERE id = p_cuenta;
    v_ref := '40012:20261001:' || v_rastreo;
    SET LOCAL ROLE sc_spei;
    SELECT * INTO d FROM ifpe.aml_evaluar(v_cliente, 'ENTRADA', v_canal, p_monto, v_tx, 'spei:' || v_ref,
                                          jsonb_build_object('cuenta', p_ordenante));
    UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + p_monto WHERE id = p_cuenta;
    UPDATE ifpe.reserva_custodia SET saldo = saldo + p_monto WHERE id = 'e0000000-0000-0000-0000-00000000000e';
    INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
    VALUES (v_tx, 'DEPOSITO', p_cuenta, 'e0000000-0000-0000-0000-00000000000e', p_monto, 'spei:' || v_ref, v_ref);
    INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                   estado, transaccion_id, origen, registrado_por, raw_payload, cuenta_ordenante)
    VALUES ('40012', v_rastreo, '2026-10-01', v_clabe, p_cuenta, p_monto, 'ABONADO', v_tx, p_origen,
            CASE WHEN p_origen = 'OPERADOR' THEN '0f000000-0000-0000-0000-0000000000f0'::uuid END, '{}', p_ordenante);
    IF d.decision = 'RETENER' THEN
        PERFORM ifpe.aml_retener(d.evaluacion_id);
    END IF;
    RESET ROLE;
    SET CONSTRAINTS ALL IMMEDIATE;
    SET CONSTRAINTS ALL DEFERRED;
    RETURN QUERY SELECT d.decision, d.puntaje, d.nivel;
END
$$;

-- SPEI enviado (RETIRO + orden PENDIENTE), como sc_ifpe. Si la evaluación
-- bloquea, la transacción se queda solo con la evaluación (sin ledger).
CREATE FUNCTION pg_temp.retiro_spei(p_cuenta UUID, p_monto NUMERIC, p_clabe TEXT)
RETURNS TABLE (decision TEXT, puntaje NUMERIC, nivel TEXT)
LANGUAGE plpgsql AS $$
DECLARE
    v_cliente UUID;
    v_clabe   TEXT;
    v_tx      UUID := gen_random_uuid();
    v_clave   TEXT := 'cli:' || v_tx;
    v_rastreo TEXT := 'SC' || upper(substr(replace(v_tx::text, '-', ''), 1, 20));
    d         RECORD;
BEGIN
    SELECT cliente_id, clabe INTO v_cliente, v_clabe FROM ifpe.cuentas WHERE id = p_cuenta;
    SET LOCAL ROLE sc_ifpe;
    SELECT * INTO d FROM ifpe.aml_evaluar(v_cliente, 'SALIDA', 'SPEI_ENVIADO', p_monto, v_tx, v_clave,
                                          jsonb_build_object('cuenta', p_clabe));
    IF d.decision <> 'BLOQUEAR' THEN
        UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - p_monto WHERE id = p_cuenta;
        UPDATE ifpe.reserva_custodia SET saldo = saldo - p_monto WHERE id = 'e0000000-0000-0000-0000-00000000000e';
        INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia,
                                        referencia_externa)
        VALUES (v_tx, 'RETIRO', p_cuenta, 'e0000000-0000-0000-0000-00000000000e', p_monto, v_clave, p_clabe);
        INSERT INTO ifpe.spei_outbound (cliente_id, cuenta_id, clave_idempotencia, clave_rastreo, fecha_solicitud,
                                        ordenante_nombre, ordenante_clabe, banco_receptor, beneficiario_clabe,
                                        beneficiario_nombre, monto, concepto, referencia_numerica, tx_retiro_id)
        VALUES (v_cliente, p_cuenta, v_clave, v_rastreo, current_date, 'CLIENTE PRUEBA', v_clabe, substr(p_clabe, 1, 3),
                p_clabe, 'BENEFICIARIO', p_monto, 'PAGO', 1, v_tx);
    END IF;
    RESET ROLE;
    SET CONSTRAINTS ALL IMMEDIATE;
    SET CONSTRAINTS ALL DEFERRED;
    RETURN QUERY SELECT d.decision, d.puntaje, d.nivel;
END
$$;

-- Transferencia interna (evalúa la salida y, si no se bloquea, la entrada).
CREATE FUNCTION pg_temp.transferencia(p_origen UUID, p_destino UUID, p_monto NUMERIC)
RETURNS TABLE (decision TEXT, puntaje NUMERIC, nivel TEXT)
LANGUAGE plpgsql AS $$
DECLARE
    v_tx    UUID := gen_random_uuid();
    v_clave TEXT := 'cli:' || v_tx;
    s       RECORD;
    e       RECORD;
BEGIN
    SET LOCAL ROLE sc_ifpe;
    SELECT * INTO s FROM ifpe.aml_evaluar((SELECT cliente_id FROM ifpe.cuentas WHERE id = p_origen), 'SALIDA',
                                          'TRANSFERENCIA_ENVIADA', p_monto, v_tx, v_clave, '{}');
    IF s.decision <> 'BLOQUEAR' THEN
        SELECT * INTO e FROM ifpe.aml_evaluar((SELECT cliente_id FROM ifpe.cuentas WHERE id = p_destino), 'ENTRADA',
                                              'TRANSFERENCIA_RECIBIDA', p_monto, v_tx, v_clave, '{}');
        UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - p_monto WHERE id = p_origen;
        UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + p_monto WHERE id = p_destino;
        INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia)
        VALUES (v_tx, 'TRANSFERENCIA', p_origen, p_destino, p_monto, v_clave);
        IF e.decision = 'RETENER' THEN
            PERFORM ifpe.aml_retener(e.evaluacion_id);
        END IF;
    END IF;
    RESET ROLE;
    SET CONSTRAINTS ALL IMMEDIATE;
    SET CONSTRAINTS ALL DEFERRED;
    RETURN QUERY SELECT s.decision, s.puntaje, s.nivel;
END
$$;

-- Inversión en mutuo (TRASPASO_A_SOFOM + aportación SOFOM), como sc_puente.
CREATE FUNCTION pg_temp.inversion(p_cuenta UUID, p_monto NUMERIC)
RETURNS TABLE (decision TEXT, puntaje NUMERIC, nivel TEXT)
LANGUAGE plpgsql AS $$
DECLARE
    v_cliente UUID;
    v_tx      UUID := gen_random_uuid();
    v_espejo  UUID := gen_random_uuid();
    v_mutuo   UUID := gen_random_uuid();
    v_clave   TEXT := 'cli:' || v_tx;
    v_sofom   UUID;
    d         RECORD;
BEGIN
    SELECT cliente_id INTO v_cliente FROM ifpe.cuentas WHERE id = p_cuenta;
    SET LOCAL ROLE sc_puente;
    SELECT * INTO d FROM ifpe.aml_evaluar(v_cliente, 'SALIDA', 'INVERSION_MUTUO', p_monto, v_tx, v_clave, '{}');
    IF d.decision <> 'BLOQUEAR' THEN
        UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - p_monto WHERE id = p_cuenta;
        UPDATE ifpe.reserva_custodia SET saldo = saldo - p_monto WHERE id = 'e0000000-0000-0000-0000-00000000000e';
        INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia,
                                        ref_movimiento_espejo)
        VALUES (v_tx, 'TRASPASO_A_SOFOM', p_cuenta, 'e0000000-0000-0000-0000-00000000000e', p_monto, v_clave, v_espejo);
        INSERT INTO sofom.cuentas (cliente_id) VALUES (v_cliente) ON CONFLICT DO NOTHING;
        SELECT id INTO v_sofom FROM sofom.cuentas WHERE cliente_id = v_cliente;
        UPDATE sofom.cuentas SET saldo_bloqueado = saldo_bloqueado + p_monto WHERE id = v_sofom;
        UPDATE sofom.caja SET saldo = saldo + p_monto WHERE id = 'f0000000-0000-0000-0000-00000000000f';
        INSERT INTO sofom.contratos_mutuo (id, cliente_id, cuenta_sofom_id, cuenta_ifpe_origen_id, tx_aportacion_ifpe_id,
                                           principal, tasa_anual, plazo_dias, fecha_inicio, hash_terminos)
        VALUES (v_mutuo, v_cliente, v_sofom, p_cuenta, v_tx, p_monto, 0.05, 365, current_date, repeat('a', 64));
        INSERT INTO sofom.transacciones (id, tipo, cuenta_id, contrato_id, caja_id, monto, clave_idempotencia,
                                         ref_movimiento_espejo)
        VALUES (v_espejo, 'APORTACION_MUTUO', v_sofom, v_mutuo, 'f0000000-0000-0000-0000-00000000000f', p_monto, v_clave,
                v_tx);
    END IF;
    RESET ROLE;
    SET CONSTRAINTS ALL IMMEDIATE;
    SET CONSTRAINTS ALL DEFERRED;
    RETURN QUERY SELECT d.decision, d.puntaje, d.nivel;
END
$$;

-- Un paso de la tabla de calificación: ejecuta la operación y exige la
-- decisión, el puntaje y el nivel calculados por la base de datos.
CREATE FUNCTION pg_temp.paso(p_cliente TEXT, p_operacion TEXT, p_monto NUMERIC, p_destino TEXT, p_decision TEXT,
                             p_puntaje NUMERIC, p_nivel TEXT) RETURNS void
LANGUAGE plpgsql AS $$
DECLARE
    r RECORD;
BEGIN
    CASE p_operacion
        WHEN 'DEPOSITO' THEN SELECT * INTO r FROM pg_temp.deposito_spei(pg_temp.cta(p_cliente), p_monto);
        WHEN 'RETIRO' THEN SELECT * INTO r FROM pg_temp.retiro_spei(pg_temp.cta(p_cliente), p_monto, p_destino);
        WHEN 'TRANSFERENCIA' THEN
            SELECT * INTO r FROM pg_temp.transferencia(pg_temp.cta(p_cliente), pg_temp.cta(p_destino), p_monto);
        WHEN 'INVERSION' THEN SELECT * INTO r FROM pg_temp.inversion(pg_temp.cta(p_cliente), p_monto);
    END CASE;
    IF r.decision IS DISTINCT FROM p_decision OR r.puntaje IS DISTINCT FROM p_puntaje OR r.nivel IS DISTINCT FROM p_nivel THEN
        RAISE EXCEPTION 'FALLO [Calificación: % % %]: se obtuvo (%, %, %), se esperaba (%, %, %)', p_cliente, p_operacion,
            p_monto, r.decision, r.puntaje, r.nivel, p_decision, p_puntaje, p_nivel;
    END IF;
    RAISE NOTICE 'OK  califica   % % % -> % % %', p_cliente, p_operacion, p_monto, r.decision, r.puntaje, r.nivel;
END
$$;

-- Alertas de un cliente como 'REGLA NIVEL PUNTAJE DECISION' por folio.
CREATE FUNCTION pg_temp.alertas(p_cliente TEXT) RETURNS TEXT[]
LANGUAGE sql STABLE AS $$
    SELECT coalesce(array_agg(regla_codigo || ' ' || nivel_riesgo || ' ' || puntaje || ' ' || decision ORDER BY folio), '{}')
      FROM aml.alertas WHERE cliente_id = pg_temp.c(p_cliente)
$$;

-- -----------------------------------------------------------------------------
-- Datos base (como dueño del esquema)
-- -----------------------------------------------------------------------------
INSERT INTO ifpe.reserva_custodia (id, institucion, tipo, numero_cuenta) VALUES
  ('e0000000-0000-0000-0000-00000000000e', 'Banco de México', 'BANXICO', 'SC-RES-001');
INSERT INTO sofom.caja (id, institucion, numero_cuenta) VALUES
  ('f0000000-0000-0000-0000-00000000000f', 'Banco Custodio SOFOM', 'SC-SOFOM-001');
INSERT INTO kyc.clientes (id, tipo_persona, nombre, apellido_paterno, email, estatus_kyc, kyc_verificado_en, nivel_riesgo_pld)
VALUES ('0f000000-0000-0000-0000-0000000000f0', 'FISICA', 'Operador', 'Mesa', 'op@banco.mx', 'VERIFICADO', now(), 'BAJO');
INSERT INTO kyc.credenciales (cliente_id, password_hash, rol) VALUES ('0f000000-0000-0000-0000-0000000000f0', 'x', 'OPERADOR');
SELECT pg_temp.oficial('Oficial1');
SELECT pg_temp.oficial('Oficial2');
SELECT pg_temp.oficial('Auditora', 'AUDITOR');

-- -----------------------------------------------------------------------------
-- 1. Catálogo y permisos
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('Catálogo: ninguna función de aml ni envoltura ifpe.aml_*/sofom.aml_* es ejecutable por PUBLIC',
  NOT EXISTS (SELECT 1 FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
               CROSS JOIN LATERAL aclexplode(coalesce(p.proacl, acldefault('f', p.proowner))) a
               WHERE a.grantee = 0 AND a.privilege_type = 'EXECUTE'
                 AND (n.nspname = 'aml' OR (n.nspname IN ('ifpe', 'sofom') AND p.proname LIKE 'aml\_%'))));
SELECT pg_temp.exigir('Catálogo: hay funciones en aml y las cinco envolturas',
  (SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace WHERE n.nspname = 'aml') > 50
  AND (SELECT count(*) FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
        WHERE n.nspname IN ('ifpe', 'sofom') AND p.proname LIKE 'aml\_%') = 5);
SELECT pg_temp.exigir('Catálogo: sin USAGE en aml para los seis roles transaccionales; sí para sc_aml y sc_auditor',
  NOT has_schema_privilege('sc_ifpe', 'aml', 'USAGE') AND NOT has_schema_privilege('sc_spei', 'aml', 'USAGE')
  AND NOT has_schema_privilege('sc_puente', 'aml', 'USAGE') AND NOT has_schema_privilege('sc_sofom', 'aml', 'USAGE')
  AND NOT has_schema_privilege('sc_kyc', 'aml', 'USAGE') AND NOT has_schema_privilege('sc_kyc_verificador', 'aml', 'USAGE')
  AND has_schema_privilege('sc_aml', 'aml', 'USAGE') AND has_schema_privilege('sc_auditor', 'aml', 'USAGE'));
SELECT pg_temp.exigir('Catálogo: sc_aml sin USAGE en ifpe, sofom, kyc ni nucleo',
  NOT has_schema_privilege('sc_aml', 'ifpe', 'USAGE') AND NOT has_schema_privilege('sc_aml', 'sofom', 'USAGE')
  AND NOT has_schema_privilege('sc_aml', 'kyc', 'USAGE') AND NOT has_schema_privilege('sc_aml', 'nucleo', 'USAGE'));
SELECT pg_temp.exigir('Catálogo: sc_aml sin INSERT, UPDATE ni DELETE en ninguna tabla de aml, ifpe, sofom ni kyc',
  NOT EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
               WHERE n.nspname IN ('aml', 'ifpe', 'sofom', 'kyc') AND c.relkind IN ('r', 'p', 'v')
                 AND (has_table_privilege('sc_aml', c.oid, 'INSERT') OR has_table_privilege('sc_aml', c.oid, 'UPDATE')
                      OR has_table_privilege('sc_aml', c.oid, 'DELETE')
                      OR has_any_column_privilege('sc_aml', c.oid, 'INSERT')
                      OR has_any_column_privilege('sc_aml', c.oid, 'UPDATE'))));
SELECT pg_temp.exigir('Catálogo: sc_aml y sc_auditor leen todas las tablas de aml (incluida la vista heredada)',
  NOT EXISTS (SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
               WHERE n.nspname = 'aml' AND c.relkind IN ('r', 'v')
                 AND NOT (has_table_privilege('sc_aml', c.oid, 'SELECT') AND has_table_privilege('sc_auditor', c.oid, 'SELECT')))
  AND has_table_privilege('sc_aml', 'aml.v_historial_pld', 'SELECT'));
SELECT pg_temp.exigir('Catálogo: sc_ifpe ya no cambia ifpe.cuentas.estado',
  NOT has_column_privilege('sc_ifpe', 'ifpe.cuentas', 'estado', 'UPDATE')
  AND has_column_privilege('sc_ifpe', 'ifpe.cuentas', 'saldo_disponible', 'UPDATE'));
-- cliente_id conserva el privilegio: el trigger de 0007 lo vuelve inmutable
-- (y tests/db/0007_kyc.sql espera su error, no un permission denied).
SELECT pg_temp.exigir('Catálogo: sc_puente solo actualiza saldos (y el titular inmutable) de sofom.cuentas',
  NOT has_table_privilege('sc_puente', 'sofom.cuentas', 'UPDATE')
  AND has_column_privilege('sc_puente', 'sofom.cuentas', 'saldo_disponible', 'UPDATE')
  AND has_column_privilege('sc_puente', 'sofom.cuentas', 'saldo_bloqueado', 'UPDATE')
  AND NOT has_column_privilege('sc_puente', 'sofom.cuentas', 'estado', 'UPDATE')
  AND NOT has_column_privilege('sc_puente', 'sofom.cuentas', 'moneda', 'UPDATE')
  AND (SELECT array_agg(attname::text ORDER BY attname) FROM pg_attribute
        WHERE attrelid = 'sofom.cuentas'::regclass AND attnum > 0 AND NOT attisdropped
          AND has_column_privilege('sc_puente', 'sofom.cuentas', attname, 'UPDATE'))
      = ARRAY['cliente_id', 'saldo_bloqueado', 'saldo_disponible']);
SELECT pg_temp.exigir('Catálogo: kyc.alertas_pld sin SELECT para sc_kyc / sc_kyc_verificador ni INSERT para los roles transaccionales',
  NOT has_table_privilege('sc_kyc', 'kyc.alertas_pld', 'SELECT')
  AND NOT has_table_privilege('sc_kyc', 'kyc.alertas_pld', 'UPDATE')
  AND NOT has_table_privilege('sc_kyc_verificador', 'kyc.alertas_pld', 'SELECT')
  AND NOT has_table_privilege('sc_ifpe', 'kyc.alertas_pld', 'INSERT')
  AND NOT has_table_privilege('sc_sofom', 'kyc.alertas_pld', 'INSERT')
  AND NOT has_table_privilege('sc_puente', 'kyc.alertas_pld', 'INSERT')
  AND NOT has_table_privilege('sc_spei', 'kyc.alertas_pld', 'INSERT'));
SELECT pg_temp.exigir('Catálogo: las envolturas solo para sus roles',
  has_function_privilege('sc_ifpe', 'ifpe.aml_evaluar(uuid, text, text, numeric, uuid, text, jsonb)', 'EXECUTE')
  AND has_function_privilege('sc_spei', 'ifpe.aml_evaluar(uuid, text, text, numeric, uuid, text, jsonb)', 'EXECUTE')
  AND has_function_privilege('sc_puente', 'ifpe.aml_evaluar(uuid, text, text, numeric, uuid, text, jsonb)', 'EXECUTE')
  AND NOT has_function_privilege('sc_sofom', 'ifpe.aml_evaluar(uuid, text, text, numeric, uuid, text, jsonb)', 'EXECUTE')
  AND has_function_privilege('sc_spei', 'ifpe.aml_retener(uuid)', 'EXECUTE')
  AND NOT has_function_privilege('sc_puente', 'ifpe.aml_retener(uuid)', 'EXECUTE')
  AND has_function_privilege('sc_spei', 'ifpe.aml_cliente_congelado(uuid)', 'EXECUTE')
  AND NOT has_function_privilege('sc_ifpe', 'ifpe.aml_cliente_congelado(uuid)', 'EXECUTE')
  AND has_function_privilege('sc_sofom', 'sofom.aml_evaluar(uuid, text, text, numeric, uuid, text, jsonb)', 'EXECUTE')
  AND has_function_privilege('sc_sofom', 'sofom.aml_bloquear_cliente(uuid)', 'EXECUTE')
  AND NOT has_function_privilege('sc_ifpe', 'sofom.aml_evaluar(uuid, text, text, numeric, uuid, text, jsonb)', 'EXECUTE'));
SELECT pg_temp.exigir('Catálogo: sc_aml no ejecuta el motor ni las funciones solo del dueño',
  NOT has_function_privilege('sc_aml', 'aml.evaluar_operacion(text, uuid, text, text, numeric, uuid, text, jsonb)', 'EXECUTE')
  AND NOT has_function_privilege('sc_aml', 'aml.congelar(uuid, text, uuid, uuid, numeric, text, uuid)', 'EXECUTE')
  AND NOT has_function_privilege('sc_aml', 'aml.liberar_retencion(uuid, uuid)', 'EXECUTE')
  AND has_function_privilege('sc_aml', 'aml.exigir_lector(uuid)', 'EXECUTE')
  AND has_function_privilege('sc_aml', 'aml.persona_json(uuid)', 'EXECUTE')
  AND has_function_privilege('sc_auditor', 'aml.verificar_bitacora()', 'EXECUTE')
  AND NOT has_function_privilege('sc_auditor', 'aml.resolver_alerta(uuid, uuid, text, text, text, boolean, boolean, text, text)', 'EXECUTE'));
SELECT pg_temp.debe_fallar('Catálogo: sc_ifpe no lee aml.alertas', ARRAY[
  'SET LOCAL ROLE sc_ifpe', 'SELECT 1 FROM aml.alertas'
], 'permission denied for schema aml');
SELECT pg_temp.debe_fallar('Catálogo: sc_aml no inserta alertas', ARRAY[
  'SET LOCAL ROLE sc_aml',
  $q$INSERT INTO aml.alertas (cliente_id, entidad, sentido, regla_id, regla_codigo, tipo_reporte, origen, decision,
                              nivel_riesgo, puntaje, detalles, clave_deduplicacion, fecha_limite_reporte)
     VALUES (gen_random_uuid(), 'IFPE', 'NINGUNO', gen_random_uuid(), 'X', 'INUSUAL', 'MANUAL', 'REGISTRADA', 'BAJO', 0,
             '{}', 'x', now())$q$
], 'permission denied for table alertas');
SELECT pg_temp.debe_fallar('Catálogo: sc_aml no lee el ledger', ARRAY[
  'SET LOCAL ROLE sc_aml', 'SELECT 1 FROM ifpe.transacciones'
], 'permission denied for schema ifpe');

-- -----------------------------------------------------------------------------
-- 2. Semillas
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('Semillas: parámetros versión 1 (30/60/85, sin observación, 24 h / 72 h, 60 días, 24 h, 7 días)',
  (SELECT count(*) = 1 AND bool_and(id = 1 AND umbral_medio = 30 AND umbral_alto = 60 AND umbral_critico = 85
          AND NOT modo_observacion AND sla_critica_horas = 24 AND sla_congelamiento_horas = 72
          AND dias_reporte_inusual = 60 AND horas_reporte_urgente = 24 AND dias_aviso_plazo = 7 AND propuesta_id IS NULL)
     FROM aml.parametros));
SELECT pg_temp.exigir('Semillas: una regla activa por (código, nivel) según la matriz',
  (SELECT array_agg(codigo || ':' || nivel_cliente ORDER BY codigo, nivel_cliente) FROM aml.reglas_riesgo WHERE activa)
  = ARRAY['M01_PEP:TODOS', 'M02_GEOGRAFIA:TODOS', 'M03_NIVEL_RIESGO:ALTO', 'M03_NIVEL_RIESGO:BAJO',
          'M03_NIVEL_RIESGO:MEDIO', 'M04_CUENTA_NUEVA:TODOS', 'R01_OPERACION_RELEVANTE:TODOS',
          'R02_FRACCIONAMIENTO:TODOS', 'R03_CLIENTE_RIESGO_ALTO:ALTO', 'R04_FUERA_DE_PERFIL:ALTO',
          'R04_FUERA_DE_PERFIL:BAJO', 'R04_FUERA_DE_PERFIL:MEDIO', 'R05_ESTRUCTURACION:ALTO', 'R05_ESTRUCTURACION:BAJO',
          'R05_ESTRUCTURACION:MEDIO', 'R07_VOLUMEN_DIARIO:ALTO', 'R07_VOLUMEN_DIARIO:BAJO', 'R07_VOLUMEN_DIARIO:MEDIO',
          'R08_FRECUENCIA:ALTO', 'R08_FRECUENCIA:BAJO', 'R08_FRECUENCIA:MEDIO', 'R09_LISTA_BLOQUEADAS:TODOS',
          'R11_PASO_RAPIDO:TODOS', 'R20_ESTRUCTURACION_LENTA:ALTO', 'R20_ESTRUCTURACION_LENTA:BAJO',
          'R20_ESTRUCTURACION_LENTA:MEDIO', 'R21_VOLUMEN_MENSUAL:ALTO', 'R21_VOLUMEN_MENSUAL:BAJO',
          'R21_VOLUMEN_MENSUAL:MEDIO', 'R22_CONCENTRACION_ENTRADAS:TODOS', 'R23_DISPERSION_SALIDAS:TODOS',
          'R24_PASO_RAPIDO_48H:TODOS', 'R25_CIRCULAR:TODOS', 'R26_RED_ORDENANTE:TODOS', 'R27_RED_BENEFICIARIO:TODOS',
          'R30_CONGELAMIENTO_MANUAL:TODOS']
  AND (SELECT NOT activa FROM aml.reglas_riesgo WHERE codigo = 'R00_LEGADO'));
SELECT pg_temp.exigir('Semillas: R05 bandas 0.80 / 0.75 / 0.60 y frecuencias 3 / 3 / 2; solo R05, R09, R11, R20 y R30 congelan',
  (SELECT banda_inferior = 0.75 AND frecuencia_min = 3 FROM aml.regla('R05_ESTRUCTURACION', 'MEDIO'))
  AND (SELECT banda_inferior = 0.60 AND frecuencia_min = 2 FROM aml.regla('R05_ESTRUCTURACION', 'ALTO'))
  AND (SELECT banda_inferior = 0.80 FROM aml.regla('R05_ESTRUCTURACION', 'BAJO'))
  AND (SELECT array_agg(DISTINCT codigo ORDER BY codigo) FROM aml.reglas_riesgo WHERE permite_congelar)
      = ARRAY['R05_ESTRUCTURACION', 'R09_LISTA_BLOQUEADAS', 'R11_PASO_RAPIDO', 'R20_ESTRUCTURACION_LENTA',
              'R30_CONGELAMIENTO_MANUAL']);
SELECT pg_temp.exigir('Semillas: M02 con los países del llamado a la acción del GAFI {KP,IR,MM}; R03 solo para ALTO',
  (SELECT paises_alto_riesgo::text[] = ARRAY['KP', 'IR', 'MM'] AND entidades_alto_riesgo = '{}' AND estados_alto_riesgo = '{}'
     FROM aml.regla('M02_GEOGRAFIA', 'MEDIO'))
  AND aml.regla('R03_CLIENTE_RIESGO_ALTO', 'MEDIO') IS NULL);
SELECT pg_temp.exigir('Semillas: monitoreo en las cabezas del ledger; la bitácora tiene la versión 1 y la migración',
  (SELECT ultima_secuencia_ifpe = 0 AND ultima_secuencia_sofom = 0 AND en_curso_hasta = '-infinity' FROM aml.monitoreo_estado)
  AND (SELECT count(*) FROM aml.bitacora WHERE evento = 'REGLA_VERSION_NUEVA') = (SELECT count(*) FROM aml.reglas_riesgo)
  AND (SELECT count(*) FROM aml.bitacora WHERE evento = 'PARAMETROS_VERSION_NUEVA') = 1
  AND (SELECT datos ->> 'alertas_copiadas' = '0' FROM aml.bitacora WHERE evento = 'MIGRACION_0009'));

-- -----------------------------------------------------------------------------
-- 3. Tabla de calificación (calibración de §2.5 y bordes de banda), cada
--    escenario con clientes nuevos: MEDIO, cuenta de días, sin ingreso ni PEP
-- -----------------------------------------------------------------------------
SELECT pg_temp.cliente(n) FROM unnest(ARRAY['Rel', 'Ana', 'Fan', 'Paso', 'Mil', 'Mut', 'Borde1', 'Borde2', 'Borde3',
                                            'Borde4']) n;
SELECT pg_temp.cliente(n, 'ALTO') FROM unnest(ARRAY['Pep', 'Alto1', 'Alto2']) n;
UPDATE kyc.clientes SET es_pep = TRUE, ingreso_mensual_estimado = 20000 WHERE nombre = 'Pep';

-- Heredado de ledger.rs: R01 es registro (no alerta); R02 a 65 ALTO, sin congelar.
SELECT pg_temp.paso('Rel', 'DEPOSITO', 150000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Rel', 'DEPOSITO', 80000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Rel', 'DEPOSITO', 80000, NULL, 'PERMITIR', 65, 'ALTO');
SELECT pg_temp.exigir('Calificación 150k/80k/80k: solo la alerta R02 y el registro de la operación relevante',
  pg_temp.alertas('Rel') = ARRAY['R02_FRACCIONAMIENTO ALTO 65.00 REGISTRADA']
  AND (SELECT (count(*), sum(monto)) = (1::bigint, 150000::numeric) FROM aml.operaciones_relevantes
        WHERE cliente_id = pg_temp.c('Rel'))
  AND NOT aml.cliente_congelado(pg_temp.c('Rel')));

-- TASK 4: tres abonos de 145,000.
SELECT pg_temp.paso('Ana', 'DEPOSITO', 145000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Ana', 'DEPOSITO', 145000, NULL, 'PERMITIR', 65, 'ALTO');
SELECT pg_temp.exigir('Estructuración: tras el segundo abono, solo R02 y sin congelamiento',
  pg_temp.alertas('Ana') = ARRAY['R02_FRACCIONAMIENTO ALTO 65.00 REGISTRADA'] AND NOT aml.cliente_congelado(pg_temp.c('Ana')));
SELECT pg_temp.paso('Ana', 'DEPOSITO', 145000, NULL, 'RETENER', 100, 'CRITICO');
SELECT pg_temp.exigir('Estructuración: R05 CRITICO 100 RETENIDA, congelamiento AUTOMATICO y cuenta CONGELADO_PREVENTIVO',
  pg_temp.alertas('Ana') = ARRAY['R02_FRACCIONAMIENTO ALTO 65.00 REGISTRADA', 'R05_ESTRUCTURACION CRITICO 100.00 RETENIDA']
  AND (SELECT origen = 'AUTOMATICO' AND estado = 'ACTIVO' AND puntaje = 100 AND alerta_id IS NOT NULL
         FROM aml.congelamientos WHERE cliente_id = pg_temp.c('Ana'))
  AND (SELECT estado = 'CONGELADO_PREVENTIVO' FROM ifpe.cuentas WHERE id = pg_temp.cta('Ana')));
SELECT pg_temp.exigir('Estructuración: la retención cubre los fondos del patrón (0 disponible, 435,000 bloqueado)',
  (SELECT (saldo_disponible, saldo_bloqueado) = (0::numeric, 435000::numeric) FROM ifpe.cuentas WHERE id = pg_temp.cta('Ana'))
  AND (SELECT count(*) = 1 AND sum(monto) = 435000 FROM aml.retenciones WHERE cliente_id = pg_temp.c('Ana')));
SELECT pg_temp.exigir('Estructuración: la RETENCION tiene clave ret:<id>, concepto neutro y hash válido',
  (SELECT t.clave_idempotencia = 'ret:' || r.id AND t.concepto = 'Retención temporal' AND t.tipo = 'RETENCION'
          AND t.reserva_id IS NULL AND t.tx_hash = nucleo.sha256_hex(ifpe.payload_canonico(t))
     FROM aml.retenciones r JOIN ifpe.transacciones t ON t.id = r.tx_retencion_id WHERE r.cliente_id = pg_temp.c('Ana')));
SELECT pg_temp.exigir('Estructuración: evaluaciones PERMITIDA, ALERTADA, RETENIDA en línea; avisos y bitácora',
  (SELECT array_agg(resultado || '/' || origen || '/' || canal ORDER BY creado_en) FROM aml.evaluaciones
    WHERE cliente_id = pg_temp.c('Ana'))
  = ARRAY['PERMITIDA/EN_LINEA/SPEI_WEBHOOK', 'ALERTADA/EN_LINEA/SPEI_WEBHOOK', 'RETENIDA/EN_LINEA/SPEI_WEBHOOK']
  AND (SELECT array_agg(tipo ORDER BY id) FROM aml.notificaciones WHERE cliente_id = pg_temp.c('Ana'))
      = ARRAY['ALERTA_NUEVA', 'ALERTA_CRITICA', 'CONGELAMIENTO']
  AND (SELECT array_agg(DISTINCT evento ORDER BY evento) FROM aml.bitacora WHERE cliente_id = pg_temp.c('Ana'))
      = ARRAY['ALERTA_CREADA', 'CONGELAMIENTO_APLICADO', 'RETENCION_APLICADA']
  AND (SELECT count(*) = 3 FROM ifpe.spei_inbound s JOIN ifpe.cuentas c ON c.id = s.cuenta_id
        WHERE c.cliente_id = pg_temp.c('Ana') AND s.estado = 'ABONADO'));
SELECT pg_temp.exigir('Estructuración: la evaluación crítica guarda el cálculo (R05 + M03 + M04 = 105 -> 100, fondos del patrón)',
  (SELECT factores ->> 'puntaje_bruto' = '105.00' AND factores ->> 'dominante' = 'R05_ESTRUCTURACION'
          AND (factores ->> 'congelar')::boolean AND factores ->> 'monto_patron' = '435000.00000000'
          AND jsonb_array_length(factores -> 'transacciones_patron') = 2
          AND (SELECT array_agg(f ->> 'codigo' ORDER BY f ->> 'codigo') FROM jsonb_array_elements(factores -> 'factores') f)
              = ARRAY['M03_NIVEL_RIESGO', 'M04_CUENTA_NUEVA', 'R05_ESTRUCTURACION']
          AND monto_retener = 435000 AND parametros_id = 1 AND cardinality(reglas_aplicadas) > 5
     FROM aml.evaluaciones WHERE cliente_id = pg_temp.c('Ana') AND resultado = 'RETENIDA'));

-- Dispersión: 450k de entrada y tres SPEI de 145k a tres CLABE.
SELECT pg_temp.paso('Fan', 'DEPOSITO', 450000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Fan', 'RETIRO', 145000, '012180001234567899', 'PERMITIR', 40, 'MEDIO');
SELECT pg_temp.paso('Fan', 'RETIRO', 145000, '002010077777777771', 'PERMITIR', 40, 'MEDIO');
SELECT pg_temp.paso('Fan', 'RETIRO', 145000, '014180655000000002', 'BLOQUEAR', 100, 'CRITICO');
SELECT pg_temp.exigir('Dispersión: el tercer envío se bloquea sin ledger; R07 agrupada y R05 por intento bloqueado',
  (SELECT count(*) FROM ifpe.transacciones WHERE cuenta_origen_id = pg_temp.cta('Fan') AND tipo = 'RETIRO') = 2
  AND (SELECT count(*) FROM ifpe.spei_outbound WHERE cliente_id = pg_temp.c('Fan')) = 2
  AND (SELECT (saldo_disponible, saldo_bloqueado) = (160000::numeric, 0::numeric) FROM ifpe.cuentas WHERE id = pg_temp.cta('Fan'))
  AND (SELECT count(*) = 1 FROM aml.evaluaciones WHERE cliente_id = pg_temp.c('Fan') AND resultado = 'BLOQUEADA'
          AND motivo_bloqueo = 'PUNTAJE_CRITICO' AND transaccion_id IS NULL)
  AND (SELECT array_agg(regla_codigo || ' ' || origen || ' ' || decision || ' ' || ocurrencias ORDER BY folio)
         FROM aml.alertas WHERE cliente_id = pg_temp.c('Fan'))
      = ARRAY['R07_VOLUMEN_DIARIO EN_LINEA REGISTRADA 2', 'R05_ESTRUCTURACION INTENTO_BLOQUEADO BLOQUEADA 1']
  AND aml.cliente_congelado(pg_temp.c('Fan'))
  AND (SELECT estado = 'CONGELADO_PREVENTIVO' FROM ifpe.cuentas WHERE id = pg_temp.cta('Fan')));

-- Paso rápido: 200k de entrada y 185k de salida.
SELECT pg_temp.paso('Paso', 'DEPOSITO', 200000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Paso', 'RETIRO', 185000, '012180001234567899', 'BLOQUEAR', 85, 'CRITICO');
SELECT pg_temp.exigir('Paso rápido: R11 CRITICO 85 bloqueada, congelamiento y ningún RETIRO',
  pg_temp.alertas('Paso') = ARRAY['R11_PASO_RAPIDO CRITICO 85.00 BLOQUEADA'] AND aml.cliente_congelado(pg_temp.c('Paso'))
  AND NOT EXISTS (SELECT 1 FROM ifpe.transacciones WHERE cuenta_origen_id = pg_temp.cta('Paso')));

-- Perfil: PEP de riesgo alto con ingreso de 20,000; los factores de perfil
-- nunca congelan.
SELECT pg_temp.paso('Pep', 'DEPOSITO', 200000, NULL, 'PERMITIR', 100, 'CRITICO');
SELECT pg_temp.paso('Pep', 'RETIRO', 160000, '012180001234567899', 'PERMITIR', 100, 'CRITICO');
SELECT pg_temp.exigir('Perfil PEP: alertas CRITICO REGISTRADA (R03 + R04 + R07 + M01 + M03 + M04 = 125) sin congelar',
  (SELECT array_agg(decision ORDER BY folio) FROM aml.alertas WHERE cliente_id = pg_temp.c('Pep')) = ARRAY['REGISTRADA', 'REGISTRADA']
  AND (SELECT factores ->> 'puntaje_bruto' = '125.00' AND NOT (factores ->> 'congelar')::boolean
         FROM aml.evaluaciones WHERE cliente_id = pg_temp.c('Pep') AND sentido = 'SALIDA')
  AND NOT aml.cliente_congelado(pg_temp.c('Pep')));

-- reserva.rs: un abono de 10,000,202.10 es relevante y de volumen (R07 40 MEDIO).
SELECT pg_temp.paso('Mil', 'DEPOSITO', 10000202.10, NULL, 'PERMITIR', 40, 'MEDIO');
SELECT pg_temp.exigir('Volumen: alerta R07 MEDIO, registro relevante y sin congelamiento',
  pg_temp.alertas('Mil') = ARRAY['R07_VOLUMEN_DIARIO MEDIO 40.00 REGISTRADA']
  AND EXISTS (SELECT 1 FROM aml.operaciones_relevantes WHERE cliente_id = pg_temp.c('Mil') AND monto = 10000202.10)
  AND NOT aml.cliente_congelado(pg_temp.c('Mil')));

-- puente.rs: abono de 100k e inversión de 100k en mutuo: nada.
SELECT pg_temp.paso('Mut', 'DEPOSITO', 100000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Mut', 'INVERSION', 100000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.exigir('Mutuo: sin alertas ni congelamiento',
  pg_temp.alertas('Mut') = '{}' AND NOT aml.cliente_congelado(pg_temp.c('Mut')));

-- Bordes de banda MEDIO [112,500, 150,000) tras dos abonos de 145k.
SELECT pg_temp.paso(n, 'DEPOSITO', 145000, NULL, 'PERMITIR', 0, 'BAJO') FROM unnest(ARRAY['Borde1', 'Borde2', 'Borde3', 'Borde4']) n;
SELECT pg_temp.paso(n, 'DEPOSITO', 145000, NULL, 'PERMITIR', 65, 'ALTO') FROM unnest(ARRAY['Borde1', 'Borde2', 'Borde3', 'Borde4']) n;
SELECT pg_temp.paso('Borde1', 'DEPOSITO', 112499.99, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Borde2', 'DEPOSITO', 112500, NULL, 'RETENER', 100, 'CRITICO');
SELECT pg_temp.paso('Borde3', 'DEPOSITO', 149999.99, NULL, 'RETENER', 100, 'CRITICO');
SELECT pg_temp.paso('Borde4', 'DEPOSITO', 150000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.exigir('Bordes MEDIO: 112,499.99 fuera, 112,500 y 149,999.99 dentro, 150,000 solo registro',
  NOT aml.cliente_congelado(pg_temp.c('Borde1')) AND aml.cliente_congelado(pg_temp.c('Borde2'))
  AND aml.cliente_congelado(pg_temp.c('Borde3')) AND NOT aml.cliente_congelado(pg_temp.c('Borde4'))
  AND EXISTS (SELECT 1 FROM aml.operaciones_relevantes WHERE cliente_id = pg_temp.c('Borde4') AND monto = 150000)
  AND (SELECT sum(monto) = 402500 FROM aml.retenciones WHERE cliente_id = pg_temp.c('Borde2')));

-- Bordes ALTO [90,000, 150,000) con frecuencia 2 (R03 suma 35 a todo >= 50k).
SELECT pg_temp.paso(n, 'DEPOSITO', 145000, NULL, 'PERMITIR', 55, 'MEDIO') FROM unnest(ARRAY['Alto1', 'Alto2']) n;
SELECT pg_temp.paso('Alto1', 'DEPOSITO', 89999.99, NULL, 'PERMITIR', 100, 'CRITICO');
SELECT pg_temp.paso('Alto2', 'DEPOSITO', 90000, NULL, 'RETENER', 100, 'CRITICO');
SELECT pg_temp.exigir('Bordes ALTO: 89,999.99 queda fuera de banda (CRITICO sin congelar), 90,000 dentro (congela)',
  NOT aml.cliente_congelado(pg_temp.c('Alto1')) AND aml.cliente_congelado(pg_temp.c('Alto2'))
  AND NOT EXISTS (SELECT 1 FROM aml.alertas WHERE cliente_id = pg_temp.c('Alto1') AND regla_codigo = 'R05_ESTRUCTURACION'));

SELECT pg_temp.exigir('Calibración: solo congelan Ana, Fan, Paso, Borde2, Borde3 y Alto2',
  (SELECT array_agg(k.nombre ORDER BY k.nombre) FROM aml.congelamientos f JOIN kyc.clientes k ON k.id = f.cliente_id)
  = ARRAY['Alto2', 'Ana', 'Borde2', 'Borde3', 'Fan', 'Paso']);
SELECT pg_temp.exigir('Calibración: ningún movimiento en línea quedó sin evaluación (sin marcas DIFERIDA)',
  NOT EXISTS (SELECT 1 FROM aml.evaluaciones WHERE origen = 'DIFERIDA')
  AND NOT EXISTS (SELECT 1 FROM aml.bitacora WHERE evento = 'EVALUACION_DIFERIDA'));

-- -----------------------------------------------------------------------------
-- 4. Guardias sobre un cliente congelado (aml.congelar)
-- -----------------------------------------------------------------------------
SELECT pg_temp.cliente(n) FROM unnest(ARRAY['Fz', 'Bx', 'Legal']) n;
SELECT pg_temp.paso('Fz', 'DEPOSITO', 50000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Fz', 'RETIRO', 1000, '012180001234567899', 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Fz', 'RETIRO', 1000, '002010077777777771', 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Fz', 'INVERSION', 10000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Bx', 'DEPOSITO', 20000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Legal', 'DEPOSITO', 1000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.debe_pasar('Guardia: congelamiento manual de Fz (cuenta CONGELADO_PREVENTIVO)', ARRAY[
  format($q$SELECT aml.congelar(%L, 'MANUAL', NULL, NULL, 100, 'Congelamiento de prueba de las guardias', %L)$q$,
         pg_temp.c('Fz'), pg_temp.c('Oficial1'))
]);
SELECT pg_temp.exigir('Guardia: estado espejo y congelamiento activo',
  (SELECT estado = 'CONGELADO_PREVENTIVO' FROM ifpe.cuentas WHERE id = pg_temp.cta('Fz')) AND aml.cliente_congelado(pg_temp.c('Fz')));

SELECT pg_temp.debe_fallar('Guardia: RETIRO de un cliente congelado (sc_ifpe)', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100 WHERE id = %L$q$, pg_temp.cta('Fz')),
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo - 100 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  format($q$INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia, referencia_externa)
            VALUES ('RETIRO', %L, 'e0000000-0000-0000-0000-00000000000e', 100, 'cli:guardia-1', '012180001234567899')$q$,
         pg_temp.cta('Fz'))
], 'SC_OPERACION_NO_DISPONIBLE');
SELECT pg_temp.debe_fallar('Guardia: TRANSFERENCIA enviada por un cliente congelado (sc_ifpe)', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100 WHERE id = %L$q$, pg_temp.cta('Fz')),
  format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 100 WHERE id = %L$q$, pg_temp.cta('Bx')),
  format($q$INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia)
            VALUES ('TRANSFERENCIA', %L, %L, 100, 'cli:guardia-2')$q$, pg_temp.cta('Fz'), pg_temp.cta('Bx'))
], 'SC_OPERACION_NO_DISPONIBLE');
SELECT pg_temp.debe_fallar('Guardia: TRASPASO_A_SOFOM de un cliente congelado (sc_puente)', ARRAY[
  'SET LOCAL ROLE sc_puente',
  format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100 WHERE id = %L$q$, pg_temp.cta('Fz')),
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo - 100 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  format($q$INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia, ref_movimiento_espejo)
            VALUES ('TRASPASO_A_SOFOM', %L, 'e0000000-0000-0000-0000-00000000000e', 100, 'cli:guardia-3', gen_random_uuid())$q$,
         pg_temp.cta('Fz'))
], 'SC_OPERACION_NO_DISPONIBLE');
SELECT pg_temp.debe_fallar('Guardia: ni el dueño del esquema retira de un cliente congelado', ARRAY[
  format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100 WHERE id = %L$q$, pg_temp.cta('Fz')),
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo - 100 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  format($q$INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia, referencia_externa)
            VALUES ('RETIRO', %L, 'e0000000-0000-0000-0000-00000000000e', 100, 'cli:guardia-4', '012180001234567899')$q$,
         pg_temp.cta('Fz'))
], 'SC_OPERACION_NO_DISPONIBLE');
SELECT pg_temp.paso('Fz', 'RETIRO', 100, '012180001234567899', 'BLOQUEAR', NULL, NULL);
SELECT pg_temp.paso('Fz', 'TRANSFERENCIA', 100, 'Bx', 'BLOQUEAR', NULL, NULL);
SELECT pg_temp.paso('Fz', 'INVERSION', 100, NULL, 'BLOQUEAR', NULL, NULL);
-- Un reintento con la misma clave no cuenta un intento nuevo.
SELECT pg_temp.exigir('Guardia: intento bloqueado',
  (SELECT decision FROM ifpe.aml_evaluar(pg_temp.c('Fz'), 'SALIDA', 'SPEI_ENVIADO', 100, gen_random_uuid(), 'cli:reintento', '{}'))
  = 'BLOQUEAR');
SELECT pg_temp.exigir('Guardia: su reintento con la misma clave también se bloquea',
  (SELECT decision FROM ifpe.aml_evaluar(pg_temp.c('Fz'), 'SALIDA', 'SPEI_ENVIADO', 100, gen_random_uuid(), 'cli:reintento', '{}'))
  = 'BLOQUEAR');
SELECT pg_temp.exigir('Guardia: cuatro intentos bloqueados (el reintento no cuenta) con evaluación y bitácora',
  (SELECT intentos_bloqueados = 4 AND ultimo_intento_en IS NOT NULL FROM aml.congelamientos
        WHERE cliente_id = pg_temp.c('Fz') AND estado = 'ACTIVO')
  AND (SELECT count(*) = 4 FROM aml.evaluaciones WHERE cliente_id = pg_temp.c('Fz') AND resultado = 'BLOQUEADA'
          AND motivo_bloqueo = 'CONGELADO')
  AND (SELECT count(*) = 4 FROM aml.bitacora WHERE cliente_id = pg_temp.c('Fz') AND evento = 'OPERACION_BLOQUEADA'));

-- Entrante a un cliente congelado: se abona y se retiene completo.
SELECT pg_temp.paso('Fz', 'DEPOSITO', 3000, NULL, 'RETENER', 0, 'BAJO');
SELECT pg_temp.paso('Bx', 'TRANSFERENCIA', 2000, 'Fz', 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.exigir('Guardia: los abonos a un congelado se retienen completos (3,000 y 2,000)',
  (SELECT array_agg(monto ORDER BY creado_en) FROM aml.retenciones WHERE cliente_id = pg_temp.c('Fz')) = ARRAY[3000, 2000]::numeric[]
  AND (SELECT saldo_bloqueado = 5000 FROM ifpe.cuentas WHERE id = pg_temp.cta('Fz'))
  AND (SELECT count(*) = 2 FROM aml.evaluaciones WHERE cliente_id = pg_temp.c('Fz') AND resultado = 'RETENIDA'));

-- Las correcciones del operador siguen; sobre un congelado quedan en la bitácora.
SELECT pg_temp.debe_pasar('Guardia: corrección spei-correccion: de un congelado (sc_spei)', ARRAY[
  'SET LOCAL ROLE sc_spei',
  format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 500 WHERE id = %L$q$, pg_temp.cta('Fz')),
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo - 500 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia)
            VALUES ('7c000000-0000-0000-0000-000000000001', 'RETIRO', %L, 'e0000000-0000-0000-0000-00000000000e', 500,
                    'spei-correccion:c0000000-0000-0000-0000-000000000001')$q$, pg_temp.cta('Fz')),
  format($q$INSERT INTO ifpe.spei_correcciones (id, spei_inbound_id, cuenta_id, monto, motivo, autorizado_por,
                                                clave_idempotencia, tx_id)
            VALUES ('c0000000-0000-0000-0000-000000000001', %L, %L, 500, 'abono duplicado del banco emisor',
                    '0f000000-0000-0000-0000-0000000000f0', 'op:corr-1', '7c000000-0000-0000-0000-000000000001')$q$,
         (SELECT id FROM ifpe.spei_inbound WHERE cuenta_id = pg_temp.cta('Fz') AND monto = 50000), pg_temp.cta('Fz')),
  'RESET ROLE'
]);
SELECT pg_temp.exigir('Guardia: la corrección deja CARGO_EN_CUENTA_CONGELADA y un aviso ALTA',
  EXISTS (SELECT 1 FROM aml.bitacora WHERE evento = 'CARGO_EN_CUENTA_CONGELADA' AND cliente_id = pg_temp.c('Fz')
             AND transaccion_id = '7c000000-0000-0000-0000-000000000001')
  AND EXISTS (SELECT 1 FROM aml.notificaciones WHERE tipo = 'CARGO_EN_CUENTA_CONGELADA' AND prioridad = 'ALTA'
                 AND cliente_id = pg_temp.c('Fz')));

-- El reverso de una orden propia y el vencimiento de un mutuo entran sin retención.
SELECT pg_temp.debe_pasar('Guardia: reverso spei-reverso: de una orden de un congelado (sc_spei)', ARRAY[
  'SET LOCAL ROLE sc_spei',
  format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 1000 WHERE id = %L$q$, pg_temp.cta('Fz')),
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 1000 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia)
            VALUES ('7c000000-0000-0000-0000-000000000002', 'DEPOSITO', %L, 'e0000000-0000-0000-0000-00000000000e', 1000,
                    'spei-reverso:' || %L)$q$,
         pg_temp.cta('Fz'), (SELECT id FROM ifpe.spei_outbound WHERE cliente_id = pg_temp.c('Fz')
                                AND beneficiario_clabe = '012180001234567899')),
  format($q$UPDATE ifpe.spei_outbound SET estado = 'RECHAZADA', tx_reverso_id = '7c000000-0000-0000-0000-000000000002',
                   causa_devolucion = 99, finalizada_en = now() WHERE id = %L$q$,
         (SELECT id FROM ifpe.spei_outbound WHERE cliente_id = pg_temp.c('Fz') AND beneficiario_clabe = '012180001234567899')),
  'RESET ROLE'
]);
SELECT pg_temp.debe_pasar('Guardia: capital propio de la SOFOM (sc_sofom)', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$UPDATE sofom.caja SET saldo = saldo + 1000 WHERE id = 'f0000000-0000-0000-0000-00000000000f'$q$,
  $q$INSERT INTO sofom.transacciones (tipo, caja_id, monto, clave_idempotencia)
     VALUES ('APORTACION_CAPITAL', 'f0000000-0000-0000-0000-00000000000f', 1000, 'capital-aml-1')$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_pasar('Guardia: RETORNO_DE_SOFOM (interés del mutuo) a un congelado (sc_puente)', ARRAY[
  'SET LOCAL ROLE sc_puente',
  format($q$UPDATE sofom.cuentas SET saldo_disponible = saldo_disponible + 50 WHERE cliente_id = %L$q$, pg_temp.c('Fz')),
  format($q$INSERT INTO sofom.transacciones (tipo, cuenta_id, contrato_id, caja_id, monto, clave_idempotencia)
            SELECT 'PAGO_INTERES_MUTUO', m.cuenta_sofom_id, m.id, 'f0000000-0000-0000-0000-00000000000f', 50, 'interes-aml-1'
              FROM sofom.contratos_mutuo m WHERE m.cliente_id = %L$q$, pg_temp.c('Fz')),
  format($q$UPDATE sofom.cuentas SET saldo_disponible = saldo_disponible - 50 WHERE cliente_id = %L$q$, pg_temp.c('Fz')),
  $q$UPDATE sofom.caja SET saldo = saldo - 50 WHERE id = 'f0000000-0000-0000-0000-00000000000f'$q$,
  format($q$INSERT INTO sofom.transacciones (id, tipo, cuenta_id, contrato_id, caja_id, monto, clave_idempotencia,
                                             ref_movimiento_espejo)
            SELECT '7c000000-0000-0000-0000-000000000004', 'RETORNO_A_IFPE', m.cuenta_sofom_id, m.id,
                   'f0000000-0000-0000-0000-00000000000f', 50, 'retorno-aml-1', '7c000000-0000-0000-0000-000000000003'
              FROM sofom.contratos_mutuo m WHERE m.cliente_id = %L$q$, pg_temp.c('Fz')),
  format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 50 WHERE id = %L$q$, pg_temp.cta('Fz')),
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 50 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia,
                                            ref_movimiento_espejo)
            VALUES ('7c000000-0000-0000-0000-000000000003', 'RETORNO_DE_SOFOM', %L, 'e0000000-0000-0000-0000-00000000000e',
                    50, 'retorno-aml-1', '7c000000-0000-0000-0000-000000000004')$q$, pg_temp.cta('Fz')),
  'RESET ROLE'
]);
SELECT pg_temp.exigir('Guardia: el reverso y el vencimiento quedan disponibles (sin retención nueva)',
  (SELECT count(*) = 2 FROM aml.retenciones WHERE cliente_id = pg_temp.c('Fz'))
  AND (SELECT (saldo_disponible, saldo_bloqueado) = (38550::numeric, 5000::numeric) FROM ifpe.cuentas WHERE id = pg_temp.cta('Fz')));
SELECT pg_temp.debe_fallar('Guardia: un abono spei: a un congelado sin su retención', ARRAY[
  'SET LOCAL ROLE sc_spei',
  format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 700 WHERE id = %L$q$, pg_temp.cta('Fz')),
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 700 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
            VALUES ('7c000000-0000-0000-0000-000000000005', 'DEPOSITO', %L, 'e0000000-0000-0000-0000-00000000000e', 700,
                    'spei:40012:20261001:SINRETENCION', '40012:20261001:SINRETENCION')$q$, pg_temp.cta('Fz')),
  format($q$INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id,
                                           monto, estado, transaccion_id, origen, raw_payload)
            VALUES ('40012', 'SINRETENCION', '2026-10-01', %L, %L, 700, 'ABONADO', '7c000000-0000-0000-0000-000000000005',
                    'WEBHOOK', '{}')$q$,
         (SELECT clabe FROM ifpe.cuentas WHERE id = pg_temp.cta('Fz')), pg_temp.cta('Fz'))
], 'SC_AML_RETENCION_FALTANTE');

-- CONGELADA (orden de autoridad): la guardia cierra el hueco que solo exigía Rust.
SELECT pg_temp.debe_pasar('Guardia: el dueño congela legalmente la cuenta de Legal', ARRAY[
  format($q$UPDATE ifpe.cuentas SET estado = 'CONGELADA' WHERE id = %L$q$, pg_temp.cta('Legal'))
]);
SELECT pg_temp.debe_fallar('Guardia: RETIRO de una cuenta CONGELADA', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100 WHERE id = %L$q$, pg_temp.cta('Legal')),
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo - 100 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  format($q$INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia, referencia_externa)
            VALUES ('RETIRO', %L, 'e0000000-0000-0000-0000-00000000000e', 100, 'cli:legal-1', '012180001234567899')$q$,
         pg_temp.cta('Legal'))
], 'SC_CUENTA_CONGELADA');

-- SOFOM: sin activación ni desembolso de crédito para un acreditado congelado.
SELECT pg_temp.debe_pasar('Guardia: crédito EN_FONDEO de Fz (alta del dueño)', ARRAY[
  format($q$INSERT INTO sofom.creditos (id, acreditado_id, principal, tasa_anual, plazo_dias, fecha_desembolso)
            VALUES ('4c000000-0000-0000-0000-000000000001', %L, 5000, 0.2, 30, current_date)$q$, pg_temp.c('Fz'))
]);
SELECT pg_temp.debe_fallar('Guardia: EN_FONDEO -> VIGENTE de un acreditado congelado (sc_sofom)', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$UPDATE sofom.creditos SET estado = 'VIGENTE', saldo_insoluto = 5000 WHERE id = '4c000000-0000-0000-0000-000000000001'$q$
], 'SC_OPERACION_NO_DISPONIBLE');
SELECT pg_temp.debe_fallar('Guardia: DESEMBOLSO_CREDITO a un acreditado congelado (sc_sofom)', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$INSERT INTO sofom.transacciones (tipo, credito_id, caja_id, monto, clave_idempotencia)
     VALUES ('DESEMBOLSO_CREDITO', '4c000000-0000-0000-0000-000000000001', 'f0000000-0000-0000-0000-00000000000f', 5000,
             'desembolso-aml-1')$q$
], 'SC_OPERACION_NO_DISPONIBLE');
SELECT pg_temp.exigir('Guardia: sofom.aml_evaluar bloquea el desembolso de un congelado',
  (SELECT decision FROM sofom.aml_evaluar(pg_temp.c('Fz'), 'SALIDA', 'DESEMBOLSO_CREDITO', 5000, gen_random_uuid(),
                                          'sofom:desembolso-aml-2', '{}')) = 'BLOQUEAR');
SELECT pg_temp.debe_fallar('Guardia: una combinación inválida de entidad, sentido y canal', ARRAY[
  format($q$SELECT * FROM sofom.aml_evaluar(%L, 'ENTRADA', 'SPEI_WEBHOOK', 100, gen_random_uuid(), 'x', '{}')$q$,
         pg_temp.c('Bx'))
], 'SC_AML_PARAMETRO_INVALIDO');

-- -----------------------------------------------------------------------------
-- 5. Retenciones protegidas y reserva del 100%
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('Retención: una LIBERACION genérica no alcanza una retención PLD (sc_ifpe)', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  format($q$UPDATE ifpe.cuentas SET saldo_bloqueado = saldo_bloqueado - 1000, saldo_disponible = saldo_disponible + 1000
             WHERE id = %L$q$, pg_temp.cta('Ana')),
  format($q$INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia)
            VALUES ('LIBERACION_RETENCION', %L, %L, 1000, 'op:liberacion-1')$q$, pg_temp.cta('Ana'), pg_temp.cta('Ana'))
], 'SC_RETENCION_PROTEGIDA');
SELECT pg_temp.debe_fallar('Retención: una clave ret: falsificada por el dueño', ARRAY[
  format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100, saldo_bloqueado = saldo_bloqueado + 100
             WHERE id = %L$q$, pg_temp.cta('Fz')),
  format($q$INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia)
            VALUES ('RETENCION', %L, %L, 100, 'ret:' || gen_random_uuid())$q$, pg_temp.cta('Fz'), pg_temp.cta('Fz'))
], 'SC_AML_CLAVE_RESERVADA');
SELECT pg_temp.debe_fallar('Retención: una liberación con clave lib: falsificada no alcanza la retención', ARRAY[
  format($q$UPDATE ifpe.cuentas SET saldo_bloqueado = saldo_bloqueado - 100, saldo_disponible = saldo_disponible + 100
             WHERE id = %L$q$, pg_temp.cta('Fz')),
  format($q$INSERT INTO ifpe.transacciones (tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia)
            VALUES ('LIBERACION_RETENCION', %L, %L, 100, 'lib:' || %L)$q$, pg_temp.cta('Fz'), pg_temp.cta('Fz'),
         (SELECT id FROM aml.retenciones WHERE cliente_id = pg_temp.c('Fz') ORDER BY creado_en LIMIT 1))
], 'SC_RETENCION_PROTEGIDA');
SELECT pg_temp.debe_fallar('Retención: aml.liberar_retencion sin una solicitud aprobada', ARRAY[
  format($q$SELECT aml.liberar_retencion(%L, NULL)$q$, (SELECT id FROM aml.retenciones WHERE cliente_id = pg_temp.c('Ana')))
], 'SC_AML_LIBERACION_SIN_APROBACION');
SELECT pg_temp.debe_fallar('Retención: aml.aplicar_retencion de una evaluación que no retiene', ARRAY[
  format($q$SELECT aml.aplicar_retencion(%L)$q$,
         (SELECT id FROM aml.evaluaciones WHERE cliente_id = pg_temp.c('Rel') ORDER BY creado_en LIMIT 1))
], 'SC_AML_EVALUACION_INCOHERENTE');
SELECT pg_temp.debe_fallar('Retención: inmutable (ni el dueño la borra)', ARRAY[
  'DELETE FROM aml.retenciones'
], 'SC_AML_REGISTRO_INMUTABLE');
SELECT pg_temp.exigir('Retención: la reserva cuadra y cada saldo coincide con su sombra del ledger',
  (SELECT pasivo_clientes = activo_reserva FROM ifpe.estado_reserva)
  AND (SELECT pasivo_clientes FROM ifpe.estado_reserva) = (SELECT sum(saldo_disponible + saldo_bloqueado) FROM ifpe.cuentas)
  AND NOT EXISTS (SELECT 1 FROM ifpe.cuentas c LEFT JOIN ifpe.saldos_ledger s ON s.cuenta_id = c.id
                   WHERE (c.saldo_disponible, c.saldo_bloqueado) IS DISTINCT FROM
                         (coalesce(s.disponible, 0), coalesce(s.bloqueado, 0)))
  AND NOT EXISTS (SELECT 1 FROM ifpe.cuentas c
                   WHERE c.saldo_bloqueado < (SELECT coalesce(sum(monto), 0) FROM aml.retenciones r
                                               WHERE r.cuenta_id = c.id AND r.estado = 'VIGENTE')));
SELECT pg_temp.exigir('Retención: la cuenta congelada sigue siendo hoja del Proof of Reserve con su saldo total',
  EXISTS (SELECT 1 FROM ifpe.v_hojas_reserva WHERE cuenta_id = pg_temp.cta('Ana') AND saldo_total = 435000));
SELECT pg_temp.exigir('Retención: todo movimiento ret:/lib: tiene su registro en aml.retenciones',
  NOT EXISTS (SELECT 1 FROM ifpe.transacciones t
               WHERE (t.clave_idempotencia LIKE 'ret:%' AND NOT EXISTS (SELECT 1 FROM aml.retenciones r WHERE r.tx_retencion_id = t.id))
                  OR (t.clave_idempotencia LIKE 'lib:%' AND NOT EXISTS (SELECT 1 FROM aml.retenciones r WHERE r.tx_liberacion_id = t.id))));

-- -----------------------------------------------------------------------------
-- 6. Espejo del estado de la cuenta
-- -----------------------------------------------------------------------------
SELECT pg_temp.debe_fallar('Espejo: el dueño reactiva una cuenta congelada preventivamente', ARRAY[
  format($q$UPDATE ifpe.cuentas SET estado = 'ACTIVA' WHERE id = %L$q$, pg_temp.cta('Ana'))
], 'SC_AML_ESPEJO');
SELECT pg_temp.debe_fallar('Espejo: CONGELADO_PREVENTIVO sin congelamiento', ARRAY[
  format($q$UPDATE ifpe.cuentas SET estado = 'CONGELADO_PREVENTIVO' WHERE id = %L$q$, pg_temp.cta('Bx'))
], 'SC_AML_ESPEJO');
SELECT pg_temp.debe_fallar('Espejo: sc_ifpe ya no cambia el estado de una cuenta', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  format($q$UPDATE ifpe.cuentas SET estado = 'ACTIVA' WHERE id = %L$q$, pg_temp.cta('Ana'))
], 'permission denied for table cuentas');
SELECT pg_temp.debe_fallar('Espejo: CONGELADO_PREVENTIVO no existe en la SOFOM', ARRAY[
  format($q$UPDATE sofom.cuentas SET estado = 'CONGELADO_PREVENTIVO' WHERE cliente_id = %L$q$, pg_temp.c('Fz'))
], 'cuentas_estado_check');
SELECT pg_temp.debe_pasar('Espejo: congelar preventivamente una cuenta CONGELADA la deja CONGELADA', ARRAY[
  format($q$SELECT aml.congelar(%L, 'MANUAL', NULL, NULL, 100, 'Congelamiento sobre una cuenta con orden de autoridad', NULL)$q$,
         pg_temp.c('Legal'))
]);
SELECT pg_temp.exigir('Espejo: CONGELADA y congelamiento ACTIVO conviven',
  (SELECT estado = 'CONGELADA' FROM ifpe.cuentas WHERE id = pg_temp.cta('Legal')) AND aml.cliente_congelado(pg_temp.c('Legal')));

-- -----------------------------------------------------------------------------
-- 7. Coherencia y cobertura de las evaluaciones
-- -----------------------------------------------------------------------------
-- Escribe en una subtransacción anidada (otro xmin).
CREATE FUNCTION pg_temp.en_subtransaccion(sentencias TEXT) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
    BEGIN
        EXECUTE sentencias;
    EXCEPTION WHEN division_by_zero THEN
        RAISE;
    END;
END
$$;

-- Abono SPEI escrito sin evaluar (como un camino que olvidó la evaluación).
CREATE FUNCTION pg_temp.abono_directo(p_tx UUID, p_cuenta UUID, p_monto NUMERIC, p_rastreo TEXT, p_origen TEXT DEFAULT 'WEBHOOK')
RETURNS TEXT
LANGUAGE sql AS $$
    SELECT format($q$
        UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + %2$s WHERE id = %1$L;
        UPDATE ifpe.reserva_custodia SET saldo = saldo + %2$s WHERE id = 'e0000000-0000-0000-0000-00000000000e';
        INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
        VALUES (%3$L, 'DEPOSITO', %1$L, 'e0000000-0000-0000-0000-00000000000e', %2$s, 'spei:40012:20261001:' || %4$L,
                '40012:20261001:' || %4$L);
        INSERT INTO ifpe.spei_inbound (institucion_emisora, clave_rastreo, fecha_operacion, receptor_clabe, cuenta_id, monto,
                                       estado, transaccion_id, origen, registrado_por, raw_payload)
        VALUES ('40012', %4$L, '2026-10-01', (SELECT clabe FROM ifpe.cuentas WHERE id = %1$L), %1$L, %2$s, 'ABONADO', %3$L,
                %5$L, CASE WHEN %5$L = 'OPERADOR' THEN '0f000000-0000-0000-0000-0000000000f0'::uuid END, '{}')$q$,
        p_cuenta, p_monto, p_tx, p_rastreo, p_origen)
$$;

SELECT pg_temp.cliente('Cob');
SELECT pg_temp.debe_fallar('Coherencia: la evaluación no coincide con el monto del abono', ARRAY[
  format($q$SELECT ifpe.aml_evaluar(%L, 'ENTRADA', 'SPEI_WEBHOOK', 1000, '7e000000-0000-0000-0000-000000000001',
                                    'spei:40012:20261001:MONTO1', '{}')$q$, pg_temp.c('Bx')),
  pg_temp.abono_directo('7e000000-0000-0000-0000-000000000001', pg_temp.cta('Bx'), 999, 'MONTO1')
], 'SC_AML_EVALUACION_INCOHERENTE');
SELECT pg_temp.debe_fallar('Coherencia: la evaluación no coincide con el canal del movimiento', ARRAY[
  format($q$SELECT ifpe.aml_evaluar(%L, 'ENTRADA', 'TRANSFERENCIA_RECIBIDA', 1000, '7e000000-0000-0000-0000-000000000002',
                                    'spei:40012:20261001:CANAL1', '{}')$q$, pg_temp.c('Bx')),
  pg_temp.abono_directo('7e000000-0000-0000-0000-000000000002', pg_temp.cta('Bx'), 1000, 'CANAL1')
], 'SC_AML_EVALUACION_INCOHERENTE');
SELECT pg_temp.debe_fallar('Coherencia: la evaluación es de otro cliente', ARRAY[
  format($q$SELECT ifpe.aml_evaluar(%L, 'ENTRADA', 'SPEI_WEBHOOK', 1000, '7e000000-0000-0000-0000-000000000003',
                                    'spei:40012:20261001:OTRO1', '{}')$q$, pg_temp.c('Rel')),
  pg_temp.abono_directo('7e000000-0000-0000-0000-000000000003', pg_temp.cta('Bx'), 1000, 'OTRO1')
], 'SC_AML_EVALUACION_INCOHERENTE');
SELECT pg_temp.debe_fallar('Coherencia: retener un abono escrito en otra (sub)transacción que su evaluación', ARRAY[
  format($q$SELECT ifpe.aml_evaluar(%L, 'ENTRADA', 'SPEI_WEBHOOK', 800, '7e000000-0000-0000-0000-000000000004',
                                    'spei:40012:20261001:XMIN1', '{}')$q$, pg_temp.c('Fz')),
  format('SELECT pg_temp.en_subtransaccion(%L)',
         pg_temp.abono_directo('7e000000-0000-0000-0000-000000000004', pg_temp.cta('Fz'), 800, 'XMIN1')),
  $q$SELECT ifpe.aml_retener((SELECT id FROM aml.evaluaciones WHERE transaccion_id = '7e000000-0000-0000-0000-000000000004'))$q$
], 'SC_AML_EVALUACION_INCOHERENTE');
SELECT pg_temp.debe_fallar('Coherencia: una evaluación en línea sin su movimiento del ledger', ARRAY[
  format($q$SELECT ifpe.aml_evaluar(%L, 'ENTRADA', 'SPEI_WEBHOOK', 1000, gen_random_uuid(), 'spei:huerfana', '{}')$q$,
         pg_temp.c('Bx'))
], 'SC_AML_EVALUACION_HUERFANA');
SELECT pg_temp.debe_fallar('Coherencia: un canal que no corresponde al sentido', ARRAY[
  format($q$SELECT ifpe.aml_evaluar(%L, 'SALIDA', 'SPEI_WEBHOOK', 1, gen_random_uuid(), 'x', '{}')$q$, pg_temp.c('Bx'))
], 'SC_AML_PARAMETRO_INVALIDO');

-- Cobertura: movimientos escritos sin evaluación reciben una marca DIFERIDA
-- (nunca un error) y un aviso COBERTURA.
SELECT pg_temp.debe_pasar('Cobertura: abono SPEI de demostración sin evaluación (sc_spei)', ARRAY[
  'SET LOCAL ROLE sc_spei',
  pg_temp.abono_directo('7e000000-0000-0000-0000-000000000011', pg_temp.cta('Cob'), 5000, 'COBERTURA1', 'DEMO'),
  'RESET ROLE'
]);
ALTER TABLE ifpe.transacciones DISABLE TRIGGER trg_ifpe_tx_spei_registrado;
SELECT pg_temp.debe_pasar('Cobertura: DEPOSITO spei: sin registro en spei_inbound (lista blanca desactivada por el dueño)', ARRAY[
  format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 300 WHERE id = %L$q$, pg_temp.cta('Cob')),
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo + 300 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_destino_id, reserva_id, monto, clave_idempotencia, referencia_externa)
            VALUES ('7e000000-0000-0000-0000-000000000012', 'DEPOSITO', %L, 'e0000000-0000-0000-0000-00000000000e', 300,
                    'spei:ANTERIOR0005', 'ANTERIOR0005')$q$, pg_temp.cta('Cob'))
]);
ALTER TABLE ifpe.transacciones ENABLE TRIGGER trg_ifpe_tx_spei_registrado;
SELECT pg_temp.debe_pasar('Cobertura: TRANSFERENCIA sin evaluación (sc_ifpe)', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 100 WHERE id = %L$q$, pg_temp.cta('Cob')),
  format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible + 100 WHERE id = %L$q$, pg_temp.cta('Bx')),
  format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, cuenta_destino_id, monto, clave_idempotencia)
            VALUES ('7e000000-0000-0000-0000-000000000013', 'TRANSFERENCIA', %L, %L, 100, 'cli:cobertura-trf')$q$,
         pg_temp.cta('Cob'), pg_temp.cta('Bx')),
  'RESET ROLE'
]);
SELECT pg_temp.debe_pasar('Cobertura: RETIRO y orden SPEI sin evaluación (sc_ifpe)', ARRAY[
  'SET LOCAL ROLE sc_ifpe',
  format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 200 WHERE id = %L$q$, pg_temp.cta('Cob')),
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo - 200 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia, referencia_externa)
            VALUES ('7e000000-0000-0000-0000-000000000014', 'RETIRO', %L, 'e0000000-0000-0000-0000-00000000000e', 200,
                    'cli:cobertura-ret', '012180001234567899')$q$, pg_temp.cta('Cob')),
  format($q$INSERT INTO ifpe.spei_outbound (cliente_id, cuenta_id, clave_idempotencia, clave_rastreo, fecha_solicitud,
                 ordenante_nombre, ordenante_clabe, banco_receptor, beneficiario_clabe, beneficiario_nombre, monto, concepto,
                 referencia_numerica, tx_retiro_id)
            VALUES (%L, %L, 'cli:cobertura-ret', 'SCCOBERTURA1', current_date, 'COB PRUEBA', %L, '012', '012180001234567899',
                    'BENEFICIARIO', 200, 'PAGO', 1, '7e000000-0000-0000-0000-000000000014')$q$,
         pg_temp.c('Cob'), pg_temp.cta('Cob'), (SELECT clabe FROM ifpe.cuentas WHERE id = pg_temp.cta('Cob'))),
  'RESET ROLE'
]);
SELECT pg_temp.debe_pasar('Cobertura: TRASPASO_A_SOFOM (mutuo) sin evaluación (sc_puente)', ARRAY[
  'SET LOCAL ROLE sc_puente',
  format($q$UPDATE ifpe.cuentas SET saldo_disponible = saldo_disponible - 1000 WHERE id = %L$q$, pg_temp.cta('Cob')),
  $q$UPDATE ifpe.reserva_custodia SET saldo = saldo - 1000 WHERE id = 'e0000000-0000-0000-0000-00000000000e'$q$,
  format($q$INSERT INTO ifpe.transacciones (id, tipo, cuenta_origen_id, reserva_id, monto, clave_idempotencia, ref_movimiento_espejo)
            VALUES ('7e000000-0000-0000-0000-000000000015', 'TRASPASO_A_SOFOM', %L, 'e0000000-0000-0000-0000-00000000000e',
                    1000, 'cli:cobertura-mutuo', '7e000000-0000-0000-0000-000000000016')$q$, pg_temp.cta('Cob')),
  format($q$INSERT INTO sofom.cuentas (cliente_id) VALUES (%L)$q$, pg_temp.c('Cob')),
  format($q$UPDATE sofom.cuentas SET saldo_bloqueado = saldo_bloqueado + 1000 WHERE cliente_id = %L$q$, pg_temp.c('Cob')),
  $q$UPDATE sofom.caja SET saldo = saldo + 1000 WHERE id = 'f0000000-0000-0000-0000-00000000000f'$q$,
  format($q$INSERT INTO sofom.contratos_mutuo (id, cliente_id, cuenta_sofom_id, cuenta_ifpe_origen_id, tx_aportacion_ifpe_id,
                                               principal, tasa_anual, plazo_dias, fecha_inicio, hash_terminos)
            SELECT '3b000000-0000-0000-0000-000000000001', %L, s.id, %L, '7e000000-0000-0000-0000-000000000015', 1000, 0.05,
                   365, current_date, repeat('b', 64)
              FROM sofom.cuentas s WHERE s.cliente_id = %L$q$, pg_temp.c('Cob'), pg_temp.cta('Cob'), pg_temp.c('Cob')),
  format($q$INSERT INTO sofom.transacciones (id, tipo, cuenta_id, contrato_id, caja_id, monto, clave_idempotencia,
                                             ref_movimiento_espejo)
            SELECT '7e000000-0000-0000-0000-000000000016', 'APORTACION_MUTUO', s.id, '3b000000-0000-0000-0000-000000000001',
                   'f0000000-0000-0000-0000-00000000000f', 1000, 'cli:cobertura-mutuo', '7e000000-0000-0000-0000-000000000015'
              FROM sofom.cuentas s WHERE s.cliente_id = %L$q$, pg_temp.c('Cob')),
  'RESET ROLE'
]);
SELECT pg_temp.debe_pasar('Cobertura: crédito de Cob fondeado por el mutuo de Mut, activado y desembolsado sin evaluación', ARRAY[
  format($q$INSERT INTO sofom.creditos (id, acreditado_id, principal, tasa_anual, plazo_dias, fecha_desembolso, clabe_desembolso)
            VALUES ('4c000000-0000-0000-0000-000000000002', %L, 1000, 0.2, 30, current_date, '012180001234567899')$q$,
         pg_temp.c('Cob')),
  'SET LOCAL ROLE sc_sofom',
  format($q$INSERT INTO sofom.asignaciones_fondeo (credito_id, contrato_id, monto)
            SELECT '4c000000-0000-0000-0000-000000000002', m.id, 1000 FROM sofom.contratos_mutuo m WHERE m.cliente_id = %L$q$,
         pg_temp.c('Mut')),
  $q$UPDATE sofom.creditos SET estado = 'VIGENTE', saldo_insoluto = 1000 WHERE id = '4c000000-0000-0000-0000-000000000002'$q$,
  $q$UPDATE sofom.caja SET saldo = saldo - 1000 WHERE id = 'f0000000-0000-0000-0000-00000000000f'$q$,
  $q$INSERT INTO sofom.transacciones (id, tipo, credito_id, caja_id, monto, clave_idempotencia)
     VALUES ('7e000000-0000-0000-0000-000000000017', 'DESEMBOLSO_CREDITO', '4c000000-0000-0000-0000-000000000002',
             'f0000000-0000-0000-0000-00000000000f', 1000, 'desembolso-cobertura')$q$,
  'RESET ROLE'
]);
SELECT pg_temp.debe_pasar('Cobertura: cobro del crédito sin evaluación (sc_sofom)', ARRAY[
  'SET LOCAL ROLE sc_sofom',
  $q$UPDATE sofom.caja SET saldo = saldo + 500 WHERE id = 'f0000000-0000-0000-0000-00000000000f'$q$,
  $q$INSERT INTO sofom.transacciones (id, tipo, credito_id, caja_id, monto, clave_idempotencia)
     VALUES ('7e000000-0000-0000-0000-000000000018', 'COBRO_CREDITO', '4c000000-0000-0000-0000-000000000002',
             'f0000000-0000-0000-0000-00000000000f', 500, 'cobro-cobertura')$q$,
  $q$INSERT INTO sofom.cobros (transaccion_id, credito_id, principal, interes)
     VALUES ('7e000000-0000-0000-0000-000000000018', '4c000000-0000-0000-0000-000000000002', 500, 0)$q$,
  $q$UPDATE sofom.creditos SET saldo_insoluto = 500 WHERE id = '4c000000-0000-0000-0000-000000000002'$q$,
  'RESET ROLE'
]);
SELECT pg_temp.exigir('Cobertura: exactamente una marca DIFERIDA por perspectiva, con su canal',
  (SELECT array_agg(entidad || ' ' || sentido || ' ' || canal || ' ' || k.nombre ORDER BY e.creado_en)
     FROM aml.evaluaciones e JOIN kyc.clientes k ON k.id = e.cliente_id
    WHERE e.origen = 'DIFERIDA' AND e.resultado = 'PENDIENTE_REEVALUACION')
  = ARRAY['IFPE ENTRADA SPEI_DEMO Cob', 'IFPE ENTRADA SPEI_WEBHOOK Cob', 'IFPE SALIDA TRANSFERENCIA_ENVIADA Cob',
          'IFPE ENTRADA TRANSFERENCIA_RECIBIDA Bx', 'IFPE SALIDA SPEI_ENVIADO Cob', 'IFPE SALIDA INVERSION_MUTUO Cob',
          'SOFOM SALIDA DESEMBOLSO_CREDITO Cob', 'SOFOM ENTRADA COBRO_CREDITO Cob']
  AND (SELECT count(*) = 8 FROM aml.notificaciones WHERE tipo = 'COBERTURA' AND prioridad = 'ALTA')
  AND (SELECT count(*) = 8 FROM aml.bitacora WHERE evento = 'EVALUACION_DIFERIDA'));

-- -----------------------------------------------------------------------------
-- 8. Despacho SPEI: la orden nunca intentada de un congelado no se reclama
-- -----------------------------------------------------------------------------
SELECT pg_temp.paso('Bx', 'RETIRO', 100, '012180001234567899', 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Rel', 'RETIRO', 100, '012180001234567899', 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.debe_pasar('Despacho: reclamo por lotes de tres órdenes PENDIENTE, una de un congelado (sc_spei)', ARRAY[
  'SET LOCAL ROLE sc_spei',
  format($q$UPDATE ifpe.spei_outbound SET intentos_envio = intentos_envio + 1, proximo_intento_en = now() + interval '1 minute'
             WHERE estado = 'PENDIENTE' AND intentos_envio = 0 AND cliente_id IN (%L, %L, %L)$q$,
         pg_temp.c('Fz'), pg_temp.c('Bx'), pg_temp.c('Rel')),
  'RESET ROLE'
]);
SELECT pg_temp.exigir('Despacho: se reclamaron dos órdenes; la del congelado sigue PENDIENTE sin intentos',
  (SELECT array_agg(k.nombre || ':' || o.intentos_envio ORDER BY k.nombre) FROM ifpe.spei_outbound o
     JOIN kyc.clientes k ON k.id = o.cliente_id
    WHERE o.estado = 'PENDIENTE' AND k.nombre IN ('Fz', 'Bx', 'Rel'))
  = ARRAY['Bx:1', 'Fz:0', 'Rel:1']
  AND ifpe.aml_cliente_congelado(pg_temp.c('Fz')) AND NOT ifpe.aml_cliente_congelado(pg_temp.c('Bx')));

-- -----------------------------------------------------------------------------
-- 9. Máquinas de estados e inmutabilidad
-- -----------------------------------------------------------------------------
SELECT pg_temp.cliente('Est');
SELECT pg_temp.paso('Est', 'DEPOSITO', 80000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Est', 'DEPOSITO', 80000, NULL, 'PERMITIR', 65, 'ALTO');
SELECT pg_temp.debe_fallar('Alerta: PENDIENTE -> CONFIRMADO directo', ARRAY[
  format($q$UPDATE aml.alertas SET estado = 'CONFIRMADO', dictamen = repeat('d', 40), resuelta_por = %L, resuelta_en = now()
             WHERE cliente_id = %L$q$, pg_temp.c('Oficial1'), pg_temp.c('Est'))
], 'SC_AML_TRANSICION_INVALIDA');
SELECT pg_temp.debe_fallar('Alerta: una columna de identidad no cambia', ARRAY[
  format($q$UPDATE aml.alertas SET regla_codigo = 'R07_VOLUMEN_DIARIO' WHERE cliente_id = %L$q$, pg_temp.c('Est'))
], 'SC_AML_ALERTA_INMUTABLE');
SELECT pg_temp.debe_fallar('Alerta: el puntaje no baja mientras está abierta', ARRAY[
  format($q$UPDATE aml.alertas SET puntaje = 10 WHERE cliente_id = %L$q$, pg_temp.c('Est'))
], 'SC_AML_TRANSICION_INVALIDA');
SELECT pg_temp.debe_fallar('Alerta: la decisión no se debilita', ARRAY[
  format($q$UPDATE aml.alertas SET decision = 'REGISTRADA' WHERE cliente_id = %L AND decision = 'RETENIDA'$q$, pg_temp.c('Ana'))
], 'SC_AML_TRANSICION_INVALIDA');
SELECT pg_temp.debe_fallar('Alerta: el plazo UIF no se alarga', ARRAY[
  format($q$UPDATE aml.alertas SET fecha_limite_reporte = fecha_limite_reporte + interval '1 day' WHERE cliente_id = %L$q$,
         pg_temp.c('Est'))
], 'SC_AML_TRANSICION_INVALIDA');
SELECT pg_temp.debe_fallar('Alerta: no se borra', ARRAY['DELETE FROM aml.alertas'], 'SC_AML_ALERTA_INMUTABLE');
SELECT pg_temp.debe_fallar('Alerta: no se trunca', ARRAY['TRUNCATE aml.alertas CASCADE'], '_INMUTABLE');
SELECT pg_temp.debe_pasar('Alerta: el dueño la pone EN_REVISION con un oficial', ARRAY[
  format($q$UPDATE aml.alertas SET estado = 'EN_REVISION', asignada_a = %L, tomada_en = now() WHERE cliente_id = %L$q$,
         pg_temp.c('Oficial1'), pg_temp.c('Est'))
]);
SELECT pg_temp.debe_fallar('Alerta: cerrar sin dictamen', ARRAY[
  format($q$UPDATE aml.alertas SET estado = 'CONFIRMADO' WHERE cliente_id = %L$q$, pg_temp.c('Est'))
], 'ck_aml_alerta_cierre');
SELECT pg_temp.debe_fallar('Alerta: nadie resuelve sobre sí mismo', ARRAY[
  format($q$UPDATE aml.alertas SET asignada_a = cliente_id WHERE cliente_id = %L$q$, pg_temp.c('Est'))
], 'ck_aml_autoresolucion');
SELECT pg_temp.debe_fallar('Alerta: el folio UIF solo en una alerta CONFIRMADO', ARRAY[
  format($q$UPDATE aml.alertas SET folio_uif = 'SITI-1', reportada_uif_en = current_date WHERE cliente_id = %L$q$, pg_temp.c('Est'))
], 'ck_aml_uif');
SELECT pg_temp.debe_pasar('Alerta: el dueño la cierra como FALSO_POSITIVO con dictamen', ARRAY[
  format($q$UPDATE aml.alertas SET estado = 'FALSO_POSITIVO', dictamen = 'Ingresos por venta de un vehículo documentada',
                   resuelta_por = %L, resuelta_en = clock_timestamp() WHERE cliente_id = %L$q$,
         pg_temp.c('Oficial1'), pg_temp.c('Est'))
]);
SELECT pg_temp.debe_fallar('Alerta: una alerta cerrada no cambia', ARRAY[
  format($q$UPDATE aml.alertas SET dictamen = 'Otro dictamen distinto escrito después del cierre' WHERE cliente_id = %L$q$,
         pg_temp.c('Est'))
], 'SC_AML_ALERTA_INMUTABLE');
SELECT pg_temp.debe_fallar('Alerta: una alerta cerrada no se reabre', ARRAY[
  format($q$UPDATE aml.alertas SET estado = 'EN_REVISION' WHERE cliente_id = %L$q$, pg_temp.c('Est'))
], 'SC_AML_TRANSICION_INVALIDA');
SELECT pg_temp.debe_fallar('Alerta: una alerta FALSO_POSITIVO no lleva folio UIF', ARRAY[
  format($q$UPDATE aml.alertas SET folio_uif = 'SITI-2', reportada_uif_en = current_date WHERE cliente_id = %L$q$, pg_temp.c('Est'))
], 'SC_AML_ALERTA_INMUTABLE');
-- Marca de supresión: lo revisado como falso positivo no vuelve a contar.
SELECT pg_temp.paso('Est', 'DEPOSITO', 80000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.exigir('Alerta: tras el falso positivo, un tercer abono de 80k no reabre R02',
  pg_temp.alertas('Est') = ARRAY['R02_FRACCIONAMIENTO ALTO 65.00 REGISTRADA']
  AND (SELECT array_agg(evento ORDER BY id) FROM aml.bitacora WHERE alerta_id = (SELECT id FROM aml.alertas WHERE cliente_id = pg_temp.c('Est')))
      = ARRAY['ALERTA_CREADA', 'ALERTA_TOMADA', 'ALERTA_FALSO_POSITIVO']);

SELECT pg_temp.debe_fallar('Bitácora: UPDATE', ARRAY['UPDATE aml.bitacora SET motivo = $$x$$'], 'SC_AML_BITACORA_INMUTABLE');
SELECT pg_temp.debe_fallar('Bitácora: DELETE', ARRAY['DELETE FROM aml.bitacora'], 'SC_AML_BITACORA_INMUTABLE');
SELECT pg_temp.debe_fallar('Bitácora: TRUNCATE', ARRAY['TRUNCATE aml.bitacora'], 'SC_AML_BITACORA_INMUTABLE');
SELECT pg_temp.exigir('Bitácora: el hash de cada renglón es el canónico',
  NOT EXISTS (SELECT 1 FROM aml.bitacora b WHERE b.hash_contenido <> aml.hash_bitacora(b))
  AND (SELECT valido FROM aml.verificar_bitacora()));

SELECT pg_temp.debe_fallar('Congelamiento: LEVANTADO sin solicitud aprobada en la misma transacción', ARRAY[
  format($q$UPDATE aml.congelamientos SET estado = 'LEVANTADO', levantado_por = %L, levantado_en = now(),
                   solicitud_id = gen_random_uuid() WHERE cliente_id = %L$q$, pg_temp.c('Oficial2'), pg_temp.c('Fz'))
], 'SC_AML_LEVANTAMIENTO_SIN_APROBACION');
SELECT pg_temp.debe_fallar('Congelamiento: el SLA no se extiende (errata E7)', ARRAY[
  format($q$UPDATE aml.congelamientos SET sla_vence_en = sla_vence_en + interval '30 days' WHERE cliente_id = %L$q$,
         pg_temp.c('Fz'))
], 'SC_AML_REGISTRO_INMUTABLE');
SELECT pg_temp.debe_fallar('Congelamiento: los intentos bloqueados no disminuyen', ARRAY[
  format($q$UPDATE aml.congelamientos SET intentos_bloqueados = 0 WHERE cliente_id = %L$q$, pg_temp.c('Fz'))
], 'SC_AML_REGISTRO_INMUTABLE');
SELECT pg_temp.debe_fallar('Congelamiento: no se borra', ARRAY['DELETE FROM aml.congelamientos'], 'SC_AML_REGISTRO_INMUTABLE');
SELECT pg_temp.debe_fallar('Congelamiento: uno ACTIVO por cliente', ARRAY[
  format($q$INSERT INTO aml.congelamientos (cliente_id, origen, motivo, sla_vence_en)
            VALUES (%L, 'MANUAL', 'Segundo congelamiento del mismo cliente', now())$q$, pg_temp.c('Fz'))
], 'ux_aml_congelamiento_vigente');

SELECT pg_temp.debe_fallar('Regla: un parámetro de una versión no cambia', ARRAY[
  $q$UPDATE aml.reglas_riesgo SET umbral_monto = 1 WHERE codigo = 'R05_ESTRUCTURACION' AND nivel_cliente = 'MEDIO'$q$
], 'SC_AML_REGLA_INMUTABLE');
SELECT pg_temp.debe_fallar('Regla: desactivar sin una propuesta aprobada', ARRAY[
  $q$UPDATE aml.reglas_riesgo SET activa = FALSE, desactivada_en = now() WHERE codigo = 'R05_ESTRUCTURACION' AND activa$q$
], 'SC_AML_REGLA_SIN_APROBACION');
SELECT pg_temp.debe_fallar('Regla: una versión 2 sin propuesta aprobada', ARRAY[
  $q$INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte, puntos,
                                    umbral_monto)
     VALUES ('R03_CLIENTE_RIESGO_ALTO', 'MEDIO', 2, 'x', 'x', 'PRIMARIA', 'AMBOS', 'INUSUAL', 35, 1)$q$
], 'SC_AML_REGLA_SIN_APROBACION');
SELECT pg_temp.debe_fallar('Regla: no se borra', ARRAY['DELETE FROM aml.reglas_riesgo WHERE codigo = $$R00_LEGADO$$'],
  'SC_AML_REGLA_INMUTABLE');
SELECT pg_temp.debe_fallar('Regla: una ventana en línea no excede 24 h', ARRAY[
  $q$INSERT INTO aml.reglas_riesgo (codigo, nivel_cliente, version, nombre, descripcion, clase, sentido, tipo_reporte, puntos,
                                    volumen_max, ventana_horas)
     VALUES ('R07_VOLUMEN_DIARIO', 'TODOS', 1, 'x', 'x', 'PRIMARIA', 'AMBOS', 'INUSUAL', 25, 1, 48)$q$
], 'ck_aml_regla_ventana_en_linea');
SELECT pg_temp.debe_fallar('Parámetros: UPDATE', ARRAY['UPDATE aml.parametros SET modo_observacion = TRUE'],
  'SC_AML_PARAMETROS_INMUTABLE');
SELECT pg_temp.debe_fallar('Parámetros: DELETE', ARRAY['DELETE FROM aml.parametros'], 'SC_AML_PARAMETROS_INMUTABLE');
SELECT pg_temp.debe_fallar('Parámetros: una versión nueva sin propuesta aprobada', ARRAY[
  'INSERT INTO aml.parametros (modo_observacion) VALUES (TRUE)'
], 'SC_AML_REGLA_SIN_APROBACION');

SELECT pg_temp.debe_fallar('Legado: INSERT en kyc.alertas_pld', ARRAY[
  format($q$INSERT INTO kyc.alertas_pld (cliente_id, entidad, tipo_operacion, regla, nivel_riesgo, descripcion)
            VALUES (%L, 'IFPE', 'INUSUAL', 'R02', 'ALTO', 'x')$q$, pg_temp.c('Est'))
], 'SC_PLD_LEGADO_INMUTABLE');
-- Un renglón histórico (insertado con el trigger desactivado, como si fuera anterior a 0009).
ALTER TABLE kyc.alertas_pld DISABLE TRIGGER trg_alertas_pld_legado;
INSERT INTO kyc.alertas_pld (cliente_id, entidad, tipo_operacion, regla, nivel_riesgo, descripcion)
VALUES (pg_temp.c('Est'), 'IFPE', 'INUSUAL', 'R02_FRACCIONAMIENTO', 'ALTO', 'histórica');
ALTER TABLE kyc.alertas_pld ENABLE TRIGGER trg_alertas_pld_legado;
SELECT pg_temp.debe_fallar('Legado: UPDATE en kyc.alertas_pld', ARRAY['UPDATE kyc.alertas_pld SET analista = $$x$$'],
  'SC_PLD_LEGADO_INMUTABLE');
SELECT pg_temp.debe_fallar('Legado: DELETE en kyc.alertas_pld', ARRAY['DELETE FROM kyc.alertas_pld'], 'SC_PLD_LEGADO_INMUTABLE');
SELECT pg_temp.debe_fallar('Legado: TRUNCATE de kyc.alertas_pld', ARRAY['TRUNCATE kyc.alertas_pld CASCADE'], '_INMUTABLE');

SELECT pg_temp.debe_fallar('Evaluaciones: solo inserción', ARRAY['UPDATE aml.evaluaciones SET puntaje = 0'], 'SC_LEDGER_INMUTABLE');
SELECT pg_temp.debe_fallar('Notificaciones: solo se marca la entrega', ARRAY['UPDATE aml.notificaciones SET resumen = $$x$$'],
  'SC_AML_REGISTRO_INMUTABLE');
SELECT pg_temp.debe_fallar('Relevantes: solo el folio del reporte', ARRAY['UPDATE aml.operaciones_relevantes SET monto = 1'],
  'SC_AML_REGISTRO_INMUTABLE');

-- -----------------------------------------------------------------------------
-- 10. Funciones de los oficiales (como sc_aml): roles, cuatro ojos,
--     idempotencia, flujos completos y el contrato JSON (§4.2)
-- -----------------------------------------------------------------------------
CREATE FUNCTION pg_temp.alerta(p_cliente TEXT, p_regla TEXT) RETURNS UUID
LANGUAGE sql STABLE AS $$
    SELECT id FROM aml.alertas WHERE cliente_id = pg_temp.c(p_cliente) AND regla_codigo = p_regla ORDER BY folio DESC LIMIT 1
$$;
-- Llamada de un oficial a aml.resolver_alerta (sentencia SQL lista para ejecutarse).
CREATE FUNCTION pg_temp.resolver(p_oficial TEXT, p_cliente TEXT, p_regla TEXT, p_accion TEXT, p_dictamen TEXT, p_clave TEXT,
                                 p_urgente BOOLEAN DEFAULT FALSE, p_congelar BOOLEAN DEFAULT FALSE,
                                 p_tipo_levantamiento TEXT DEFAULT NULL) RETURNS TEXT
LANGUAGE sql STABLE AS $$
    SELECT format('SELECT aml.resolver_alerta(%L, %L, %L, %L, NULL, %L, %L, %L, %L)', pg_temp.c(p_oficial),
                  pg_temp.alerta(p_cliente, p_regla), p_accion, p_dictamen, p_urgente, p_congelar, p_tipo_levantamiento, p_clave)
$$;
CREATE TEMP TABLE r (k TEXT PRIMARY KEY, v JSONB);

SELECT pg_temp.debe_fallar('Oficiales: un OPERADOR no resuelve alertas', ARRAY[
  'SET LOCAL ROLE sc_aml', pg_temp.resolver('Operador', 'Ana', 'R05_ESTRUCTURACION', 'TOMAR', NULL, 'cum:op:1')
], 'SC_AML_NO_AUTORIZADO');
SELECT pg_temp.debe_fallar('Oficiales: un AUDITOR no escribe', ARRAY[
  'SET LOCAL ROLE sc_aml', pg_temp.resolver('Auditora', 'Ana', 'R05_ESTRUCTURACION', 'TOMAR', NULL, 'cum:aud:1')
], 'SC_AML_NO_AUTORIZADO');
SELECT pg_temp.debe_fallar('Oficiales: un cliente no lee expedientes', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format('SELECT aml.consultar_alerta(%L, %L)', pg_temp.c('Rel'), pg_temp.alerta('Ana', 'R05_ESTRUCTURACION'))
], 'SC_AML_NO_AUTORIZADO');
SELECT pg_temp.debe_fallar('Oficiales: exigir_lector rechaza a un OPERADOR', ARRAY[
  'SET LOCAL ROLE sc_aml', format('SELECT aml.exigir_lector(%L)', pg_temp.c('Operador'))
], 'SC_AML_NO_AUTORIZADO');
SELECT pg_temp.exigir('Oficiales: exigir_lector admite AUDITOR y CUMPLIMIENTO',
  pg_temp.como_aml(format('SELECT to_jsonb(aml.exigir_lector(%L))', pg_temp.c('Auditora'))) = '"AUDITOR"'
  AND pg_temp.como_aml(format('SELECT to_jsonb(aml.exigir_lector(%L))', pg_temp.c('Oficial1'))) = '"CUMPLIMIENTO"');
SELECT pg_temp.debe_fallar('Oficiales: alerta inexistente', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.resolver_alerta(%L, gen_random_uuid(), 'TOMAR', NULL, NULL, FALSE, FALSE, NULL, 'cum:o1:x')$q$,
         pg_temp.c('Oficial1'))
], 'SC_AML_NO_ENCONTRADO');

-- Falso positivo con cuatro ojos sobre Ana (TASK 4).
INSERT INTO r SELECT 'tomar', pg_temp.como_aml(pg_temp.resolver('Oficial1', 'Ana', 'R05_ESTRUCTURACION', 'TOMAR', NULL,
                                                                'cum:o1:tomar-r05'));
SELECT pg_temp.exigir('Oficiales: TOMAR deja la alerta EN_REVISION asignada al oficial',
  (SELECT v -> 'alerta' ->> 'estado' = 'EN_REVISION' AND v -> 'alerta' -> 'asignada_a' ->> 'id' = pg_temp.c('Oficial1')::text
          AND v ->> 'idempotente' = 'false' AND v -> 'congelamiento' ->> 'estado' = 'ACTIVO' AND v -> 'solicitud' = 'null'
     FROM r WHERE k = 'tomar'));
SELECT pg_temp.exigir('Oficiales: la misma clave y el mismo cuerpo responden idempotente',
  pg_temp.como_aml(pg_temp.resolver('Oficial1', 'Ana', 'R05_ESTRUCTURACION', 'TOMAR', NULL, 'cum:o1:tomar-r05'))
  = (SELECT v - 'idempotente' FROM r WHERE k = 'tomar') || '{"idempotente": true}');
SELECT pg_temp.debe_fallar('Oficiales: la misma clave con otro cuerpo', ARRAY[
  'SET LOCAL ROLE sc_aml', pg_temp.resolver('Oficial1', 'Ana', 'R02_FRACCIONAMIENTO', 'TOMAR', NULL, 'cum:o1:tomar-r05')
], 'SC_CLAVE_IDEMPOTENCIA_REUTILIZADA');
SELECT pg_temp.debe_fallar('Oficiales: otro oficial no toma una alerta asignada', ARRAY[
  'SET LOCAL ROLE sc_aml', pg_temp.resolver('Oficial2', 'Ana', 'R05_ESTRUCTURACION', 'TOMAR', NULL, 'cum:o2:tomar-r05')
], 'SC_AML_ASIGNADA_A_OTRO');
SELECT pg_temp.debe_fallar('Oficiales: CONFIRMAR una alerta PENDIENTE', ARRAY[
  'SET LOCAL ROLE sc_aml',
  pg_temp.resolver('Oficial1', 'Ana', 'R02_FRACCIONAMIENTO', 'CONFIRMAR', 'Operaciones fraccionadas sin justificación económica',
                   'cum:o1:confirmar-r02')
], 'SC_AML_TRANSICION_INVALIDA');
SELECT pg_temp.debe_fallar('Oficiales: un dictamen de menos de 30 caracteres', ARRAY[
  'SET LOCAL ROLE sc_aml', pg_temp.resolver('Oficial1', 'Ana', 'R05_ESTRUCTURACION', 'FALSO_POSITIVO', '  corto  ', 'cum:o1:fp-x')
], 'SC_AML_DICTAMEN_INVALIDO');
INSERT INTO r SELECT 'fp_r05', pg_temp.como_aml(pg_temp.resolver('Oficial1', 'Ana', 'R05_ESTRUCTURACION', 'FALSO_POSITIVO',
    'Depósitos de nómina de la empresa del cliente, con contrato y recibos', 'cum:o1:fp-r05'));
SELECT pg_temp.exigir('Oficiales: con el cliente congelado, el falso positivo queda propuesto y nace la solicitud',
  (SELECT v -> 'alerta' ->> 'estado' = 'EN_REVISION' AND v -> 'alerta' ->> 'resolucion_propuesta' = 'FALSO_POSITIVO'
          AND v -> 'solicitud' ->> 'estado' = 'PENDIENTE' AND v -> 'solicitud' ->> 'tipo' = 'LEVANTAR_Y_LIBERAR'
          AND v -> 'congelamiento' -> 'solicitud_pendiente' ->> 'id' = v -> 'solicitud' ->> 'id'
     FROM r WHERE k = 'fp_r05')
  AND aml.cliente_congelado(pg_temp.c('Ana')));
SELECT pg_temp.debe_fallar('Oficiales: LIBERAR con una propuesta de falso positivo pendiente', ARRAY[
  'SET LOCAL ROLE sc_aml', pg_temp.resolver('Oficial1', 'Ana', 'R05_ESTRUCTURACION', 'LIBERAR', NULL, 'cum:o1:liberar-r05')
], 'SC_AML_TRANSICION_INVALIDA');
SELECT pg_temp.debe_fallar('Oficiales: aprobar con una alerta ALTO abierta sin propuesta', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.decidir_levantamiento(%L, %L, TRUE, 'Revisión de origen de fondos completa', 'cum:o2:decide-1')$q$,
         pg_temp.c('Oficial2'), (SELECT v -> 'solicitud' ->> 'id' FROM r WHERE k = 'fp_r05'))
], 'SC_AML_ALERTAS_ABIERTAS');
SELECT pg_temp.como_aml(pg_temp.resolver('Oficial1', 'Ana', 'R02_FRACCIONAMIENTO', 'TOMAR', NULL, 'cum:o1:tomar-r02'));
INSERT INTO r SELECT 'fp_r02', pg_temp.como_aml(pg_temp.resolver('Oficial1', 'Ana', 'R02_FRACCIONAMIENTO', 'FALSO_POSITIVO',
    'Mismos depósitos de nómina; ya documentados en la alerta R05', 'cum:o1:fp-r02'));
SELECT pg_temp.exigir('Oficiales: el segundo falso positivo reutiliza la solicitud pendiente',
  (SELECT v -> 'solicitud' ->> 'id' FROM r WHERE k = 'fp_r02') = (SELECT v -> 'solicitud' ->> 'id' FROM r WHERE k = 'fp_r05')
  AND (SELECT count(*) = 1 FROM aml.solicitudes_levantamiento));
SELECT pg_temp.debe_fallar('Oficiales: quien propone no aprueba (cuatro ojos)', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.decidir_levantamiento(%L, %L, TRUE, 'Revisión de origen de fondos completa', 'cum:o1:decide-1')$q$,
         pg_temp.c('Oficial1'), (SELECT v -> 'solicitud' ->> 'id' FROM r WHERE k = 'fp_r05'))
], 'SC_AML_CUATRO_OJOS');
SELECT pg_temp.debe_fallar('Oficiales: un comentario de menos de 10 caracteres', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.decidir_levantamiento(%L, %L, TRUE, 'ok', 'cum:o2:decide-x')$q$,
         pg_temp.c('Oficial2'), (SELECT v -> 'solicitud' ->> 'id' FROM r WHERE k = 'fp_r05'))
], 'SC_AML_DICTAMEN_INVALIDO');
INSERT INTO r SELECT 'decide', pg_temp.como_aml(format(
    $q$SELECT aml.decidir_levantamiento(%L, %L, TRUE, 'Revisión de origen de fondos completa', 'cum:o2:decide-2')$q$,
    pg_temp.c('Oficial2'), (SELECT v -> 'solicitud' ->> 'id' FROM r WHERE k = 'fp_r05')));
SELECT pg_temp.exigir('Oficiales: la aprobación resuelve ambos falsos positivos, levanta el congelamiento y libera la retención',
  (SELECT v -> 'solicitud' ->> 'estado' = 'APROBADA' AND v -> 'congelamiento' ->> 'estado' = 'LEVANTADO'
          AND jsonb_array_length(v -> 'alertas_resueltas') = 2 AND jsonb_array_length(v -> 'liberaciones') = 1
          AND v -> 'liberaciones' -> 0 ->> 'monto' = '435000.00000000' AND v ->> 'idempotente' = 'false'
          AND v -> 'congelamiento' ->> 'retenido_vigente' = '0.00000000'
     FROM r WHERE k = 'decide')
  AND (SELECT bool_and(estado = 'FALSO_POSITIVO' AND resuelta_por = pg_temp.c('Oficial1') AND aprobada_por = pg_temp.c('Oficial2')
                       AND resolucion_propuesta IS NULL AND dictamen IS NOT NULL)
         FROM aml.alertas WHERE cliente_id = pg_temp.c('Ana'))
  AND (SELECT (estado, saldo_disponible, saldo_bloqueado) = ('ACTIVA', 435000::numeric, 0::numeric) FROM ifpe.cuentas
        WHERE id = pg_temp.cta('Ana'))
  AND NOT aml.cliente_congelado(pg_temp.c('Ana')));
SELECT pg_temp.exigir('Oficiales: la LIBERACION_RETENCION tiene clave lib:<id>, concepto neutro y hash válido; sc_aml la lee',
  (SELECT t.clave_idempotencia = 'lib:' || r.id AND t.concepto = 'Liberación de retención' AND r.estado = 'LIBERADA'
          AND t.tx_hash = nucleo.sha256_hex(ifpe.payload_canonico(t))
     FROM aml.retenciones r JOIN ifpe.transacciones t ON t.id = r.tx_liberacion_id WHERE r.cliente_id = pg_temp.c('Ana'))
  AND pg_temp.como_aml(format('SELECT jsonb_agg(m.clave_idempotencia) FROM aml.movimientos_ledger(ARRAY[%L, %L]::uuid[]) m',
                              (SELECT v -> 'liberaciones' -> 0 ->> 'transaccion_id' FROM r WHERE k = 'decide'),
                              (SELECT id FROM ifpe.transacciones WHERE tipo = 'DEPOSITO' LIMIT 1)))
      = (SELECT jsonb_build_array('lib:' || id) FROM aml.retenciones WHERE cliente_id = pg_temp.c('Ana')));
SELECT pg_temp.exigir('Oficiales: repetir la aprobación con la misma clave responde idempotente',
  pg_temp.como_aml(format(
      $q$SELECT aml.decidir_levantamiento(%L, %L, TRUE, 'Revisión de origen de fondos completa', 'cum:o2:decide-2')$q$,
      pg_temp.c('Oficial2'), (SELECT v -> 'solicitud' ->> 'id' FROM r WHERE k = 'fp_r05'))) ->> 'idempotente' = 'true');
SELECT pg_temp.debe_fallar('Oficiales: la misma clave de aprobación con otro comentario', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.decidir_levantamiento(%L, %L, TRUE, 'Otro comentario cualquiera', 'cum:o2:decide-2')$q$,
         pg_temp.c('Oficial2'), (SELECT v -> 'solicitud' ->> 'id' FROM r WHERE k = 'fp_r05'))
], 'SC_CLAVE_IDEMPOTENCIA_REUTILIZADA');
SELECT pg_temp.debe_fallar('Oficiales: una solicitud ya decidida', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.decidir_levantamiento(%L, %L, TRUE, 'Revisión de origen de fondos completa', 'cum:o2:decide-3')$q$,
         pg_temp.c('Oficial2'), (SELECT v -> 'solicitud' ->> 'id' FROM r WHERE k = 'fp_r05'))
], 'SC_AML_SOLICITUD_NO_PENDIENTE');
SELECT pg_temp.debe_fallar('Oficiales: una alerta cerrada no se vuelve a resolver', ARRAY[
  'SET LOCAL ROLE sc_aml', pg_temp.resolver('Oficial1', 'Ana', 'R05_ESTRUCTURACION', 'TOMAR', NULL, 'cum:o1:tomar-otra-vez')
], 'SC_AML_ALERTA_CERRADA');
-- La marca de supresión: un cuarto abono de 145k no reabre R05 ni R02 (R07 sí).
SELECT pg_temp.paso('Ana', 'DEPOSITO', 145000, NULL, 'PERMITIR', 40, 'MEDIO');
SELECT pg_temp.exigir('Oficiales: tras el falso positivo aprobado, sin R05 ni R02 nuevas ni congelamiento',
  (SELECT array_agg(regla_codigo || ' ' || estado ORDER BY folio) FROM aml.alertas WHERE cliente_id = pg_temp.c('Ana'))
  = ARRAY['R02_FRACCIONAMIENTO FALSO_POSITIVO', 'R05_ESTRUCTURACION FALSO_POSITIVO', 'R07_VOLUMEN_DIARIO PENDIENTE']
  AND NOT aml.cliente_congelado(pg_temp.c('Ana'))
  AND (SELECT count(*) = 1 FROM aml.retenciones WHERE cliente_id = pg_temp.c('Ana')));

-- Sin congelamiento, el falso positivo se resuelve de inmediato; LIBERAR devuelve la alerta a la bandeja.
SELECT pg_temp.como_aml(pg_temp.resolver('Oficial2', 'Mil', 'R07_VOLUMEN_DIARIO', 'TOMAR', NULL, 'cum:o2:mil-1'));
SELECT pg_temp.exigir('Oficiales: falso positivo inmediato de un cliente no congelado',
  (pg_temp.como_aml(pg_temp.resolver('Oficial2', 'Mil', 'R07_VOLUMEN_DIARIO', 'FALSO_POSITIVO',
                                     'Venta documentada de un inmueble con escritura pública', 'cum:o2:mil-2'))
     -> 'alerta' ->> 'estado') = 'FALSO_POSITIVO');
SELECT pg_temp.exigir('Oficiales: el falso positivo inmediato lo resuelve quien lo dictamina, sin aprobador',
  (SELECT resuelta_por = pg_temp.c('Oficial2') AND aprobada_por IS NULL AND resuelta_en IS NOT NULL
     FROM aml.alertas WHERE id = pg_temp.alerta('Mil', 'R07_VOLUMEN_DIARIO'))
  AND NOT EXISTS (SELECT 1 FROM aml.solicitudes_levantamiento s JOIN aml.congelamientos f ON f.id = s.congelamiento_id
                   WHERE f.cliente_id = pg_temp.c('Mil')));
SELECT pg_temp.como_aml(pg_temp.resolver('Oficial1', 'Pep', 'R03_CLIENTE_RIESGO_ALTO', 'TOMAR', NULL, 'cum:o1:pep-1'));
SELECT pg_temp.exigir('Oficiales: LIBERAR devuelve la alerta a PENDIENTE sin oficial asignado',
  (pg_temp.como_aml(pg_temp.resolver('Oficial1', 'Pep', 'R03_CLIENTE_RIESGO_ALTO', 'LIBERAR', NULL, 'cum:o1:pep-2'))
     -> 'alerta') @> '{"estado": "PENDIENTE", "asignada_a": null, "tomada_en": null}');

-- Confirmación (reporte de operación inusual) sobre Fan: anula la solicitud
-- pendiente, retira el falso positivo propuesto y eleva el riesgo del cliente.
SELECT pg_temp.como_aml(pg_temp.resolver('Oficial1', 'Fan', 'R05_ESTRUCTURACION', 'TOMAR', NULL, 'cum:o1:fan-1'));
SELECT pg_temp.como_aml(pg_temp.resolver('Oficial1', 'Fan', 'R05_ESTRUCTURACION', 'FALSO_POSITIVO',
                                         'Pagos a proveedores habituales del negocio del cliente', 'cum:o1:fan-2'));
SELECT pg_temp.como_aml(pg_temp.resolver('Oficial1', 'Fan', 'R07_VOLUMEN_DIARIO', 'TOMAR', NULL, 'cum:o1:fan-3'));
INSERT INTO r SELECT 'confirmar', pg_temp.como_aml(pg_temp.resolver('Oficial1', 'Fan', 'R07_VOLUMEN_DIARIO', 'CONFIRMAR',
    'Dispersión inmediata de fondos recibidos sin justificación económica', 'cum:o1:fan-4', TRUE));
SELECT pg_temp.exigir('Oficiales: CONFIRMAR urgente: plazo de 24 h, solicitud ANULADA, propuesta retirada, riesgo ALTO',
  (SELECT v -> 'alerta' ->> 'estado' = 'CONFIRMADO' AND (v -> 'alerta' ->> 'urgente')::boolean
          AND (v -> 'alerta' ->> 'fecha_limite_reporte')::timestamptz <= clock_timestamp() + interval '24 hours'
          AND v -> 'congelamiento' ->> 'confirmado' = 'true' AND v -> 'solicitud' ->> 'estado' = 'ANULADA'
          AND v -> 'solicitud' ->> 'comentario' LIKE 'Anulada por confirmación de la alerta %'
     FROM r WHERE k = 'confirmar')
  AND (SELECT estado = 'EN_REVISION' AND resolucion_propuesta IS NULL FROM aml.alertas
        WHERE id = pg_temp.alerta('Fan', 'R05_ESTRUCTURACION'))
  AND (SELECT nivel_riesgo_pld = 'ALTO' AND riesgo_evaluado_en IS NOT NULL FROM kyc.clientes WHERE id = pg_temp.c('Fan'))
  AND aml.cliente_congelado(pg_temp.c('Fan'))
  AND (SELECT array_agg(evento ORDER BY evento) FROM aml.bitacora WHERE cliente_id = pg_temp.c('Fan')
          AND evento IN ('RIESGO_CLIENTE_ELEVADO', 'LEVANTAMIENTO_ANULADO', 'ALERTA_FP_RETIRADO', 'ALERTA_CONFIRMADA',
                         'CONGELAMIENTO_CONFIRMADO'))
      = ARRAY['ALERTA_CONFIRMADA', 'ALERTA_FP_RETIRADO', 'CONGELAMIENTO_CONFIRMADO', 'LEVANTAMIENTO_ANULADO',
              'RIESGO_CLIENTE_ELEVADO']
  AND (SELECT actor_tipo = 'CUMPLIMIENTO' AND actor_id = pg_temp.c('Oficial1') AND rol_bd = 'sc_aml'
         FROM aml.bitacora WHERE evento = 'ALERTA_CONFIRMADA' AND cliente_id = pg_temp.c('Fan')));
SELECT pg_temp.debe_fallar('Reporte UIF: fecha anterior a la alerta', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.registrar_reporte_uif(%L, %L, 'SITI-2026-0001', current_date - 30, 'cum:o1:uif-0')$q$,
         pg_temp.c('Oficial1'), pg_temp.alerta('Fan', 'R07_VOLUMEN_DIARIO'))
], 'SC_AML_DICTAMEN_INVALIDO');
SELECT pg_temp.debe_fallar('Reporte UIF: solo de una alerta CONFIRMADO', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.registrar_reporte_uif(%L, %L, 'SITI-2026-0001', current_date, 'cum:o1:uif-x')$q$,
         pg_temp.c('Oficial1'), pg_temp.alerta('Fan', 'R05_ESTRUCTURACION'))
], 'SC_AML_TRANSICION_INVALIDA');
INSERT INTO r SELECT 'uif', pg_temp.como_aml(format(
    $q$SELECT aml.registrar_reporte_uif(%L, %L, 'SITI-2026-0001', (clock_timestamp() AT TIME ZONE 'America/Mexico_City')::date,
                                        'cum:o1:uif-1')$q$,
    pg_temp.c('Oficial1'), pg_temp.alerta('Fan', 'R07_VOLUMEN_DIARIO')));
SELECT pg_temp.exigir('Reporte UIF: folio registrado una vez',
  (SELECT v -> 'alerta' ->> 'folio_uif' = 'SITI-2026-0001' AND v -> 'alerta' ->> 'reportada_uif_en' ~ '^\d{4}-\d{2}-\d{2}$'
     FROM r WHERE k = 'uif'));
SELECT pg_temp.debe_fallar('Reporte UIF: un segundo folio', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.registrar_reporte_uif(%L, %L, 'SITI-2026-0002', current_date, 'cum:o1:uif-2')$q$,
         pg_temp.c('Oficial1'), pg_temp.alerta('Fan', 'R07_VOLUMEN_DIARIO'))
], 'SC_AML_FOLIO_REGISTRADO');

-- Congelamiento manual y escalamiento.
SELECT pg_temp.debe_fallar('Congelar: motivo de menos de 30 caracteres', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.congelar_manual(%L, %L, 'corto', NULL, 'cum:o1:cong-0')$q$, pg_temp.c('Oficial1'), pg_temp.c('Bx'))
], 'SC_AML_DICTAMEN_INVALIDO');
SELECT pg_temp.debe_fallar('Congelar: un oficial no se congela a sí mismo', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.congelar_manual(%L, %L, 'Motivo suficientemente largo para la prueba', NULL, 'cum:o1:cong-1')$q$,
         pg_temp.c('Oficial1'), pg_temp.c('Oficial1'))
], 'SC_AML_AUTORESOLUCION');
INSERT INTO r SELECT 'congelar', pg_temp.como_aml(format(
    $q$SELECT aml.congelar_manual(%L, %L, 'Información de autoridad sobre el cliente; revisión pendiente', NULL, 'cum:o1:cong-2')$q$,
    pg_temp.c('Oficial1'), pg_temp.c('Bx')));
SELECT pg_temp.exigir('Congelar: alerta R30 MANUAL, congelamiento MANUAL del oficial y cuenta CONGELADO_PREVENTIVO',
  (SELECT v ->> 'ya_congelado' = 'false' AND v ->> 'idempotente' = 'false' AND v -> 'alerta' -> 'regla' ->> 'codigo' = 'R30_CONGELAMIENTO_MANUAL'
          AND v -> 'alerta' ->> 'origen' = 'MANUAL' AND v -> 'congelamiento' ->> 'origen' = 'MANUAL'
          AND v -> 'congelamiento' -> 'congelado_por' ->> 'id' = pg_temp.c('Oficial1')::text
          AND v -> 'alerta' -> 'congelamiento' ->> 'id' = v -> 'congelamiento' ->> 'id'
     FROM r WHERE k = 'congelar')
  AND (SELECT estado = 'CONGELADO_PREVENTIVO' FROM ifpe.cuentas WHERE id = pg_temp.cta('Bx')));
SELECT pg_temp.exigir('Congelar: de nuevo con otra clave responde ya_congelado sin un segundo congelamiento',
  (pg_temp.como_aml(format(
      $q$SELECT aml.congelar_manual(%L, %L, 'Segundo intento de congelamiento del mismo cliente', NULL, 'cum:o1:cong-3')$q$,
      pg_temp.c('Oficial1'), pg_temp.c('Bx'))) ->> 'ya_congelado') = 'true'
  AND (SELECT count(*) = 1 FROM aml.congelamientos WHERE cliente_id = pg_temp.c('Bx')));
SELECT pg_temp.exigir('Congelar: el personal del banco es operación interna preocupante',
  (pg_temp.como_aml(format(
      $q$SELECT aml.congelar_manual(%L, %L, 'Conducta del empleado bajo investigación interna', NULL, 'cum:o1:cong-4')$q$,
      pg_temp.c('Oficial1'), pg_temp.c('Operador'))) -> 'alerta' ->> 'tipo_reporte') = 'INTERNA_PREOCUPANTE');
SELECT pg_temp.exigir('Congelar: un cliente sin cuenta IFPE se congela sin cuenta (el espejo no aplica)',
  (SELECT cuenta_ifpe_id IS NULL AND estado = 'ACTIVO' FROM aml.congelamientos WHERE cliente_id = pg_temp.c('Operador')));
SELECT pg_temp.como_aml(pg_temp.resolver('Oficial2', 'Rel', 'R02_FRACCIONAMIENTO', 'TOMAR', NULL, 'cum:o2:rel-1'));
INSERT INTO r SELECT 'escalar', pg_temp.como_aml(pg_temp.resolver('Oficial2', 'Rel', 'R02_FRACCIONAMIENTO', 'ESCALAR',
    'Patrón de fraccionamiento repetido; se congela mientras se investiga', 'cum:o2:rel-2', FALSE, TRUE));
SELECT pg_temp.exigir('Escalar: escalada, nivel CRITICO, congelamiento MANUAL y aviso ALERTA_CRITICA',
  (SELECT (v -> 'alerta' ->> 'escalada')::boolean AND v -> 'alerta' ->> 'nivel_riesgo' = 'CRITICO'
          AND v -> 'congelamiento' ->> 'origen' = 'MANUAL'
          AND v -> 'congelamiento' -> 'congelado_por' ->> 'id' = pg_temp.c('Oficial2')::text
     FROM r WHERE k = 'escalar')
  AND EXISTS (SELECT 1 FROM aml.notificaciones WHERE tipo = 'ALERTA_CRITICA' AND alerta_id = pg_temp.alerta('Rel', 'R02_FRACCIONAMIENTO'))
  AND (SELECT motivo LIKE 'Patrón de fraccionamiento%' FROM aml.bitacora WHERE evento = 'ALERTA_ESCALADA'
        AND alerta_id = pg_temp.alerta('Rel', 'R02_FRACCIONAMIENTO')));

-- Levantamiento parcial y liberación posterior (Fz: congelado sin alertas, con retenciones).
SELECT pg_temp.debe_fallar('Solicitud: LIBERAR_RETENCIONES de un congelamiento ACTIVO', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.solicitar_levantamiento(%L, %L, 'LIBERAR_RETENCIONES', 'Justificación suficientemente larga para la prueba',
                                               'cum:o1:sol-0')$q$,
         pg_temp.c('Oficial1'), (SELECT id FROM aml.congelamientos WHERE cliente_id = pg_temp.c('Fz')))
], 'SC_AML_SIN_RETENCIONES');
INSERT INTO r SELECT 'parcial', pg_temp.como_aml(format(
    $q$SELECT aml.solicitar_levantamiento(%L, %L, 'LEVANTAR_CONSERVANDO_RETENCIONES',
                                          'Sin indicios tras la revisión; las retenciones se conservan por ahora', 'cum:o1:sol-1')$q$,
    pg_temp.c('Oficial1'), (SELECT id FROM aml.congelamientos WHERE cliente_id = pg_temp.c('Fz'))));
SELECT pg_temp.debe_fallar('Solicitud: una segunda solicitud pendiente', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.solicitar_levantamiento(%L, %L, 'LEVANTAR_Y_LIBERAR', 'Justificación suficientemente larga para la prueba',
                                               'cum:o2:sol-2')$q$,
         pg_temp.c('Oficial2'), (SELECT id FROM aml.congelamientos WHERE cliente_id = pg_temp.c('Fz')))
], 'SC_AML_SOLICITUD_PENDIENTE');
SELECT pg_temp.debe_fallar('Solicitud: quien solicita no aprueba', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.decidir_levantamiento(%L, %L, TRUE, 'Aprobación del mismo oficial', 'cum:o1:sol-3')$q$,
         pg_temp.c('Oficial1'), (SELECT v -> 'solicitud' ->> 'id' FROM r WHERE k = 'parcial'))
], 'SC_AML_CUATRO_OJOS');
INSERT INTO r SELECT 'parcial_ok', pg_temp.como_aml(format(
    $q$SELECT aml.decidir_levantamiento(%L, %L, TRUE, 'De acuerdo con conservar las retenciones', 'cum:o2:sol-4')$q$,
    pg_temp.c('Oficial2'), (SELECT v -> 'solicitud' ->> 'id' FROM r WHERE k = 'parcial')));
SELECT pg_temp.exigir('Solicitud: levantamiento conservando retenciones (cuenta ACTIVA, 5,000 aún retenidos)',
  (SELECT v -> 'congelamiento' ->> 'estado' = 'LEVANTADO' AND v -> 'liberaciones' = '[]'
          AND v -> 'congelamiento' ->> 'retenido_vigente' = '5000.00000000'
     FROM r WHERE k = 'parcial_ok')
  AND (SELECT (estado, saldo_bloqueado) = ('ACTIVA', 5000::numeric) FROM ifpe.cuentas WHERE id = pg_temp.cta('Fz'))
  AND (SELECT count(*) = 2 FROM aml.retenciones WHERE cliente_id = pg_temp.c('Fz') AND estado = 'VIGENTE'));
SELECT pg_temp.paso('Fz', 'RETIRO', 100, '012180001234567899', 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.debe_fallar('Solicitud: LEVANTAR un congelamiento ya levantado', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.solicitar_levantamiento(%L, %L, 'LEVANTAR_Y_LIBERAR', 'Justificación suficientemente larga para la prueba',
                                               'cum:o1:sol-5')$q$,
         pg_temp.c('Oficial1'), (SELECT id FROM aml.congelamientos WHERE cliente_id = pg_temp.c('Fz')))
], 'SC_AML_CONGELAMIENTO_NO_ACTIVO');
INSERT INTO r SELECT 'liberar', pg_temp.como_aml(format(
    $q$SELECT aml.solicitar_levantamiento(%L, %L, 'LIBERAR_RETENCIONES',
                                          'Documentación del origen de los abonos retenidos recibida', 'cum:o1:sol-6')$q$,
    pg_temp.c('Oficial1'), (SELECT id FROM aml.congelamientos WHERE cliente_id = pg_temp.c('Fz'))));
SELECT pg_temp.como_aml(format($q$SELECT aml.decidir_levantamiento(%L, %L, FALSE, 'Falta un comprobante del segundo abono',
                                                                    'cum:o2:sol-7')$q$,
                               pg_temp.c('Oficial2'), (SELECT v -> 'solicitud' ->> 'id' FROM r WHERE k = 'liberar')));
SELECT pg_temp.exigir('Solicitud: rechazada, las retenciones siguen',
  (SELECT estado = 'RECHAZADA' AND decidida_por = pg_temp.c('Oficial2') FROM aml.solicitudes_levantamiento
    WHERE id = (SELECT (v -> 'solicitud' ->> 'id')::uuid FROM r WHERE k = 'liberar'))
  AND (SELECT count(*) = 2 FROM aml.retenciones WHERE cliente_id = pg_temp.c('Fz') AND estado = 'VIGENTE'));
INSERT INTO r SELECT 'liberar2', pg_temp.como_aml(format(
    $q$SELECT aml.solicitar_levantamiento(%L, %L, 'LIBERAR_RETENCIONES',
                                          'Comprobantes completos de ambos abonos retenidos', 'cum:o1:sol-8')$q$,
    pg_temp.c('Oficial1'), (SELECT id FROM aml.congelamientos WHERE cliente_id = pg_temp.c('Fz'))));
INSERT INTO r SELECT 'liberar2_ok', pg_temp.como_aml(format(
    $q$SELECT aml.decidir_levantamiento(%L, %L, TRUE, 'Comprobantes verificados', 'cum:o2:sol-9')$q$,
    pg_temp.c('Oficial2'), (SELECT v -> 'solicitud' ->> 'id' FROM r WHERE k = 'liberar2')));
SELECT pg_temp.exigir('Solicitud: liberación posterior de las retenciones conservadas',
  (SELECT jsonb_array_length(v -> 'liberaciones') = 2 FROM r WHERE k = 'liberar2_ok')
  AND (SELECT saldo_bloqueado = 0 FROM ifpe.cuentas WHERE id = pg_temp.cta('Fz'))
  AND (SELECT bool_and(estado = 'LIBERADA') FROM aml.retenciones WHERE cliente_id = pg_temp.c('Fz')));

-- Propuestas de cambio (cuatro ojos) y modo observación.
SELECT pg_temp.debe_fallar('Propuesta: una clave no modificable', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.proponer_cambio(%L, 'REGLA', 'R05_ESTRUCTURACION', 'MEDIO', '{"codigo": "X"}',
                                       'Justificación suficientemente larga para la prueba', 'cum:o1:prop-0')$q$, pg_temp.c('Oficial1'))
], 'SC_AML_CAMBIO_INVALIDO');
SELECT pg_temp.debe_fallar('Propuesta: un tipo JSON equivocado', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.proponer_cambio(%L, 'REGLA', 'R05_ESTRUCTURACION', 'MEDIO', '{"frecuencia_min": "4"}',
                                       'Justificación suficientemente larga para la prueba', 'cum:o1:prop-1')$q$, pg_temp.c('Oficial1'))
], 'SC_AML_CAMBIO_INVALIDO');
SELECT pg_temp.debe_fallar('Propuesta: la Lista de Personas Bloqueadas no se modifica', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.proponer_cambio(%L, 'REGLA', 'R09_LISTA_BLOQUEADAS', 'TODOS', '{"activa": false}',
                                       'Justificación suficientemente larga para la prueba', 'cum:o1:prop-2')$q$, pg_temp.c('Oficial1'))
], 'SC_AML_CAMBIO_INVALIDO');
SELECT pg_temp.debe_fallar('Propuesta: umbrales que dejan de ser crecientes', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.proponer_cambio(%L, 'PARAMETROS', NULL, NULL, '{"umbral_alto": "90"}',
                                       'Justificación suficientemente larga para la prueba', 'cum:o1:prop-3')$q$, pg_temp.c('Oficial1'))
], 'SC_AML_CAMBIO_INVALIDO');
SELECT pg_temp.debe_fallar('Propuesta: una regla sin versión activa para ese nivel', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.proponer_cambio(%L, 'REGLA', 'R03_CLIENTE_RIESGO_ALTO', 'MEDIO', '{"puntos": "40"}',
                                       'Justificación suficientemente larga para la prueba', 'cum:o1:prop-4')$q$, pg_temp.c('Oficial1'))
], 'SC_AML_NO_ENCONTRADO');
INSERT INTO r SELECT 'prop_r05', pg_temp.como_aml(format(
    $q$SELECT aml.proponer_cambio(%L, 'REGLA', 'R05_ESTRUCTURACION', 'MEDIO', '{"frecuencia_min": 4}',
                                  'Calibración: la frecuencia 3 genera demasiados falsos positivos de nómina', 'cum:o1:prop-5')$q$,
    pg_temp.c('Oficial1')));
SELECT pg_temp.debe_fallar('Propuesta: quien propone no decide', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.decidir_propuesta(%L, %L, TRUE, 'Aprobación propia', 'cum:o1:prop-6')$q$,
         pg_temp.c('Oficial1'), (SELECT v -> 'propuesta' ->> 'id' FROM r WHERE k = 'prop_r05'))
], 'SC_AML_CUATRO_OJOS');
INSERT INTO r SELECT 'prop_r05_ok', pg_temp.como_aml(format(
    $q$SELECT aml.decidir_propuesta(%L, %L, TRUE, 'De acuerdo con la calibración', 'cum:o2:prop-7')$q$,
    pg_temp.c('Oficial2'), (SELECT v -> 'propuesta' ->> 'id' FROM r WHERE k = 'prop_r05')));
SELECT pg_temp.exigir('Propuesta: R05 MEDIO versión 2 activa (frecuencia 4); la versión 1 queda inactiva con su fecha',
  (SELECT v -> 'regla' ->> 'version' = '2' AND v -> 'regla' ->> 'frecuencia_min' = '4' AND v -> 'propuesta' ->> 'estado' = 'APROBADA'
          AND v -> 'propuesta' ->> 'resultado_id' = v -> 'regla' ->> 'id' AND v -> 'parametros' = 'null'
     FROM r WHERE k = 'prop_r05_ok')
  AND (SELECT NOT activa AND desactivada_en IS NOT NULL FROM aml.reglas_riesgo
        WHERE codigo = 'R05_ESTRUCTURACION' AND nivel_cliente = 'MEDIO' AND version = 1)
  AND (SELECT frecuencia_min = 4 AND version = 2 FROM aml.regla('R05_ESTRUCTURACION', 'MEDIO'))
  AND EXISTS (SELECT 1 FROM aml.bitacora WHERE evento = 'REGLA_DESACTIVADA')
  AND EXISTS (SELECT 1 FROM aml.bitacora WHERE evento = 'PROPUESTA_APROBADA'));
SELECT pg_temp.debe_fallar('Propuesta: una propuesta ya decidida', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format($q$SELECT aml.decidir_propuesta(%L, %L, TRUE, 'Otra vez la misma', 'cum:o2:prop-8')$q$,
         pg_temp.c('Oficial2'), (SELECT v -> 'propuesta' ->> 'id' FROM r WHERE k = 'prop_r05'))
], 'SC_AML_PROPUESTA_NO_PENDIENTE');
SELECT pg_temp.cliente('Ana2');
SELECT pg_temp.paso('Ana2', 'DEPOSITO', 145000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Ana2', 'DEPOSITO', 145000, NULL, 'PERMITIR', 65, 'ALTO');
SELECT pg_temp.paso('Ana2', 'DEPOSITO', 145000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Ana2', 'DEPOSITO', 145000, NULL, 'RETENER', 100, 'CRITICO');
SELECT pg_temp.exigir('Propuesta: con frecuencia 4, R05 dispara al cuarto abono y la evaluación cita la versión 2',
  (SELECT (SELECT id FROM aml.regla('R05_ESTRUCTURACION', 'MEDIO')) = ANY (reglas_aplicadas)
     FROM aml.evaluaciones WHERE cliente_id = pg_temp.c('Ana2') AND resultado = 'RETENIDA'));
INSERT INTO r SELECT 'prop_obs', pg_temp.como_aml(format(
    $q$SELECT aml.proponer_cambio(%L, 'PARAMETROS', NULL, NULL, '{"modo_observacion": true}',
                                  'Despliegue gradual: observar dos semanas antes de congelar en automático', 'cum:o1:prop-9')$q$,
    pg_temp.c('Oficial1')));
INSERT INTO r SELECT 'prop_obs_ok', pg_temp.como_aml(format(
    $q$SELECT aml.decidir_propuesta(%L, %L, TRUE, 'De acuerdo con el despliegue gradual', 'cum:o2:prop-10')$q$,
    pg_temp.c('Oficial2'), (SELECT v -> 'propuesta' ->> 'id' FROM r WHERE k = 'prop_obs')));
-- (El id de una versión nueva puede saltar números: la identidad no es densa.)
SELECT pg_temp.exigir('Propuesta: parámetros nuevos en modo observación (alertar_desde copiado, diferencia en la bitácora)',
  (SELECT (v -> 'parametros' ->> 'id')::bigint = (SELECT max(id) FROM aml.parametros)
          AND (v -> 'parametros' ->> 'modo_observacion')::boolean
          AND v -> 'propuesta' ->> 'resultado_id' = v -> 'parametros' ->> 'id'
     FROM r WHERE k = 'prop_obs_ok')
  AND (SELECT count(*) = 2 FROM aml.parametros)
  AND (SELECT p2.alertar_desde = p1.alertar_desde AND p2.propuesta_id IS NOT NULL AND p2.modo_observacion
         FROM aml.parametros p1, aml.parametros p2 WHERE p1.id = 1 AND p2.id = (SELECT max(id) FROM aml.parametros))
  AND (SELECT datos -> 'diferencia' ? 'modo_observacion' AND NOT (datos -> 'diferencia' ? 'umbral_alto')
         FROM aml.bitacora WHERE evento = 'PARAMETROS_VERSION_NUEVA' AND objeto_id <> '1'));
SELECT pg_temp.cliente('Obs');
SELECT pg_temp.paso('Obs', 'DEPOSITO', 145000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Obs', 'DEPOSITO', 145000, NULL, 'PERMITIR', 65, 'ALTO');
SELECT pg_temp.paso('Obs', 'DEPOSITO', 145000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.paso('Obs', 'DEPOSITO', 145000, NULL, 'PERMITIR', 100, 'CRITICO');
SELECT pg_temp.exigir('Observación: alerta CRITICO REGISTRADA sin congelamiento ni retención',
  (SELECT decision = 'REGISTRADA' AND nivel_riesgo = 'CRITICO' FROM aml.alertas WHERE id = pg_temp.alerta('Obs', 'R05_ESTRUCTURACION'))
  AND NOT aml.cliente_congelado(pg_temp.c('Obs'))
  AND NOT EXISTS (SELECT 1 FROM aml.retenciones WHERE cliente_id = pg_temp.c('Obs')));
INSERT INTO r SELECT 'prop_rechazo', pg_temp.como_aml(format(
    $q$SELECT aml.proponer_cambio(%L, 'REGLA', 'R07_VOLUMEN_DIARIO', 'MEDIO', '{"puntos": "30"}',
                                  'Subir el peso del volumen diario para clientes de riesgo medio', 'cum:o1:prop-11')$q$,
    pg_temp.c('Oficial1')));
SELECT pg_temp.exigir('Propuesta: rechazada sin versión nueva',
  (pg_temp.como_aml(format($q$SELECT aml.decidir_propuesta(%L, %L, FALSE, 'Sin evidencia que lo respalde', 'cum:o2:prop-12')$q$,
                           pg_temp.c('Oficial2'), (SELECT v -> 'propuesta' ->> 'id' FROM r WHERE k = 'prop_rechazo')))
     -> 'propuesta' ->> 'estado') = 'RECHAZADA'
  AND (SELECT version = 1 FROM aml.regla('R07_VOLUMEN_DIARIO', 'MEDIO')));

-- Lectura del expediente (deja constancia del lector).
INSERT INTO r SELECT 'detalle', pg_temp.como_aml(format('SELECT aml.consultar_alerta(%L, %L)', pg_temp.c('Auditora'),
                                                        pg_temp.alerta('Ana', 'R05_ESTRUCTURACION')));
INSERT INTO r SELECT 'revision', pg_temp.como_aml(format('SELECT aml.consultar_cliente(%L, %L)', pg_temp.c('Oficial1'),
                                                         pg_temp.c('Fz')));
SELECT pg_temp.exigir('Expediente: la consulta queda en la bitácora con su lector',
  (SELECT count(*) = 1 FROM aml.bitacora WHERE evento = 'CONSULTA_EXPEDIENTE' AND actor_tipo = 'AUDITOR' AND objeto = 'ALERTA')
  AND (SELECT count(*) = 1 FROM aml.bitacora WHERE evento = 'CONSULTA_EXPEDIENTE' AND actor_tipo = 'CUMPLIMIENTO'
          AND objeto = 'CLIENTE' AND cliente_id = pg_temp.c('Fz')));
SELECT pg_temp.exigir('Expediente: evaluaciones, movimientos del patrón (retenido), historial y otras alertas',
  (SELECT jsonb_array_length(v -> 'evaluaciones') = 1 AND jsonb_array_length(v -> 'movimientos') = 3
          AND jsonb_array_length(v -> 'otras_alertas') = 2 AND jsonb_array_length(v -> 'historial') > 5
          AND v -> 'congelamiento' ->> 'estado' = 'LEVANTADO' AND v -> 'cuentas' -> 'ifpe' ->> 'estado' = 'ACTIVA'
          AND v -> 'alerta' ->> 'dictamen_propuesto' IS NULL AND jsonb_typeof(v -> 'alerta' -> 'detalles') = 'object'
     FROM r WHERE k = 'detalle'));

-- Contrato JSON: las llaves de cada constructor y de cada lectura (§4.2).
SELECT pg_temp.exigir('JSON: PersonaRefAml', pg_temp.mismas_claves(aml.persona_json(pg_temp.c('Oficial1')), ARRAY['id', 'nombre_completo'])
  AND aml.persona_json(NULL) IS NULL);
SELECT pg_temp.exigir('JSON: AlertaAml (y regla, cliente, congelamiento)',
  pg_temp.mismas_claves(aml.alerta_json(pg_temp.alerta('Ana', 'R05_ESTRUCTURACION')), ARRAY[
    'id', 'folio', 'estado', 'nivel_riesgo', 'puntaje', 'tipo_reporte', 'origen', 'decision', 'regla', 'cliente', 'entidad',
    'sentido', 'transaccion_id', 'monto', 'monto_acumulado', 'ocurrencias', 'timestamp', 'ultima_ocurrencia_en',
    'actualizado_en', 'fecha_limite_reporte', 'urgente', 'congelamiento', 'asignada_a', 'tomada_en', 'escalada',
    'resolucion_propuesta', 'propuesta_por', 'dictamen', 'resuelta_por', 'resuelta_en', 'aprobada_por', 'folio_uif',
    'reportada_uif_en'])
  AND pg_temp.mismas_claves(aml.alerta_json(pg_temp.alerta('Ana', 'R05_ESTRUCTURACION')) -> 'regla',
                            ARRAY['id', 'codigo', 'version', 'nombre'])
  AND pg_temp.mismas_claves(aml.alerta_json(pg_temp.alerta('Ana', 'R05_ESTRUCTURACION')) -> 'cliente',
                            ARRAY['id', 'nombre_completo', 'nivel_riesgo_pld', 'es_pep', 'es_interno'])
  AND pg_temp.mismas_claves(aml.alerta_json(pg_temp.alerta('Ana', 'R05_ESTRUCTURACION')) -> 'congelamiento',
                            ARRAY['id', 'folio', 'estado', 'confirmado', 'sla_vence_en'])
  AND aml.alerta_json(pg_temp.alerta('Ana', 'R05_ESTRUCTURACION')) ->> 'puntaje' = '100.00'
  AND aml.alerta_json(pg_temp.alerta('Ana', 'R05_ESTRUCTURACION')) ->> 'monto' = '145000.00000000');
SELECT pg_temp.exigir('JSON: CongelamientoAml, RetencionAml y SolicitudAml',
  pg_temp.mismas_claves(aml.congelamiento_json((SELECT id FROM aml.congelamientos WHERE cliente_id = pg_temp.c('Ana'))), ARRAY[
    'id', 'folio', 'cliente', 'cuenta_ifpe_id', 'estado', 'origen', 'alerta_id', 'evaluacion_id', 'puntaje', 'motivo',
    'confirmado', 'congelado_por', 'congelado_en', 'sla_vence_en', 'intentos_bloqueados', 'ultimo_intento_en',
    'levantado_por', 'levantado_en', 'solicitud_id', 'retenido_vigente', 'retenciones', 'solicitud_pendiente',
    'actualizado_en'])
  AND pg_temp.mismas_claves(aml.retencion_json((SELECT id FROM aml.retenciones WHERE cliente_id = pg_temp.c('Ana'))), ARRAY[
    'id', 'congelamiento_id', 'cuenta_id', 'evaluacion_id', 'transaccion_origen_id', 'tx_retencion_id', 'monto', 'estado',
    'solicitud_id', 'tx_liberacion_id', 'creado_en', 'liberada_en'])
  AND pg_temp.mismas_claves(aml.solicitud_json((SELECT (v -> 'solicitud' ->> 'id')::uuid FROM r WHERE k = 'fp_r05')), ARRAY[
    'id', 'folio', 'congelamiento_id', 'cliente', 'tipo', 'justificacion', 'solicitada_por', 'solicitada_en', 'estado',
    'decidida_por', 'decidida_en', 'comentario']));
SELECT pg_temp.exigir('JSON: ClienteAml y CuentasAml',
  pg_temp.mismas_claves(aml.cliente_json(pg_temp.c('Ana')), ARRAY[
    'id', 'tipo_persona', 'nombre_completo', 'email', 'curp', 'rfc', 'fecha_nacimiento', 'nacionalidad', 'pais_residencia',
    'entidad_nacimiento', 'domicilio', 'actividad_economica', 'ocupacion', 'ingreso_mensual_estimado', 'nivel_riesgo_pld',
    'riesgo_evaluado_en', 'es_pep', 'en_lista_bloqueadas', 'estatus_kyc', 'kyc_verificado_en', 'es_interno', 'cliente_desde'])
  AND pg_temp.mismas_claves(aml.cliente_json(pg_temp.c('Ana')) -> 'domicilio', ARRAY['estado', 'municipio', 'codigo_postal'])
  AND pg_temp.mismas_claves(aml.cuentas_json(pg_temp.c('Mut')), ARRAY['ifpe', 'sofom'])
  AND pg_temp.mismas_claves(aml.cuentas_json(pg_temp.c('Mut')) -> 'ifpe', ARRAY[
    'id', 'clabe', 'estado', 'saldo_disponible', 'saldo_bloqueado', 'retenido_aml', 'creado_en'])
  AND pg_temp.mismas_claves(aml.cuentas_json(pg_temp.c('Mut')) -> 'sofom', ARRAY[
    'id', 'estado', 'saldo_disponible', 'saldo_bloqueado', 'creado_en'])
  AND aml.cuentas_json(pg_temp.c('Ana')) -> 'sofom' = 'null'
  AND aml.cuentas_json(pg_temp.c('Mut')) -> 'ifpe' ->> 'retenido_aml' = '0.00000000');
SELECT pg_temp.exigir('JSON: ReglaAml, ParametrosAml y PropuestaAml',
  pg_temp.mismas_claves(aml.regla_json((SELECT id FROM aml.regla('M02_GEOGRAFIA', 'MEDIO'))), ARRAY[
    'id', 'codigo', 'nivel_cliente', 'version', 'nombre', 'descripcion', 'clase', 'sentido', 'tipo_reporte', 'activa', 'puntos',
    'permite_congelar', 'umbral_monto', 'banda_inferior', 'frecuencia_min', 'ventana_horas', 'volumen_max',
    'operaciones_max', 'multiplo_ingreso', 'porcentaje_min', 'contrapartes_min', 'dias_antiguedad', 'paises_alto_riesgo',
    'entidades_alto_riesgo', 'estados_alto_riesgo', 'propuesta_id', 'creado_en', 'desactivada_en'])
  AND aml.regla_json((SELECT id FROM aml.regla('M02_GEOGRAFIA', 'MEDIO'))) -> 'paises_alto_riesgo' = '["KP", "IR", "MM"]'
  AND pg_temp.mismas_claves(aml.parametros_json(), ARRAY[
    'id', 'umbral_medio', 'umbral_alto', 'umbral_critico', 'modo_observacion', 'sla_critica_horas', 'sla_congelamiento_horas',
    'dias_reporte_inusual', 'horas_reporte_urgente', 'dias_aviso_plazo', 'alertar_desde', 'propuesta_id', 'creado_en'])
  AND pg_temp.mismas_claves(aml.propuesta_json((SELECT (v -> 'propuesta' ->> 'id')::uuid FROM r WHERE k = 'prop_r05')), ARRAY[
    'id', 'folio', 'tipo', 'regla_codigo', 'nivel_cliente', 'cambios', 'justificacion', 'propuesta_por', 'propuesta_en',
    'estado', 'decidida_por', 'decidida_en', 'comentario', 'resultado_id']));
SELECT pg_temp.exigir('JSON: EvaluacionAml, ContraparteAml, CalculoAml (perfil, ventana) y FactorAml',
  (SELECT pg_temp.mismas_claves(aml.evaluacion_json(e.id), ARRAY[
            'id', 'creado_en', 'cliente_id', 'entidad', 'sentido', 'canal', 'monto', 'transaccion_id', 'clave_operacion',
            'contraparte', 'puntaje', 'nivel', 'resultado', 'motivo_bloqueo', 'origen', 'evaluacion_origen_id', 'factores',
            'reglas_aplicadas', 'parametros_id', 'monto_retener'])
          AND pg_temp.mismas_claves(aml.evaluacion_json(e.id) -> 'contraparte', ARRAY['nombre', 'cuenta', 'rfc_curp', 'cliente_id'])
          AND pg_temp.mismas_claves(e.factores, ARRAY[
            'version', 'parametros_id', 'ahora', 'nivel_cliente', 'perfil', 'ventana', 'factores', 'primarias',
            'reglas_aplicadas', 'puntaje_bruto', 'puntaje', 'nivel', 'dominante', 'dominante_regla_id', 'congelar',
            'relevante', 'regla_relevante_id', 'transacciones_patron', 'monto_patron'])
          AND pg_temp.mismas_claves(e.factores -> 'perfil', ARRAY[
            'nivel_riesgo_pld', 'es_pep', 'nacionalidad', 'pais_residencia', 'entidad_nacimiento', 'estado_domicilio',
            'ingreso_mensual_estimado', 'antiguedad_dias', 'es_interno'])
          AND pg_temp.mismas_claves(e.factores -> 'ventana', ARRAY[
            'desde', 'hasta', 'horas', 'entradas_n', 'entradas_monto', 'salidas_n', 'salidas_monto', 'operaciones_n', 'volumen'])
          AND (SELECT bool_and(pg_temp.mismas_claves(f, ARRAY['codigo', 'regla_id', 'version', 'tipo', 'puntos',
                                                               'permite_congelar', 'evidencia']))
                 FROM jsonb_array_elements(e.factores -> 'factores') f)
          AND aml.evaluacion_json(e.id) -> 'contraparte' ->> 'cuenta' = '012180001234567899'
     FROM aml.evaluaciones e WHERE e.cliente_id = pg_temp.c('Ana') AND e.resultado = 'RETENIDA')
  AND (SELECT factores -> 'dominante' = 'null' AND factores -> 'dominante_regla_id' = 'null'
         FROM aml.evaluaciones WHERE cliente_id = pg_temp.c('Rel') ORDER BY creado_en LIMIT 1));
SELECT pg_temp.exigir('JSON: ResumenAml',
  pg_temp.mismas_claves(aml.resumen(), ARRAY[
    'pendientes', 'en_revision', 'abiertas_por_nivel', 'criticas_abiertas', 'congelamientos_activos', 'solicitudes_pendientes',
    'propuestas_pendientes', 'sla_congelamiento_vencidos', 'reportes_por_vencer', 'reportes_vencidos',
    'notificaciones_sin_entregar', 'modo_observacion', 'ultimo_ciclo_en', 'servidor_en'])
  AND pg_temp.mismas_claves(aml.resumen() -> 'abiertas_por_nivel', ARRAY['BAJO', 'MEDIO', 'ALTO', 'CRITICO'])
  AND (aml.resumen() ->> 'modo_observacion')::boolean
  AND (aml.resumen() ->> 'congelamientos_activos')::int = (SELECT count(*) FROM aml.congelamientos WHERE estado = 'ACTIVO'));
SELECT pg_temp.exigir('JSON: DetalleAlertaAml, MovimientoAml y BitacoraAml',
  (SELECT pg_temp.mismas_claves(v, ARRAY['alerta', 'evaluaciones', 'movimientos', 'cliente', 'cuentas', 'congelamiento',
                                         'otras_alertas', 'historial'])
          AND pg_temp.mismas_claves(v -> 'alerta', ARRAY[
            'id', 'folio', 'estado', 'nivel_riesgo', 'puntaje', 'tipo_reporte', 'origen', 'decision', 'regla', 'cliente',
            'entidad', 'sentido', 'transaccion_id', 'monto', 'monto_acumulado', 'ocurrencias', 'timestamp',
            'ultima_ocurrencia_en', 'actualizado_en', 'fecha_limite_reporte', 'urgente', 'congelamiento', 'asignada_a',
            'tomada_en', 'escalada', 'resolucion_propuesta', 'propuesta_por', 'dictamen', 'resuelta_por', 'resuelta_en',
            'aprobada_por', 'folio_uif', 'reportada_uif_en', 'detalles', 'dictamen_propuesto', 'legado_analista'])
          AND pg_temp.mismas_claves(v -> 'movimientos' -> 0, ARRAY[
            'entidad', 'transaccion_id', 'sentido', 'canal', 'monto', 'contraparte', 'creado_en', 'retenido'])
          AND pg_temp.mismas_claves(v -> 'historial' -> 0, ARRAY[
            'id', 'ocurrido_en', 'evento', 'objeto', 'objeto_id', 'actor', 'actor_tipo', 'estado_anterior', 'estado_nuevo',
            'motivo', 'datos'])
     FROM r WHERE k = 'detalle'));
SELECT pg_temp.exigir('JSON: RevisionClienteAml (agregados, órdenes, mutuos, créditos)',
  (SELECT pg_temp.mismas_claves(v, ARRAY['cliente', 'cuentas', 'agregados', 'movimientos', 'ordenes_spei_pendientes',
                                         'mutuos_vigentes', 'creditos', 'alertas', 'congelamientos', 'relevantes_n'])
          AND pg_temp.mismas_claves(v -> 'agregados', ARRAY['h24', 'd7', 'd30'])
          AND pg_temp.mismas_claves(v -> 'agregados' -> 'd30', ARRAY['entradas_n', 'entradas_monto', 'salidas_n', 'salidas_monto'])
          AND pg_temp.mismas_claves(v -> 'ordenes_spei_pendientes' -> 0, ARRAY[
            'id', 'clave_rastreo', 'monto', 'beneficiario_clabe', 'beneficiario_nombre', 'estado', 'intentos_envio', 'creado_en'])
          AND pg_temp.mismas_claves(v -> 'mutuos_vigentes' -> 0, ARRAY['id', 'folio', 'principal', 'fecha_vencimiento'])
          AND pg_temp.mismas_claves(v -> 'creditos' -> 0, ARRAY[
            'id', 'folio', 'principal', 'saldo_insoluto', 'estado', 'fecha_vencimiento'])
          AND jsonb_array_length(v -> 'congelamientos') = 1 AND v ->> 'relevantes_n' = '0'
     FROM r WHERE k = 'revision'));
SELECT pg_temp.exigir('JSON: respuestas de las acciones de los oficiales',
  pg_temp.mismas_claves((SELECT v FROM r WHERE k = 'tomar'), ARRAY['alerta', 'congelamiento', 'solicitud', 'idempotente'])
  AND pg_temp.mismas_claves((SELECT v FROM r WHERE k = 'congelar'), ARRAY['congelamiento', 'alerta', 'ya_congelado', 'idempotente'])
  AND pg_temp.mismas_claves((SELECT v FROM r WHERE k = 'decide'), ARRAY[
    'solicitud', 'congelamiento', 'alertas_resueltas', 'liberaciones', 'idempotente'])
  AND pg_temp.mismas_claves((SELECT v -> 'liberaciones' -> 0 FROM r WHERE k = 'decide'), ARRAY['retencion_id', 'transaccion_id', 'monto'])
  AND pg_temp.mismas_claves((SELECT v FROM r WHERE k = 'parcial'), ARRAY['solicitud', 'congelamiento', 'idempotente'])
  AND pg_temp.mismas_claves((SELECT v FROM r WHERE k = 'uif'), ARRAY['alerta', 'idempotente'])
  AND pg_temp.mismas_claves((SELECT v FROM r WHERE k = 'prop_r05'), ARRAY['propuesta', 'idempotente'])
  AND pg_temp.mismas_claves((SELECT v FROM r WHERE k = 'prop_r05_ok'), ARRAY['propuesta', 'regla', 'parametros', 'idempotente']));

-- -----------------------------------------------------------------------------
-- 11. Lista de Personas Bloqueadas (también en modo observación)
-- -----------------------------------------------------------------------------
SELECT pg_temp.cliente('Lpb');
SELECT pg_temp.paso('Lpb', 'DEPOSITO', 1000, NULL, 'PERMITIR', 0, 'BAJO');
UPDATE kyc.kyc_perfiles SET estado_kyc = 'RECHAZADO', motivo_rechazo = 'Coincidencia en la Lista de Personas Bloqueadas'
 WHERE cliente_id = pg_temp.c('Lpb');
SELECT pg_temp.debe_pasar('Lista: el dueño marca al cliente en la Lista de Personas Bloqueadas', ARRAY[
  format('UPDATE kyc.clientes SET en_lista_bloqueadas = TRUE WHERE id = %L', pg_temp.c('Lpb'))
]);
SELECT pg_temp.exigir('Lista: alerta R09 urgente (24 h) y congelamiento LISTA_BLOQUEADAS pese al modo observación',
  (SELECT aml.parametros_vigentes()).modo_observacion
  AND (SELECT origen = 'LISTA_BLOQUEADAS' AND tipo_reporte = 'INUSUAL' AND urgente AND nivel_riesgo = 'CRITICO' AND puntaje = 100
              AND fecha_limite_reporte <= clock_timestamp() + interval '24 hours' AND congelamiento_id IS NOT NULL
              AND clave_deduplicacion = 'R09:' || cliente_id
         FROM aml.alertas WHERE id = pg_temp.alerta('Lpb', 'R09_LISTA_BLOQUEADAS'))
  AND (SELECT origen = 'LISTA_BLOQUEADAS' AND estado = 'ACTIVO' AND congelado_por IS NULL
         FROM aml.congelamientos WHERE cliente_id = pg_temp.c('Lpb'))
  AND (SELECT estado = 'CONGELADO_PREVENTIVO' FROM ifpe.cuentas WHERE id = pg_temp.cta('Lpb'))
  AND (SELECT array_agg(tipo ORDER BY id) FROM aml.notificaciones WHERE cliente_id = pg_temp.c('Lpb'))
      = ARRAY['ALERTA_CRITICA', 'CONGELAMIENTO']);

-- -----------------------------------------------------------------------------
-- 12. Monitoreo por lotes (como sc_aml), plazos, notificaciones y sellado
-- -----------------------------------------------------------------------------
-- Cambia la fecha del último abono de un cliente (el dueño desactiva la
-- inmutabilidad del ledger dentro de esta transacción que se revierte; la
-- cadena de hash no importa aquí).
CREATE FUNCTION pg_temp.fechar(p_cliente TEXT, p_desplazamiento INTERVAL) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
    ALTER TABLE ifpe.transacciones DISABLE TRIGGER trg_ifpe_tx_inmutable;
    UPDATE ifpe.transacciones SET creado_en = now() + p_desplazamiento
     WHERE id = (SELECT id FROM ifpe.transacciones WHERE cuenta_destino_id = pg_temp.cta(p_cliente) AND tipo = 'DEPOSITO'
                  ORDER BY secuencia DESC LIMIT 1);
    ALTER TABLE ifpe.transacciones ENABLE TRIGGER trg_ifpe_tx_inmutable;
END
$$;
-- Fin del despliegue gradual: el modo observación se apaga (cuatro ojos).
SELECT pg_temp.como_aml(format(
    $q$SELECT aml.decidir_propuesta(%L, (aml.proponer_cambio(%L, 'PARAMETROS', NULL, NULL, '{"modo_observacion": false}',
           'Fin del periodo de observación: calibración revisada', 'cum:o1:prop-13') -> 'propuesta' ->> 'id')::uuid,
           TRUE, 'De acuerdo con terminar la observación', 'cum:o2:prop-14')$q$,
    pg_temp.c('Oficial2'), pg_temp.c('Oficial1')));
SELECT pg_temp.exigir('Monitoreo: el modo observación quedó apagado', NOT (SELECT aml.parametros_vigentes()).modo_observacion);
SELECT pg_temp.cliente('Lento');
SELECT pg_temp.paso('Lento', 'DEPOSITO', 120000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.fechar('Lento', interval '1 day');
SELECT pg_temp.paso('Lento', 'DEPOSITO', 120000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.fechar('Lento', interval '2 days');
SELECT pg_temp.paso('Lento', 'DEPOSITO', 120000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.fechar('Lento', interval '3 days');
SELECT pg_temp.paso('Lento', 'DEPOSITO', 120000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.fechar('Lento', interval '3 days 10 minutes');
SELECT pg_temp.paso('Lento', 'DEPOSITO', 120000, NULL, 'PERMITIR', 0, 'BAJO');
SELECT pg_temp.fechar('Lento', interval '3 days 20 minutes');
SELECT pg_temp.exigir('Lotes: R20 (5 abonos en banda en 3 días de la Ciudad de México) crea una alerta y congela',
  pg_temp.como_aml(format('SELECT to_jsonb(aml.barrer_cliente(%L, now() + interval %L))', pg_temp.c('Lento'), '4 days')) = '1');
SELECT pg_temp.exigir('Lotes: alerta R20 LOTE CRITICO 85 y congelamiento LOTE, sin retención (los abonos son pasado)',
  (SELECT origen = 'LOTE' AND nivel_riesgo = 'CRITICO' AND puntaje = 85 AND decision = 'REGISTRADA'
          AND detalles -> 'lote' -> 'evidencia' ->> 'variante' = 'A' AND jsonb_array_length(detalles -> 'transacciones') = 5
     FROM aml.alertas WHERE id = pg_temp.alerta('Lento', 'R20_ESTRUCTURACION_LENTA'))
  AND (SELECT origen = 'LOTE' FROM aml.congelamientos WHERE cliente_id = pg_temp.c('Lento'))
  AND NOT EXISTS (SELECT 1 FROM aml.retenciones WHERE cliente_id = pg_temp.c('Lento')));
SELECT pg_temp.exigir('Lotes: volver a barrer al cliente no crea nada',
  pg_temp.como_aml(format('SELECT to_jsonb(aml.barrer_cliente(%L, now() + interval %L))', pg_temp.c('Lento'), '4 days')) = '0');

SELECT pg_temp.exigir('Monitoreo: el turno se toma una vez (marcas en las cabezas al migrar)',
  pg_temp.como_aml('SELECT to_jsonb(t) FROM aml.monitoreo_tomar_turno(600) t')
  = '{"ultima_secuencia_ifpe": 0, "ultima_secuencia_sofom": 0}');
SELECT pg_temp.exigir('Monitoreo: con el turno tomado, otra instancia no lo obtiene',
  pg_temp.como_aml('SELECT to_jsonb(t) FROM aml.monitoreo_tomar_turno(600) t') IS NULL);
SELECT pg_temp.exigir('Monitoreo: rango hasta la cabeza y candidatos por cliente',
  pg_temp.como_aml('SELECT to_jsonb(t) FROM aml.monitoreo_rango(100000) t')
  = jsonb_build_object('hasta_ifpe', (SELECT ultima_secuencia FROM ifpe.ledger_cabeza),
                       'hasta_sofom', (SELECT ultima_secuencia FROM sofom.ledger_cabeza))
  AND pg_temp.como_aml(format('SELECT to_jsonb(array_agg(c)) FROM aml.monitoreo_candidatos(0, %s, 0, %s) c',
                              (SELECT ultima_secuencia FROM ifpe.ledger_cabeza), (SELECT ultima_secuencia FROM sofom.ledger_cabeza)))
      @> jsonb_build_array(pg_temp.c('Ana'), pg_temp.c('Lento'), pg_temp.c('Cob')));
SELECT pg_temp.como_aml(format('SELECT to_jsonb(aml.monitoreo_avanzar_marca(%s, %s)::text)',
                               (SELECT ultima_secuencia FROM ifpe.ledger_cabeza), (SELECT ultima_secuencia FROM sofom.ledger_cabeza)));
SELECT pg_temp.como_aml($q$SELECT to_jsonb(aml.monitoreo_liberar_turno('{"alertas_lote": 1}')::text)$q$);
SELECT pg_temp.exigir('Monitoreo: marcas avanzadas (nunca hacia atrás) y turno liberado',
  (SELECT ultima_secuencia_ifpe = (SELECT ultima_secuencia FROM ifpe.ledger_cabeza) AND en_curso_hasta = '-infinity'
          AND ultimo_ciclo_en IS NOT NULL AND ultimo_resultado = '{"alertas_lote": 1}'
     FROM aml.monitoreo_estado));

SELECT pg_temp.exigir('Reevaluación: las ocho marcas DIFERIDA esperan',
  pg_temp.como_aml('SELECT to_jsonb(count(*)) FROM aml.marcadores_pendientes(100)') = '8');
SELECT pg_temp.exigir('Reevaluación: cada marca recibe su evaluación REEVALUADA',
  pg_temp.como_aml('SELECT to_jsonb(bool_and(aml.reevaluar(m))) FROM aml.marcadores_pendientes(100) m') = 'true');
SELECT pg_temp.exigir('Reevaluación: ocho REEVALUADA ligadas a su marca y ninguna pendiente',
  (SELECT count(*) = 8 FROM aml.evaluaciones e JOIN aml.evaluaciones o ON o.id = e.evaluacion_origen_id
    WHERE e.origen = 'REEVALUACION' AND e.resultado = 'REEVALUADA' AND e.transaccion_id = o.transaccion_id)
  AND pg_temp.como_aml('SELECT to_jsonb(count(*)) FROM aml.marcadores_pendientes(100)') = '0');
SELECT pg_temp.debe_fallar('Reevaluación: una marca se reevalúa una sola vez', ARRAY[
  'SET LOCAL ROLE sc_aml',
  format('SELECT aml.reevaluar(%L)', (SELECT id FROM aml.evaluaciones WHERE origen = 'DIFERIDA' LIMIT 1))
], 'ux_aml_eval_origen');

SELECT pg_temp.exigir('Redes: R26 (un ordenante abona en banda a varios clientes) y R27 (varios envían a una CLABE)',
  (pg_temp.como_aml('SELECT to_jsonb(aml.barrer_redes(clock_timestamp()))'))::text::int > 0);
SELECT pg_temp.exigir('Redes: una alerta R26 por cliente de la red, sin congelamiento; volver a barrer no crea nada',
  (SELECT count(*) > 3 AND bool_and(origen = 'LOTE' AND congelamiento_id IS NULL AND detalles -> 'red' ->> 'clave' = '012180001234567899')
     FROM aml.alertas WHERE regla_codigo = 'R26_RED_ORDENANTE')
  AND EXISTS (SELECT 1 FROM aml.alertas WHERE regla_codigo = 'R26_RED_ORDENANTE' AND cliente_id = pg_temp.c('Borde1'))
  AND EXISTS (SELECT 1 FROM aml.alertas WHERE regla_codigo = 'R27_RED_BENEFICIARIO')
  AND pg_temp.como_aml('SELECT to_jsonb(aml.barrer_redes(clock_timestamp()))') = '0');

SELECT pg_temp.exigir('Plazos: la primera revisión crea avisos (SLA de congelamiento y escalamiento de CRITICO)',
  (pg_temp.como_aml($q$SELECT to_jsonb(aml.revisar_plazos(now() + interval '73 hours'))$q$))::text::int > 0);
SELECT pg_temp.exigir('Plazos: la segunda revisión no repite ninguno',
  pg_temp.como_aml($q$SELECT to_jsonb(aml.revisar_plazos(now() + interval '73 hours'))$q$) = '0'
  AND EXISTS (SELECT 1 FROM aml.notificaciones WHERE tipo = 'SLA_CONGELAMIENTO'
                 AND congelamiento_id = (SELECT id FROM aml.congelamientos WHERE cliente_id = pg_temp.c('Lento')))
  AND EXISTS (SELECT 1 FROM aml.notificaciones WHERE tipo = 'ESCALAMIENTO'
                 AND alerta_id = pg_temp.alerta('Lento', 'R20_ESTRUCTURACION_LENTA')));
SELECT pg_temp.exigir('Notificaciones: se entregan todas, una vez',
  (pg_temp.como_aml('SELECT to_jsonb(count(*)) FROM (SELECT aml.marcar_notificacion(n.id) FROM aml.tomar_notificaciones(100000) n) x'))
      ::text::int > 0);
SELECT pg_temp.exigir('Notificaciones: ninguna queda sin entregar',
  NOT EXISTS (SELECT 1 FROM aml.notificaciones WHERE entregada_en IS NULL)
  AND (aml.resumen() ->> 'notificaciones_sin_entregar')::int = 0);

INSERT INTO aml.bitacora (ocurrido_en, evento, objeto, actor_tipo, motivo)
VALUES (now() - interval '1 hour', 'MIGRACION_0009', 'SISTEMA', 'MIGRACION', 'renglón antiguo 1'),
       (now() - interval '1 hour', 'MIGRACION_0009', 'SISTEMA', 'MIGRACION', 'renglón antiguo 2');
SELECT pg_temp.exigir('Sellado: los renglones de más de 10 minutos se sellan en un lote encadenado',
  (pg_temp.como_aml('SELECT to_jsonb(aml.sellar_bitacora())'))::text::int = (SELECT count(*) FROM aml.bitacora));
SELECT pg_temp.exigir('Sellado: sin renglones nuevos no hay otro lote; la verificación es válida (también para sc_auditor)',
  pg_temp.como_aml('SELECT to_jsonb(aml.sellar_bitacora())') = '0'
  AND (SELECT valido AND sellos = 1 AND filas = (SELECT count(*) FROM aml.bitacora) AND primer_error IS NULL
         FROM aml.verificar_bitacora())
  AND (SELECT hash_anterior = nucleo.hash_genesis() AND desde_id = 1 FROM aml.sellos_bitacora));
SELECT pg_temp.debe_pasar('Sellado: sc_auditor verifica la bitácora', ARRAY[
  'SET LOCAL ROLE sc_auditor',
  $q$DO $d$ BEGIN IF NOT (SELECT valido FROM aml.verificar_bitacora()) THEN RAISE EXCEPTION 'inválida'; END IF; END $d$$q$,
  'RESET ROLE'
]);

-- -----------------------------------------------------------------------------
-- 14. Reserva del 100% al final (antes de alterar la bitácora)
-- -----------------------------------------------------------------------------
SELECT pg_temp.exigir('Reserva 100%: pasivo con clientes = reserva en custodia, tras todo lo anterior',
  (SELECT pasivo_clientes = activo_reserva FROM ifpe.estado_reserva)
  AND (SELECT activo_reserva FROM ifpe.estado_reserva) = (SELECT sum(saldo) FROM ifpe.reserva_custodia)
  AND (SELECT pasivo_clientes FROM ifpe.estado_reserva) = (SELECT sum(saldo_disponible + saldo_bloqueado) FROM ifpe.cuentas));
SELECT pg_temp.exigir('Espejo final: CONGELADO_PREVENTIVO <=> congelamiento ACTIVO para todos los clientes',
  NOT EXISTS (SELECT 1 FROM ifpe.cuentas c
               WHERE (c.estado = 'CONGELADO_PREVENTIVO') <> aml.cliente_congelado(c.cliente_id)
                 AND NOT (c.estado IN ('CONGELADA', 'CERRADA') AND aml.cliente_congelado(c.cliente_id))));

-- Alterar un renglón (con la protección desactivada por el dueño) rompe la verificación.
ALTER TABLE aml.bitacora DISABLE TRIGGER trg_aml_bitacora_inmutable;
UPDATE aml.bitacora SET motivo = 'alterado' WHERE id = (SELECT min(id) FROM aml.bitacora);
ALTER TABLE aml.bitacora ENABLE TRIGGER trg_aml_bitacora_inmutable;
SELECT pg_temp.exigir('Sellado: un renglón alterado se detecta',
  (SELECT NOT valido AND primer_error LIKE 'renglón %' FROM aml.verificar_bitacora()));

ROLLBACK;

-- -----------------------------------------------------------------------------
-- 13. La evaluación exige READ COMMITTED (fuera de la transacción principal)
-- -----------------------------------------------------------------------------
BEGIN ISOLATION LEVEL REPEATABLE READ;
DO $$
BEGIN
    PERFORM * FROM ifpe.aml_evaluar(gen_random_uuid(), 'ENTRADA', 'SPEI_WEBHOOK', 1, gen_random_uuid(), 'spei:x', '{}');
    RAISE EXCEPTION 'FALLO [Aislamiento]: se esperaba SC_AML_AISLAMIENTO pero la evaluación se aceptó';
EXCEPTION WHEN OTHERS THEN
    IF strpos(SQLERRM, 'SC_AML_AISLAMIENTO') = 0 THEN
        RAISE EXCEPTION 'FALLO [Aislamiento]: error inesperado "%"', SQLERRM;
    END IF;
    RAISE NOTICE 'OK  rechazado  Aislamiento: la evaluación en REPEATABLE READ';
END
$$;
ROLLBACK;
\o
\echo 'tests/db/0009_aml.sql: todas las pruebas pasaron (ROLLBACK)'
