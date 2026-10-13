-- =============================================================================
--  Synthetic Capital — 0004: saldos ligados al ledger e integridad de cortes
-- =============================================================================
--
--  Segunda revisión adversarial. Hasta 0003, la cadena SHA-256 protegía las
--  filas del ledger pero no los SALDOS: una transacción con el rol sc_ifpe podía
--  sumar X a una cuenta y X a la reserva (cuadrado para SC_RESERVA_100) sin
--  ninguna fila en el ledger, y pasaba la auditoría y el Proof of Reserve.
--
--  1. Saldos sombra: cada saldo (cuentas y reserva de la IFPE; cuentas, caja y
--     créditos de la SOFOM) tiene una copia que SOLO mueve el trigger que
--     inserta movimientos en el ledger. Al COMMIT, todo saldo modificado debe
--     coincidir con su sombra (SC_SALDO_SIN_LEDGER). Ningún rol de la
--     aplicación puede escribir las sombras.
--  2. Secuencias del ledger estrictamente positivas (el verificador recorre 1..N).
--  3. Cortes de Proof of Reserve validados por la base de datos: conjunto de
--     hojas = cuentas vivas, hash de cada hoja recalculado, total = pasivo,
--     ancla = cabeza del ledger y anclas que nunca retroceden.
--  4. Liquidez SOFOM basada en la exposición real: el principal que los
--     acreditados ya devolvieron pertenece a los inversionistas, no es capital.
--  5. Índice SPEI: clave canónica (mayúsculas) y solo para depósitos con clave
--     `spei:` (los registros anteriores no impiden migrar).
--  6. CLABE de desembolso de cada crédito, para idempotencia exacta.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. Saldos sombra
-- -----------------------------------------------------------------------------
CREATE TABLE ifpe.saldos_ledger (
    cuenta_id  UUID PRIMARY KEY REFERENCES ifpe.cuentas (id) ON DELETE RESTRICT,
    disponible NUMERIC(20, 8) NOT NULL DEFAULT 0,
    bloqueado  NUMERIC(20, 8) NOT NULL DEFAULT 0
);
CREATE TABLE ifpe.reserva_ledger (
    reserva_id UUID PRIMARY KEY REFERENCES ifpe.reserva_custodia (id) ON DELETE RESTRICT,
    saldo      NUMERIC(20, 8) NOT NULL DEFAULT 0
);
CREATE TABLE sofom.saldos_ledger (
    cuenta_id  UUID PRIMARY KEY REFERENCES sofom.cuentas (id) ON DELETE RESTRICT,
    disponible NUMERIC(20, 8) NOT NULL DEFAULT 0,
    bloqueado  NUMERIC(20, 8) NOT NULL DEFAULT 0
);
CREATE TABLE sofom.caja_ledger (
    caja_id UUID PRIMARY KEY REFERENCES sofom.caja (id) ON DELETE RESTRICT,
    saldo   NUMERIC(20, 8) NOT NULL DEFAULT 0
);
CREATE TABLE sofom.creditos_ledger (
    credito_id     UUID PRIMARY KEY REFERENCES sofom.creditos (id) ON DELETE RESTRICT,
    saldo_insoluto NUMERIC(20, 8) NOT NULL DEFAULT 0
);

-- Movimiento de una sombra (upsert). Uso interno: sin EXECUTE para PUBLIC.
CREATE FUNCTION nucleo.mover_sombra(p_tabla TEXT, p_id UUID, p_d1 NUMERIC, p_d2 NUMERIC DEFAULT 0) RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
    IF p_id IS NULL THEN
        RAISE EXCEPTION 'SC_SALDO_SIN_LEDGER: movimiento sin cuenta para %', p_tabla;
    END IF;
    CASE p_tabla
        WHEN 'ifpe.cuentas' THEN
            INSERT INTO ifpe.saldos_ledger AS s (cuenta_id, disponible, bloqueado) VALUES (p_id, p_d1, p_d2)
            ON CONFLICT (cuenta_id) DO UPDATE SET disponible = s.disponible + EXCLUDED.disponible,
                                                  bloqueado = s.bloqueado + EXCLUDED.bloqueado;
        WHEN 'ifpe.reserva' THEN
            INSERT INTO ifpe.reserva_ledger AS s (reserva_id, saldo) VALUES (p_id, p_d1)
            ON CONFLICT (reserva_id) DO UPDATE SET saldo = s.saldo + EXCLUDED.saldo;
        WHEN 'sofom.cuentas' THEN
            INSERT INTO sofom.saldos_ledger AS s (cuenta_id, disponible, bloqueado) VALUES (p_id, p_d1, p_d2)
            ON CONFLICT (cuenta_id) DO UPDATE SET disponible = s.disponible + EXCLUDED.disponible,
                                                  bloqueado = s.bloqueado + EXCLUDED.bloqueado;
        WHEN 'sofom.caja' THEN
            INSERT INTO sofom.caja_ledger AS s (caja_id, saldo) VALUES (p_id, p_d1)
            ON CONFLICT (caja_id) DO UPDATE SET saldo = s.saldo + EXCLUDED.saldo;
        WHEN 'sofom.creditos' THEN
            INSERT INTO sofom.creditos_ledger AS s (credito_id, saldo_insoluto) VALUES (p_id, p_d1)
            ON CONFLICT (credito_id) DO UPDATE SET saldo_insoluto = s.saldo_insoluto + EXCLUDED.saldo_insoluto;
    END CASE;
END
$$;
REVOKE EXECUTE ON FUNCTION nucleo.mover_sombra(TEXT, UUID, NUMERIC, NUMERIC) FROM PUBLIC;

-- Efecto de cada tipo de movimiento IFPE sobre los saldos.
CREATE FUNCTION ifpe.fn_aplicar_tx_a_sombras() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    CASE NEW.tipo
        WHEN 'DEPOSITO', 'RETORNO_DE_SOFOM' THEN
            PERFORM nucleo.mover_sombra('ifpe.cuentas', NEW.cuenta_destino_id, NEW.monto, 0);
            PERFORM nucleo.mover_sombra('ifpe.reserva', NEW.reserva_id, NEW.monto);
        WHEN 'RETIRO', 'TRASPASO_A_SOFOM' THEN
            PERFORM nucleo.mover_sombra('ifpe.cuentas', NEW.cuenta_origen_id, -NEW.monto, 0);
            PERFORM nucleo.mover_sombra('ifpe.reserva', NEW.reserva_id, -NEW.monto);
        WHEN 'TRANSFERENCIA' THEN
            PERFORM nucleo.mover_sombra('ifpe.cuentas', NEW.cuenta_origen_id, -NEW.monto, 0);
            PERFORM nucleo.mover_sombra('ifpe.cuentas', NEW.cuenta_destino_id, NEW.monto, 0);
        WHEN 'RETENCION' THEN
            PERFORM nucleo.mover_sombra('ifpe.cuentas', NEW.cuenta_origen_id, -NEW.monto, NEW.monto);
        WHEN 'LIBERACION_RETENCION' THEN
            PERFORM nucleo.mover_sombra('ifpe.cuentas', NEW.cuenta_origen_id, NEW.monto, -NEW.monto);
    END CASE;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_ifpe_tx_sombras AFTER INSERT ON ifpe.transacciones
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_aplicar_tx_a_sombras();

-- Efecto de cada tipo de movimiento SOFOM. El principal de un cobro de crédito
-- lo aplica sofom.cobros (el movimiento lleva el total principal + interés).
CREATE FUNCTION sofom.fn_aplicar_tx_a_sombras() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    CASE NEW.tipo
        WHEN 'APORTACION_MUTUO' THEN
            PERFORM nucleo.mover_sombra('sofom.cuentas', NEW.cuenta_id, 0, NEW.monto);
            PERFORM nucleo.mover_sombra('sofom.caja', NEW.caja_id, NEW.monto);
        WHEN 'VENCIMIENTO_MUTUO' THEN
            PERFORM nucleo.mover_sombra('sofom.cuentas', NEW.cuenta_id, NEW.monto, -NEW.monto);
        WHEN 'PAGO_INTERES_MUTUO' THEN
            PERFORM nucleo.mover_sombra('sofom.cuentas', NEW.cuenta_id, NEW.monto, 0);
        WHEN 'RETORNO_A_IFPE' THEN
            PERFORM nucleo.mover_sombra('sofom.cuentas', NEW.cuenta_id, -NEW.monto, 0);
            PERFORM nucleo.mover_sombra('sofom.caja', NEW.caja_id, -NEW.monto);
        WHEN 'DESEMBOLSO_CREDITO' THEN
            PERFORM nucleo.mover_sombra('sofom.caja', NEW.caja_id, -NEW.monto);
            PERFORM nucleo.mover_sombra('sofom.creditos', NEW.credito_id, NEW.monto);
        WHEN 'COBRO_CREDITO', 'APORTACION_CAPITAL' THEN
            PERFORM nucleo.mover_sombra('sofom.caja', NEW.caja_id, NEW.monto);
    END CASE;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_sofom_tx_sombras AFTER INSERT ON sofom.transacciones
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_aplicar_tx_a_sombras();

-- Desglose de un cobro: debe corresponder exactamente a su movimiento.
CREATE FUNCTION sofom.fn_aplicar_cobro_a_sombras() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    tx sofom.transacciones%ROWTYPE;
BEGIN
    SELECT * INTO tx FROM sofom.transacciones WHERE id = NEW.transaccion_id;
    IF tx.tipo IS DISTINCT FROM 'COBRO_CREDITO' OR tx.credito_id IS DISTINCT FROM NEW.credito_id
       OR tx.monto <> NEW.principal + NEW.interes THEN
        RAISE EXCEPTION 'SC_COBRO_INVALIDO: el desglose no corresponde al movimiento %', NEW.transaccion_id
            USING ERRCODE = 'check_violation';
    END IF;
    PERFORM nucleo.mover_sombra('sofom.creditos', NEW.credito_id, -NEW.principal);
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_cobros_sombras AFTER INSERT ON sofom.cobros
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_aplicar_cobro_a_sombras();

-- Verificación diferida: saldo vivo == sombra, para cada fila modificada.
CREATE FUNCTION nucleo.fn_verificar_saldo_vs_ledger() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    clave   UUID;
    vivo1   NUMERIC;
    vivo2   NUMERIC := 0;
    sombra1 NUMERIC;
    sombra2 NUMERIC := 0;
BEGIN
    IF TG_TABLE_SCHEMA = 'ifpe' AND TG_TABLE_NAME IN ('cuentas', 'saldos_ledger') THEN
        IF TG_TABLE_NAME = 'cuentas' THEN clave := NEW.id; ELSE clave := NEW.cuenta_id; END IF;
        SELECT saldo_disponible, saldo_bloqueado INTO vivo1, vivo2 FROM ifpe.cuentas WHERE id = clave;
        SELECT disponible, bloqueado INTO sombra1, sombra2 FROM ifpe.saldos_ledger WHERE cuenta_id = clave;
    ELSIF TG_TABLE_SCHEMA = 'ifpe' THEN -- reserva_custodia / reserva_ledger
        IF TG_TABLE_NAME = 'reserva_custodia' THEN clave := NEW.id; ELSE clave := NEW.reserva_id; END IF;
        SELECT saldo INTO vivo1 FROM ifpe.reserva_custodia WHERE id = clave;
        SELECT saldo INTO sombra1 FROM ifpe.reserva_ledger WHERE reserva_id = clave;
    ELSIF TG_TABLE_NAME IN ('cuentas', 'saldos_ledger') THEN
        IF TG_TABLE_NAME = 'cuentas' THEN clave := NEW.id; ELSE clave := NEW.cuenta_id; END IF;
        SELECT saldo_disponible, saldo_bloqueado INTO vivo1, vivo2 FROM sofom.cuentas WHERE id = clave;
        SELECT disponible, bloqueado INTO sombra1, sombra2 FROM sofom.saldos_ledger WHERE cuenta_id = clave;
    ELSIF TG_TABLE_NAME IN ('caja', 'caja_ledger') THEN
        IF TG_TABLE_NAME = 'caja' THEN clave := NEW.id; ELSE clave := NEW.caja_id; END IF;
        SELECT saldo INTO vivo1 FROM sofom.caja WHERE id = clave;
        SELECT saldo INTO sombra1 FROM sofom.caja_ledger WHERE caja_id = clave;
    ELSE -- creditos / creditos_ledger
        IF TG_TABLE_NAME = 'creditos' THEN clave := NEW.id; ELSE clave := NEW.credito_id; END IF;
        SELECT saldo_insoluto INTO vivo1 FROM sofom.creditos WHERE id = clave;
        SELECT saldo_insoluto INTO sombra1 FROM sofom.creditos_ledger WHERE credito_id = clave;
    END IF;
    IF coalesce(vivo1, 0) <> coalesce(sombra1, 0) OR coalesce(vivo2, 0) <> coalesce(sombra2, 0) THEN
        RAISE EXCEPTION 'SC_SALDO_SIN_LEDGER: %.% % tiene saldo (%, %) pero el ledger justifica (%, %)',
            TG_TABLE_SCHEMA, TG_TABLE_NAME, clave, coalesce(vivo1, 0), coalesce(vivo2, 0),
            coalesce(sombra1, 0), coalesce(sombra2, 0)
            USING ERRCODE = 'check_violation',
                  HINT = 'Todo cambio de saldo debe registrarse en el ledger, en la misma transacción.';
    END IF;
    RETURN NULL;
END
$$;

-- Nombres *_saldo_ledger: se ordenan después de los triggers diferidos previos
-- de la misma tabla (p. ej. trg_caja_liquidez, trg_credito_fondeo_total).
CREATE CONSTRAINT TRIGGER trg_ifpe_cuentas_saldo_ledger
    AFTER INSERT OR UPDATE OF saldo_disponible, saldo_bloqueado ON ifpe.cuentas
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION nucleo.fn_verificar_saldo_vs_ledger();
CREATE CONSTRAINT TRIGGER trg_ifpe_sombra_cuentas_saldo_ledger
    AFTER INSERT OR UPDATE ON ifpe.saldos_ledger
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION nucleo.fn_verificar_saldo_vs_ledger();
CREATE CONSTRAINT TRIGGER trg_ifpe_reserva_saldo_ledger
    AFTER INSERT OR UPDATE OF saldo ON ifpe.reserva_custodia
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION nucleo.fn_verificar_saldo_vs_ledger();
CREATE CONSTRAINT TRIGGER trg_ifpe_sombra_reserva_saldo_ledger
    AFTER INSERT OR UPDATE ON ifpe.reserva_ledger
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION nucleo.fn_verificar_saldo_vs_ledger();
CREATE CONSTRAINT TRIGGER trg_sofom_cuentas_saldo_ledger
    AFTER INSERT OR UPDATE OF saldo_disponible, saldo_bloqueado ON sofom.cuentas
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION nucleo.fn_verificar_saldo_vs_ledger();
CREATE CONSTRAINT TRIGGER trg_sofom_sombra_cuentas_saldo_ledger
    AFTER INSERT OR UPDATE ON sofom.saldos_ledger
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION nucleo.fn_verificar_saldo_vs_ledger();
CREATE CONSTRAINT TRIGGER trg_caja_saldo_ledger
    AFTER INSERT OR UPDATE OF saldo ON sofom.caja
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION nucleo.fn_verificar_saldo_vs_ledger();
CREATE CONSTRAINT TRIGGER trg_sofom_sombra_caja_saldo_ledger
    AFTER INSERT OR UPDATE ON sofom.caja_ledger
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION nucleo.fn_verificar_saldo_vs_ledger();
CREATE CONSTRAINT TRIGGER trg_credito_saldo_ledger
    AFTER INSERT OR UPDATE OF saldo_insoluto ON sofom.creditos
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION nucleo.fn_verificar_saldo_vs_ledger();
CREATE CONSTRAINT TRIGGER trg_sofom_sombra_creditos_saldo_ledger
    AFTER INSERT OR UPDATE ON sofom.creditos_ledger
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION nucleo.fn_verificar_saldo_vs_ledger();

-- Población inicial: reproducción completa de los ledgers existentes.
INSERT INTO ifpe.saldos_ledger (cuenta_id, disponible, bloqueado)
SELECT cuenta, sum(d1), sum(d2) FROM (
    SELECT cuenta_destino_id, monto, 0::numeric FROM ifpe.transacciones
     WHERE tipo IN ('DEPOSITO', 'RETORNO_DE_SOFOM', 'TRANSFERENCIA')
    UNION ALL
    SELECT cuenta_origen_id, -monto, 0 FROM ifpe.transacciones
     WHERE tipo IN ('RETIRO', 'TRASPASO_A_SOFOM', 'TRANSFERENCIA')
    UNION ALL
    SELECT cuenta_origen_id, -monto, monto FROM ifpe.transacciones WHERE tipo = 'RETENCION'
    UNION ALL
    SELECT cuenta_origen_id, monto, -monto FROM ifpe.transacciones WHERE tipo = 'LIBERACION_RETENCION'
) m (cuenta, d1, d2) GROUP BY cuenta;

INSERT INTO ifpe.reserva_ledger (reserva_id, saldo)
SELECT reserva_id, sum(CASE WHEN tipo IN ('DEPOSITO', 'RETORNO_DE_SOFOM') THEN monto ELSE -monto END)
  FROM ifpe.transacciones WHERE reserva_id IS NOT NULL GROUP BY reserva_id;

INSERT INTO sofom.saldos_ledger (cuenta_id, disponible, bloqueado)
SELECT cuenta_id,
       sum(CASE tipo WHEN 'VENCIMIENTO_MUTUO' THEN monto WHEN 'PAGO_INTERES_MUTUO' THEN monto
                     WHEN 'RETORNO_A_IFPE' THEN -monto ELSE 0 END),
       sum(CASE tipo WHEN 'APORTACION_MUTUO' THEN monto WHEN 'VENCIMIENTO_MUTUO' THEN -monto ELSE 0 END)
  FROM sofom.transacciones WHERE cuenta_id IS NOT NULL GROUP BY cuenta_id;

INSERT INTO sofom.caja_ledger (caja_id, saldo)
SELECT caja_id,
       sum(CASE WHEN tipo IN ('APORTACION_MUTUO', 'COBRO_CREDITO', 'APORTACION_CAPITAL') THEN monto
                WHEN tipo IN ('RETORNO_A_IFPE', 'DESEMBOLSO_CREDITO') THEN -monto ELSE 0 END)
  FROM sofom.transacciones WHERE caja_id IS NOT NULL GROUP BY caja_id;

INSERT INTO sofom.creditos_ledger (credito_id, saldo_insoluto)
SELECT credito_id, sum(monto) FROM (
    SELECT credito_id, monto FROM sofom.transacciones WHERE tipo = 'DESEMBOLSO_CREDITO'
    UNION ALL
    SELECT credito_id, -principal FROM sofom.cobros
) m GROUP BY credito_id;

GRANT SELECT ON ifpe.saldos_ledger, ifpe.reserva_ledger TO sc_auditor;
GRANT SELECT ON sofom.saldos_ledger, sofom.caja_ledger, sofom.creditos_ledger TO sc_sofom;

-- -----------------------------------------------------------------------------
-- 2. Secuencias positivas
-- -----------------------------------------------------------------------------
ALTER TABLE ifpe.transacciones ADD CONSTRAINT ck_ifpe_tx_secuencia_positiva CHECK (secuencia > 0);
ALTER TABLE sofom.transacciones ADD CONSTRAINT ck_sofom_tx_secuencia_positiva CHECK (secuencia > 0);

-- -----------------------------------------------------------------------------
-- 3. Validación completa de cortes de Proof of Reserve
-- -----------------------------------------------------------------------------
-- El generador usa REPEATABLE READ: este trigger diferido ve la misma foto
-- que se usó para construir las hojas.
CREATE OR REPLACE FUNCTION ifpe.fn_verificar_hojas_corte() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    corte        ifpe.cortes_reserva%ROWTYPE;
    suma         NUMERIC(20, 8);
    n            INTEGER;
    min_indice   INTEGER;
    max_indice   INTEGER;
    pasivo       NUMERIC(20, 8);
    cabeza       ifpe.ledger_cabeza%ROWTYPE;
    ancla_previa BIGINT;
    diferencias  BIGINT;
    hashes_malos BIGINT;
BEGIN
    SELECT * INTO corte FROM ifpe.cortes_reserva WHERE id = NEW.id;
    SELECT coalesce(sum(saldo_total), 0), count(*), min(indice), max(indice)
      INTO suma, n, min_indice, max_indice
      FROM ifpe.cortes_reserva_hojas WHERE corte_id = NEW.id;
    IF suma <> corte.total_pasivo OR n <> corte.num_hojas THEN
        RAISE EXCEPTION 'SC_CORTE_INCOMPLETO: el corte % declara % hojas / pasivo %, pero tiene % hojas / suma %',
            NEW.id, corte.num_hojas, corte.total_pasivo, n, suma
            USING ERRCODE = 'check_violation';
    END IF;
    IF n > 0 AND (min_indice <> 0 OR max_indice <> n - 1) THEN
        RAISE EXCEPTION 'SC_CORTE_INVALIDO: los índices de hoja del corte % no son 0..%', NEW.id, n - 1
            USING ERRCODE = 'check_violation';
    END IF;

    SELECT pasivo_clientes INTO pasivo FROM ifpe.estado_reserva WHERE id;
    IF corte.total_pasivo <> pasivo THEN
        RAISE EXCEPTION 'SC_CORTE_INVALIDO: el corte declara pasivo % pero el pasivo con clientes es %',
            corte.total_pasivo, pasivo
            USING ERRCODE = 'check_violation';
    END IF;

    SELECT * INTO cabeza FROM ifpe.ledger_cabeza WHERE id;
    IF (corte.secuencia_ledger, corte.hash_ledger) IS DISTINCT FROM (cabeza.ultima_secuencia, cabeza.ultimo_hash) THEN
        RAISE EXCEPTION 'SC_CORTE_INVALIDO: el ancla del corte (%, %) no es la cabeza del ledger (%, %)',
            corte.secuencia_ledger, corte.hash_ledger, cabeza.ultima_secuencia, cabeza.ultimo_hash
            USING ERRCODE = 'check_violation';
    END IF;
    SELECT max(secuencia_ledger) INTO ancla_previa FROM ifpe.cortes_reserva WHERE id <> NEW.id;
    IF ancla_previa IS NOT NULL AND corte.secuencia_ledger < ancla_previa THEN
        RAISE EXCEPTION 'SC_CORTE_INVALIDO: el ancla retrocede (% < % publicada antes): el ledger fue truncado',
            corte.secuencia_ledger, ancla_previa
            USING ERRCODE = 'check_violation';
    END IF;

    SELECT count(*) INTO diferencias FROM (
        (SELECT cuenta_id, saldo_total FROM ifpe.v_hojas_reserva
         EXCEPT SELECT cuenta_id, saldo_total FROM ifpe.cortes_reserva_hojas WHERE corte_id = NEW.id)
        UNION ALL
        (SELECT cuenta_id, saldo_total FROM ifpe.cortes_reserva_hojas WHERE corte_id = NEW.id
         EXCEPT SELECT cuenta_id, saldo_total FROM ifpe.v_hojas_reserva)
    ) d;
    IF diferencias > 0 THEN
        RAISE EXCEPTION 'SC_CORTE_INVALIDO: las hojas del corte no coinciden con las cuentas vivas (% diferencias)',
            diferencias
            USING ERRCODE = 'check_violation';
    END IF;

    -- hoja = SHA256("SC-POR-v1:hoja" 0x00 || cuenta_id[16] || saldo_u128_be[16] || sal[32])
    SELECT count(*) INTO hashes_malos
      FROM ifpe.cortes_reserva_hojas h
     WHERE h.corte_id = NEW.id
       AND h.hoja_hash <> encode(sha256(
               convert_to('SC-POR-v1:hoja', 'UTF8') || '\x00'::bytea || uuid_send(h.cuenta_id)
               || '\x0000000000000000'::bytea || int8send((h.saldo_total * 100000000)::bigint)
               || decode(h.sal, 'hex')), 'hex');
    IF hashes_malos > 0 THEN
        RAISE EXCEPTION 'SC_CORTE_INVALIDO: % hojas del corte tienen un hash que no corresponde a su contenido',
            hashes_malos
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END
$$;

-- -----------------------------------------------------------------------------
-- 4. Liquidez SOFOM basada en exposición real
-- -----------------------------------------------------------------------------
-- caja >= Σ principal de mutuos vigentes - Σ saldo insoluto de créditos vivos.
-- El principal devuelto por un acreditado vuelve a respaldar al inversionista
-- que lo fondeó (no es capital propio); la cartera castigada ya no cuenta como
-- respaldo (la pérdida la cubre el capital).
-- Única fuente de verdad del efectivo requerido. Devuelve solo el agregado:
-- el puente puede consultarlo sin tener acceso a los créditos.
CREATE FUNCTION sofom.requerido_liquidez(excluir_contrato UUID DEFAULT NULL) RETURNS NUMERIC
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT (SELECT coalesce(sum(principal), 0) FROM sofom.contratos_mutuo
             WHERE estado = 'VIGENTE' AND id IS DISTINCT FROM excluir_contrato)
         - (SELECT coalesce(sum(saldo_insoluto), 0) FROM sofom.creditos WHERE estado IN ('VIGENTE', 'VENCIDO'))
$$;
REVOKE EXECUTE ON FUNCTION sofom.requerido_liquidez(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION sofom.requerido_liquidez(UUID) TO sc_sofom, sc_puente;

CREATE OR REPLACE FUNCTION sofom.fn_verificar_liquidez() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    efectivo  NUMERIC(20, 8);
    requerido NUMERIC(20, 8);
BEGIN
    SELECT coalesce(sum(saldo), 0) INTO efectivo FROM sofom.caja;
    requerido := sofom.requerido_liquidez(NULL);
    IF efectivo < requerido THEN
        RAISE EXCEPTION 'SC_LIQUIDEZ_MUTUOS: la caja SOFOM (%) no cubre el principal de los mutuos vigentes que no está prestado (%). Intereses y pérdidas deben pagarse con capital propio.',
            efectivo, requerido
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_credito_liquidez
    AFTER INSERT OR UPDATE ON sofom.creditos
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_verificar_liquidez();

-- -----------------------------------------------------------------------------
-- 5. Índice SPEI canónico
-- -----------------------------------------------------------------------------
-- Un SPEI se identifica por banco emisor, fecha de operación y clave de
-- rastreo: '<emisor>:<AAAAMMDD>:<RASTREO>', ya en mayúsculas (el backend la
-- canoniza). El índice cubre solo identidades canónicas: los depósitos
-- anteriores a esta migración guardaban la clave de rastreo tal cual y pueden
-- incluir variantes de mayúsculas del mismo pago (el doble abono que esta
-- migración corrige); quedan en el ledger como historia y no impiden migrar.
DROP INDEX ifpe.ux_ifpe_tx_deposito_referencia;
CREATE UNIQUE INDEX ux_ifpe_tx_deposito_spei
    ON ifpe.transacciones (upper(referencia_externa))
    WHERE tipo = 'DEPOSITO' AND clave_idempotencia LIKE 'spei:%'
      AND referencia_externa ~ '^[0-9]{3,5}:[0-9]{8}:[A-Za-z0-9]{1,30}$';

-- -----------------------------------------------------------------------------
-- 6. CLABE de desembolso
-- -----------------------------------------------------------------------------
ALTER TABLE sofom.creditos
    ADD COLUMN clabe_desembolso CHAR(18)
        CONSTRAINT ck_creditos_clabe_desembolso CHECK (clabe_desembolso IS NULL OR nucleo.clabe_valida(clabe_desembolso));
