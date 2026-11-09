-- =============================================================================
--  0012: Bóveda Soberana en tiempo real
--
--  Una versión por cuenta y avisos de PostgreSQL (LISTEN/NOTIFY) para el
--  canal WebSocket de la bóveda (backend/src/boveda). El cliente recibe su
--  estado completo una vez y después cada cambio, en orden, sin que un hueco
--  pase inadvertido (contrato: frontend/src/types/vault.ts).
--
--  1. ifpe.cuentas.seq: la versión de los saldos de la cuenta. Sube
--     exactamente una vez por cada transacción que cambia saldo_disponible o
--     saldo_bloqueado, dentro de esa misma transacción. Lo calcula un trigger
--     para todos los caminos (ledger, SPEI, puente SOFOM, retenciones PLD):
--     ningún código puede olvidarlo ni escribirlo. seq_xact guarda la
--     transacción que la subió por última vez.
--  2. Cuando seq cambia: NOTIFY sc_boveda {tipo: saldo, cliente_id, seq, xact}.
--     PostgreSQL entrega el aviso solo si la transacción confirma, y en orden
--     de confirmación: nunca se anuncia un saldo que no llegó a existir.
--  3. La causa: la transacción que acredita un SPEI entrante
--     (spei_inbound.xact) o reembolsa una orden saliente
--     (spei_outbound.xact_reverso) queda sellada con su id. El backend asocia
--     el aviso con la transferencia por ese id, sin depender del orden de las
--     sentencias dentro de la transacción.
--  4. Un corte de Proof of Reserve nuevo avisa {tipo: reserva}.
--
--  Nada de esto mueve dinero ni cambia reglas: solo versiona y describe.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. Versión de los saldos
-- -----------------------------------------------------------------------------
ALTER TABLE ifpe.cuentas
    ADD COLUMN seq BIGINT NOT NULL DEFAULT 0
        CONSTRAINT ck_ifpe_cuentas_seq_no_negativa CHECK (seq >= 0),
    ADD COLUMN seq_xact xid8;

COMMENT ON COLUMN ifpe.cuentas.seq IS
    'Versión de los saldos: +1 por transacción que cambia saldo_disponible o saldo_bloqueado (trigger trg_ifpe_cuentas_seq).';
COMMENT ON COLUMN ifpe.cuentas.seq_xact IS
    'Transacción (pg_current_xact_id) que subió seq por última vez; la misma va en el aviso sc_boveda.';

CREATE FUNCTION ifpe.fn_cuenta_seq() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_xact xid8;
BEGIN
    -- Toda cuenta nace en la versión 0, la escriba quien la escriba.
    IF TG_OP = 'INSERT' THEN
        NEW.seq := 0;
        NEW.seq_xact := NULL;
        RETURN NEW;
    END IF;
    IF (NEW.saldo_disponible, NEW.saldo_bloqueado) IS DISTINCT FROM (OLD.saldo_disponible, OLD.saldo_bloqueado) THEN
        v_xact := pg_current_xact_id();
        -- Una sola vez por transacción: varias sentencias de la misma
        -- transacción producen una única versión, la que se ve al confirmar.
        IF OLD.seq_xact IS DISTINCT FROM v_xact THEN
            NEW.seq := OLD.seq + 1;
            NEW.seq_xact := v_xact;
            RETURN NEW;
        END IF;
    END IF;
    -- Sin cambio de saldo (o ya contado): seq no se toca, ni aunque lo pida un UPDATE.
    NEW.seq := OLD.seq;
    NEW.seq_xact := OLD.seq_xact;
    RETURN NEW;
END
$$;

CREATE TRIGGER trg_ifpe_cuentas_seq BEFORE INSERT OR UPDATE ON ifpe.cuentas
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_cuenta_seq();

-- -----------------------------------------------------------------------------
-- 2. Aviso de cambio de saldo
-- -----------------------------------------------------------------------------
CREATE FUNCTION ifpe.fn_cuenta_avisar() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM pg_notify('sc_boveda', json_build_object(
        'tipo', 'saldo',
        'cliente_id', NEW.cliente_id,
        'seq', NEW.seq,
        'xact', NEW.seq_xact::text
    )::text);
    RETURN NULL;
END
$$;

-- Solo la sentencia que subió seq avisa: una transacción, un aviso por cuenta.
CREATE TRIGGER trg_ifpe_cuentas_seq_aviso AFTER UPDATE ON ifpe.cuentas
    FOR EACH ROW WHEN (NEW.seq IS DISTINCT FROM OLD.seq) EXECUTE FUNCTION ifpe.fn_cuenta_avisar();

-- -----------------------------------------------------------------------------
-- 3. La transacción que movió el dinero, en los movimientos SPEI
-- -----------------------------------------------------------------------------
ALTER TABLE ifpe.spei_inbound ADD COLUMN xact xid8;
COMMENT ON COLUMN ifpe.spei_inbound.xact IS
    'Transacción que registró el SPEI (pg_current_xact_id). Si lo acreditó, es la misma que subió la versión de la cuenta.';

-- spei_inbound es inmutable (trg_spei_inbound_inmutable): basta sellar al insertar.
CREATE FUNCTION ifpe.fn_spei_inbound_xact() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    NEW.xact := pg_current_xact_id();
    RETURN NEW;
END
$$;

CREATE TRIGGER trg_spei_inbound_xact BEFORE INSERT ON ifpe.spei_inbound
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_spei_inbound_xact();

ALTER TABLE ifpe.spei_outbound ADD COLUMN xact_reverso xid8;
COMMENT ON COLUMN ifpe.spei_outbound.xact_reverso IS
    'Transacción que reembolsó la orden (la que fijó tx_reverso_id), para asociarla con la versión de la cuenta.';

CREATE FUNCTION ifpe.fn_spei_outbound_xact() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        NEW.xact_reverso := CASE WHEN NEW.tx_reverso_id IS NOT NULL THEN pg_current_xact_id() END;
    ELSIF NEW.tx_reverso_id IS NOT NULL AND OLD.tx_reverso_id IS NULL THEN
        NEW.xact_reverso := pg_current_xact_id();
    ELSE
        NEW.xact_reverso := OLD.xact_reverso;
    END IF;
    RETURN NEW;
END
$$;

CREATE TRIGGER trg_spei_outbound_xact BEFORE INSERT OR UPDATE ON ifpe.spei_outbound
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_spei_outbound_xact();

CREATE INDEX ix_spei_inbound_xact ON ifpe.spei_inbound (cuenta_id, xact) WHERE xact IS NOT NULL;
CREATE INDEX ix_spei_outbound_xact_reverso ON ifpe.spei_outbound (cuenta_id, xact_reverso)
    WHERE xact_reverso IS NOT NULL;

-- -----------------------------------------------------------------------------
-- 4. Aviso de corte de Proof of Reserve
-- -----------------------------------------------------------------------------
CREATE FUNCTION ifpe.fn_corte_avisar() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM pg_notify('sc_boveda', json_build_object('tipo', 'reserva', 'corte_id', NEW.id)::text);
    RETURN NULL;
END
$$;

CREATE TRIGGER trg_ifpe_cortes_reserva_aviso AFTER INSERT ON ifpe.cortes_reserva
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_corte_avisar();
