-- =============================================================================
--  Synthetic Capital — 0003: endurecimiento tras la revisión adversarial
-- =============================================================================
--
--  1. SPEI entrante: un mismo pago (clave de rastreo) no puede acreditarse dos
--     veces. Sin esto, un doble registro inflaría pasivo y reserva por igual
--     (SC_RESERVA_100 no lo detecta) sin que el efectivo exista.
--  2. Revocación de sesiones: `tokens_validos_desde` invalida JWT emitidos antes.
--  3. Liquidez SOFOM: el efectivo de caja siempre cubre el principal no
--     prestado de todos los mutuos vigentes. Los intereses y las pérdidas se
--     pagan con capital propio, nunca con el dinero de otro inversionista
--     (eso sería prestar corto para fondear largo).
--  4. Castigo de cartera: un crédito castigado libera su fondeo; la pérdida la
--     absorbe el capital de la SOFOM (la regla 3 lo exige al COMMIT). Antes, un
--     incumplimiento congelaba para siempre el mutuo que lo fondeó.
--  5. Contratos de mutuo: no pueden vencer mientras fondeen cartera viva;
--     `interes_pagado` solo cambia al vencer; `sofom.cuentas.saldo_bloqueado`
--     cuadra siempre con el principal vigente de la cuenta. El rol de crédito
--     pierde el acceso de escritura a mutuos y cuentas de inversionistas.
--  6. Fecha valor: un mutuo inicia y un crédito se desembolsa hoy (no se
--     admiten fechas retroactivas que esquivarían el "sin retiro anticipado").
--  7. Cobros de crédito: desglose principal/interés para idempotencia exacta.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. SPEI entrante idempotente por clave de rastreo
-- -----------------------------------------------------------------------------
CREATE UNIQUE INDEX ux_ifpe_tx_deposito_referencia
    ON ifpe.transacciones (referencia_externa)
    WHERE tipo = 'DEPOSITO' AND referencia_externa IS NOT NULL;

-- -----------------------------------------------------------------------------
-- 2. Revocación de sesiones
-- -----------------------------------------------------------------------------
ALTER TABLE kyc.credenciales ADD COLUMN tokens_validos_desde TIMESTAMPTZ;
COMMENT ON COLUMN kyc.credenciales.tokens_validos_desde IS
    'JWT con iat anterior a este instante se rechazan (NULL = ninguno revocado).';
GRANT UPDATE (tokens_validos_desde) ON kyc.credenciales TO sc_kyc;

-- -----------------------------------------------------------------------------
-- 3. Liquidez de la SOFOM: caja >= principal no prestado de mutuos vigentes
-- -----------------------------------------------------------------------------
CREATE FUNCTION sofom.fn_verificar_liquidez() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    efectivo   NUMERIC(20, 8);
    no_prestado NUMERIC(20, 8);
BEGIN
    SELECT coalesce(sum(saldo), 0) INTO efectivo FROM sofom.caja;
    SELECT coalesce(sum(principal - principal_asignado), 0) INTO no_prestado
      FROM sofom.contratos_mutuo WHERE estado = 'VIGENTE';
    IF efectivo < no_prestado THEN
        RAISE EXCEPTION 'SC_LIQUIDEZ_MUTUOS: la caja SOFOM (%) no cubre el principal no prestado de los mutuos vigentes (%). Intereses y pérdidas deben pagarse con capital propio.',
            efectivo, no_prestado
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_caja_liquidez
    AFTER INSERT OR UPDATE ON sofom.caja
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_verificar_liquidez();
CREATE CONSTRAINT TRIGGER trg_mutuo_liquidez
    AFTER INSERT OR UPDATE ON sofom.contratos_mutuo
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_verificar_liquidez();

-- -----------------------------------------------------------------------------
-- 4 y 5. Reglas de mutuos y créditos (reemplazan las funciones de 0001)
-- -----------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION sofom.fn_proteger_mutuo() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF (NEW.cliente_id, NEW.cuenta_sofom_id, NEW.cuenta_ifpe_origen_id, NEW.tx_aportacion_ifpe_id,
        NEW.principal, NEW.tasa_anual, NEW.plazo_dias, NEW.fecha_inicio, NEW.hash_terminos)
       IS DISTINCT FROM
       (OLD.cliente_id, OLD.cuenta_sofom_id, OLD.cuenta_ifpe_origen_id, OLD.tx_aportacion_ifpe_id,
        OLD.principal, OLD.tasa_anual, OLD.plazo_dias, OLD.fecha_inicio, OLD.hash_terminos) THEN
        RAISE EXCEPTION 'SC_MUTUO_INMUTABLE: los términos del contrato de mutuo % no pueden modificarse', OLD.folio
            USING ERRCODE = 'restrict_violation';
    END IF;

    IF NEW.interes_pagado IS DISTINCT FROM OLD.interes_pagado
       AND NOT (OLD.estado = 'VIGENTE' AND NEW.estado = 'VENCIDO') THEN
        RAISE EXCEPTION 'SC_MUTUO_INTERES: el interés de un mutuo solo se fija al vencimiento'
            USING ERRCODE = 'check_violation';
    END IF;

    IF NEW.estado IS DISTINCT FROM OLD.estado THEN
        IF NOT ((OLD.estado = 'VIGENTE' AND NEW.estado = 'VENCIDO')
             OR (OLD.estado = 'VENCIDO' AND NEW.estado = 'LIQUIDADO')) THEN
            RAISE EXCEPTION 'SC_MUTUO_TRANSICION: % -> % no permitida', OLD.estado, NEW.estado
                USING ERRCODE = 'check_violation';
        END IF;
        IF NEW.estado = 'VENCIDO' AND current_date < OLD.fecha_vencimiento THEN
            RAISE EXCEPTION 'SC_MUTUO_RETIRO_ANTICIPADO: el contrato % vence el %; no se admite retiro anticipado',
                OLD.folio, OLD.fecha_vencimiento
                USING ERRCODE = 'check_violation';
        END IF;
        IF NEW.estado = 'VENCIDO' AND NEW.principal_asignado > 0 THEN
            RAISE EXCEPTION 'SC_MUTUO_CON_CARTERA_VIGENTE: el contrato % aún fondea % en créditos no liquidados ni castigados',
                OLD.folio, NEW.principal_asignado
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;
    RETURN NEW;
END
$$;

CREATE OR REPLACE FUNCTION sofom.fn_proteger_credito() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    min_venc_mutuo DATE;
    asignado       NUMERIC(20, 8);
BEGIN
    IF (NEW.acreditado_id, NEW.principal, NEW.tasa_anual, NEW.plazo_dias, NEW.fecha_desembolso)
       IS DISTINCT FROM
       (OLD.acreditado_id, OLD.principal, OLD.tasa_anual, OLD.plazo_dias, OLD.fecha_desembolso) THEN
        IF OLD.estado <> 'EN_FONDEO' THEN
            RAISE EXCEPTION 'SC_CREDITO_INMUTABLE: el crédito % ya fue desembolsado; sus términos son fijos', OLD.folio
                USING ERRCODE = 'restrict_violation';
        END IF;
        SELECT min(m.fecha_vencimiento), coalesce(sum(a.monto), 0)
          INTO min_venc_mutuo, asignado
          FROM sofom.asignaciones_fondeo a
          JOIN sofom.contratos_mutuo m ON m.id = a.contrato_id
         WHERE a.credito_id = OLD.id;
        IF min_venc_mutuo IS NOT NULL AND min_venc_mutuo < NEW.fecha_desembolso + NEW.plazo_dias THEN
            RAISE EXCEPTION 'SC_CALCE_PLAZOS: el nuevo vencimiento del crédito % (%) excede el de su fondeo (%)',
                OLD.folio, NEW.fecha_desembolso + NEW.plazo_dias, min_venc_mutuo
                USING ERRCODE = 'check_violation';
        END IF;
        IF asignado > NEW.principal THEN
            RAISE EXCEPTION 'SC_SOBREFONDEO: el crédito % ya tiene % asignados > nuevo principal %',
                OLD.folio, asignado, NEW.principal
                USING ERRCODE = 'check_violation';
        END IF;
    END IF;

    IF NEW.estado IS DISTINCT FROM OLD.estado THEN
        IF NOT ((OLD.estado = 'EN_FONDEO' AND NEW.estado = 'VIGENTE')
             OR (OLD.estado = 'VIGENTE'   AND NEW.estado IN ('VENCIDO', 'LIQUIDADO'))
             OR (OLD.estado = 'VENCIDO'   AND NEW.estado IN ('LIQUIDADO', 'CASTIGADO'))) THEN
            RAISE EXCEPTION 'SC_CREDITO_TRANSICION: % -> % no permitida', OLD.estado, NEW.estado
                USING ERRCODE = 'check_violation';
        END IF;
        -- LIQUIDADO: el principal regresó a caja. CASTIGADO: la pérdida la
        -- absorbe el capital propio (trg_*_liquidez lo exige al COMMIT). En
        -- ambos casos el mutuo deja de fondear este crédito y puede vencer.
        IF NEW.estado IN ('LIQUIDADO', 'CASTIGADO') THEN
            UPDATE sofom.contratos_mutuo m
               SET principal_asignado = m.principal_asignado - a.monto
              FROM sofom.asignaciones_fondeo a
             WHERE a.credito_id = OLD.id AND a.contrato_id = m.id AND NOT a.liberada;
            UPDATE sofom.asignaciones_fondeo SET liberada = TRUE
             WHERE credito_id = OLD.id AND NOT liberada;
        END IF;
    END IF;
    RETURN NEW;
END
$$;

-- El saldo bloqueado de cada cuenta de inversión es exactamente el principal
-- de sus mutuos vigentes (liga el estado del contrato con el movimiento de dinero).
CREATE FUNCTION sofom.fn_verificar_bloqueado_cuenta() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    cuenta    UUID;
    bloqueado NUMERIC(20, 8);
    vigente   NUMERIC(20, 8);
BEGIN
    -- IF (no CASE): PL/pgSQL resuelve los campos de NEW de una expresión
    -- completa aunque la rama no aplique, y cada tabla tiene columnas distintas.
    IF TG_TABLE_NAME = 'cuentas' THEN
        cuenta := NEW.id;
    ELSE
        cuenta := NEW.cuenta_sofom_id;
    END IF;
    SELECT saldo_bloqueado INTO bloqueado FROM sofom.cuentas WHERE id = cuenta;
    SELECT coalesce(sum(principal), 0) INTO vigente
      FROM sofom.contratos_mutuo WHERE cuenta_sofom_id = cuenta AND estado = 'VIGENTE';
    IF bloqueado <> vigente THEN
        RAISE EXCEPTION 'SC_MUTUO_DESCUADRE: la cuenta de inversión % tiene % bloqueado pero % en mutuos vigentes',
            cuenta, bloqueado, vigente
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_cuenta_sofom_bloqueado
    AFTER INSERT OR UPDATE ON sofom.cuentas
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_verificar_bloqueado_cuenta();
CREATE CONSTRAINT TRIGGER trg_mutuo_bloqueado
    AFTER INSERT OR UPDATE ON sofom.contratos_mutuo
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_verificar_bloqueado_cuenta();

-- El rol de crédito no escribe mutuos ni cuentas de inversionistas: solo el
-- puente (con el movimiento de dinero correspondiente) puede hacerlo.
REVOKE UPDATE (estado, interes_pagado) ON sofom.contratos_mutuo FROM sc_sofom;
REVOKE INSERT, UPDATE ON sofom.cuentas FROM sc_sofom;

-- -----------------------------------------------------------------------------
-- 6. Fecha valor
-- -----------------------------------------------------------------------------
CREATE FUNCTION sofom.fn_fecha_valor_hoy() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    fecha DATE;
BEGIN
    IF TG_TABLE_NAME = 'contratos_mutuo' THEN
        fecha := NEW.fecha_inicio;
    ELSE
        fecha := NEW.fecha_desembolso;
    END IF;
    IF fecha <> current_date THEN
        RAISE EXCEPTION 'SC_FECHA_VALOR: % debe registrarse con fecha de hoy (%), no %',
            TG_TABLE_NAME, current_date, fecha
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_mutuo_fecha_valor BEFORE INSERT ON sofom.contratos_mutuo
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_fecha_valor_hoy();
CREATE TRIGGER trg_credito_fecha_valor BEFORE INSERT ON sofom.creditos
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_fecha_valor_hoy();

-- -----------------------------------------------------------------------------
-- 7. Desglose de cobros de crédito
-- -----------------------------------------------------------------------------
CREATE TABLE sofom.cobros (
    transaccion_id UUID PRIMARY KEY REFERENCES sofom.transacciones (id) ON DELETE RESTRICT,
    credito_id     UUID NOT NULL REFERENCES sofom.creditos (id) ON DELETE RESTRICT,
    principal      NUMERIC(18, 8) NOT NULL CHECK (principal >= 0),
    interes        NUMERIC(18, 8) NOT NULL CHECK (interes >= 0),
    CONSTRAINT ck_cobros_positivo CHECK (principal + interes > 0)
);
CREATE TRIGGER trg_cobros_inmutable BEFORE UPDATE OR DELETE ON sofom.cobros
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
GRANT SELECT, INSERT ON sofom.cobros TO sc_sofom;
