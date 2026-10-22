-- =============================================================================
--  Synthetic Capital — 0002: identidad, capital SOFOM y hojas de Proof of Reserve
-- =============================================================================
--
--  1. `kyc.credenciales.rol`: separa clientes de operadores/auditores. Solo el
--     dueño del esquema (arranque) puede asignar un rol distinto de CLIENTE.
--  2. Rol `sc_kyc` (alta y autenticación). Puede abrir una cuenta IFPE, pero
--     únicamente con las columnas de identidad: los saldos nacen en 0 por
--     DEFAULT y este rol no tiene privilegio para fijarlos. El onboarding no
--     puede crear dinero.
--  3. SOFOM: tipo de movimiento APORTACION_CAPITAL (capital propio a caja).
--     Los intereses del mutuo se pagan con capital/ingresos de la SOFOM, nunca
--     con dinero de la IFPE.
--  4. `ifpe.cortes_reserva_hojas`: hojas de cada corte de Proof of Reserve, para
--     que cualquier cliente pueda obtener su prueba de inclusión después.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. Rol de usuario en credenciales
-- -----------------------------------------------------------------------------
ALTER TABLE kyc.credenciales
    ADD COLUMN rol TEXT NOT NULL DEFAULT 'CLIENTE'
        CONSTRAINT ck_credenciales_rol CHECK (rol IN ('CLIENTE', 'OPERADOR', 'AUDITOR'));

-- -----------------------------------------------------------------------------
-- 2. Rol sc_kyc
-- -----------------------------------------------------------------------------
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sc_kyc') THEN
        CREATE ROLE sc_kyc NOLOGIN;
    END IF;
    EXECUTE format('GRANT sc_kyc TO %I', current_user);
END
$$;

GRANT USAGE ON SCHEMA nucleo, kyc TO sc_kyc;
GRANT SELECT, INSERT, UPDATE ON kyc.clientes TO sc_kyc;
-- `rol` queda fuera: un error en el alta nunca puede crear un operador.
GRANT SELECT ON kyc.credenciales TO sc_kyc;
GRANT INSERT (cliente_id, password_hash) ON kyc.credenciales TO sc_kyc;
GRANT UPDATE (password_hash, intentos_fallidos, bloqueado_hasta, ultimo_acceso) ON kyc.credenciales TO sc_kyc;
-- Bandeja de cumplimiento PLD.
GRANT SELECT, UPDATE ON kyc.alertas_pld TO sc_kyc;
-- Apertura de cuenta IFPE: solo identidad, nunca saldos.
GRANT USAGE ON SCHEMA ifpe TO sc_kyc;
GRANT INSERT (cliente_id, clabe) ON ifpe.cuentas TO sc_kyc;
GRANT SELECT (id, cliente_id, clabe, moneda, estado, creado_en) ON ifpe.cuentas TO sc_kyc;

-- -----------------------------------------------------------------------------
-- 3. SOFOM: aportación de capital propio a caja
-- -----------------------------------------------------------------------------
ALTER TABLE sofom.transacciones DROP CONSTRAINT transacciones_tipo_check;
ALTER TABLE sofom.transacciones ADD CONSTRAINT transacciones_tipo_check CHECK (tipo IN (
    'APORTACION_MUTUO',   -- efectivo llega de la IFPE: +caja, +bloqueado
    'VENCIMIENTO_MUTUO',  -- bloqueado -> disponible
    'PAGO_INTERES_MUTUO', -- +disponible (gasto de la SOFOM; el efectivo sale de caja en RETORNO_A_IFPE)
    'RETORNO_A_IFPE',     -- -caja, -disponible (efectivo regresa a la IFPE)
    'DESEMBOLSO_CREDITO', -- -caja, +cartera
    'COBRO_CREDITO',      -- +caja, -cartera (principal y/o intereses)
    'APORTACION_CAPITAL'  -- +caja (capital propio de la SOFOM)
));

ALTER TABLE sofom.transacciones DROP CONSTRAINT ck_sofom_tx_forma;
ALTER TABLE sofom.transacciones ADD CONSTRAINT ck_sofom_tx_forma CHECK (
    (tipo = 'APORTACION_MUTUO'   AND cuenta_id IS NOT NULL AND contrato_id IS NOT NULL AND caja_id IS NOT NULL
                                 AND credito_id IS NULL AND ref_movimiento_espejo IS NOT NULL) OR
    (tipo = 'VENCIMIENTO_MUTUO'  AND cuenta_id IS NOT NULL AND contrato_id IS NOT NULL AND caja_id IS NULL
                                 AND credito_id IS NULL) OR
    (tipo = 'PAGO_INTERES_MUTUO' AND cuenta_id IS NOT NULL AND contrato_id IS NOT NULL AND caja_id IS NOT NULL
                                 AND credito_id IS NULL) OR
    (tipo = 'RETORNO_A_IFPE'     AND cuenta_id IS NOT NULL AND caja_id IS NOT NULL
                                 AND credito_id IS NULL AND ref_movimiento_espejo IS NOT NULL) OR
    (tipo IN ('DESEMBOLSO_CREDITO', 'COBRO_CREDITO')
                                 AND credito_id IS NOT NULL AND caja_id IS NOT NULL
                                 AND cuenta_id IS NULL AND contrato_id IS NULL) OR
    (tipo = 'APORTACION_CAPITAL' AND caja_id IS NOT NULL
                                 AND cuenta_id IS NULL AND contrato_id IS NULL AND credito_id IS NULL
                                 AND ref_movimiento_espejo IS NULL)
);

-- -----------------------------------------------------------------------------
-- 4. Hojas de cada corte de Proof of Reserve
-- -----------------------------------------------------------------------------
CREATE TABLE ifpe.cortes_reserva_hojas (
    corte_id    UUID NOT NULL REFERENCES ifpe.cortes_reserva (id) ON DELETE RESTRICT,
    indice      INTEGER NOT NULL CHECK (indice >= 0),
    cuenta_id   UUID NOT NULL REFERENCES ifpe.cuentas (id) ON DELETE RESTRICT,
    saldo_total NUMERIC(18, 8) NOT NULL CHECK (saldo_total >= 0),
    sal         CHAR(64) NOT NULL CHECK (sal ~ '^[0-9a-f]{64}$'),       -- aleatoria por hoja y corte (privacidad)
    hoja_hash   CHAR(64) NOT NULL CHECK (hoja_hash ~ '^[0-9a-f]{64}$'),

    PRIMARY KEY (corte_id, indice),
    CONSTRAINT ux_cortes_hojas_cuenta UNIQUE (corte_id, cuenta_id)
);
CREATE INDEX ix_cortes_hojas_cuenta ON ifpe.cortes_reserva_hojas (cuenta_id);
CREATE TRIGGER trg_cortes_hojas_inmutable BEFORE UPDATE OR DELETE ON ifpe.cortes_reserva_hojas
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_cortes_hojas_no_truncate BEFORE TRUNCATE ON ifpe.cortes_reserva_hojas
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_cortes_no_truncate BEFORE TRUNCATE ON ifpe.cortes_reserva
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- Al COMMIT, las hojas de un corte deben sumar exactamente su total_pasivo y
-- ser tantas como num_hojas (la raíz publicada no puede omitir cuentas).
CREATE FUNCTION ifpe.fn_verificar_hojas_corte() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    corte ifpe.cortes_reserva%ROWTYPE;
    suma  NUMERIC(20, 8);
    n     INTEGER;
BEGIN
    SELECT * INTO corte FROM ifpe.cortes_reserva WHERE id = NEW.id;
    SELECT coalesce(sum(saldo_total), 0), count(*) INTO suma, n
      FROM ifpe.cortes_reserva_hojas WHERE corte_id = NEW.id;
    IF suma <> corte.total_pasivo OR n <> corte.num_hojas THEN
        RAISE EXCEPTION 'SC_CORTE_INCOMPLETO: el corte % declara % hojas / pasivo %, pero tiene % hojas / suma %',
            NEW.id, corte.num_hojas, corte.total_pasivo, n, suma
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_cortes_hojas_completas
    AFTER INSERT ON ifpe.cortes_reserva
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_verificar_hojas_corte();

GRANT SELECT, INSERT ON ifpe.cortes_reserva_hojas TO sc_auditor;
