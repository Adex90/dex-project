-- =============================================================================
--  Synthetic Capital — 0018: posiciones del mercado en el Proof of Reserve
-- =============================================================================
--
--  Un corte de Proof of Reserve (0001, 0002, 0004) prueba el efectivo: un
--  árbol Merkle de sumas con una hoja por cuenta IFPE (disponible + bloqueado
--  + comprometido) cuya raíz suma el pasivo con clientes, igual a la reserva
--  en custodia. Las posiciones del mercado (0013: oro y plata tokenizados,
--  certificados) no estaban en el corte. Este cambio las agrega sin tocar el
--  árbol del efectivo: misma codificación v1, misma raíz.
--
--  1. Un árbol de sumas por activo (`ifpe.cortes_reserva_activos`), con una
--     hoja por cuenta con posición abierta en él: la cantidad en unidades de
--     10^-8, una sal aleatoria por hoja y corte, y el orden barajado, como en
--     el del efectivo. La hoja lleva su dominio y el ticker, así que no puede
--     pasar de un árbol a otro ni hacerse pasar por una hoja del efectivo:
--
--       hoja = SHA256("SC-POR-v1:posicion" 0x00 || ticker || 0x00 || cuenta_id[16]
--                     || cantidad_u128_be[16] || sal[32])
--       nodo = el del efectivo ("SC-POR-v1:nodo" 0x01 ...)
--
--     La identidad de la hoja es la cuenta IFPE del cliente, la misma que su
--     hoja del efectivo (una cuenta MXN por cliente). La raíz de cada árbol
--     suma las unidades que el mercado debe a sus clientes en ese activo: el
--     pasivo por activo, publicado con el corte. No hay todavía registro de la
--     custodia de los subyacentes, así que el corte prueba la inclusión y el
--     total de cada posición, no su respaldo.
--  2. El ancla del ledger del mercado (`secuencia_mercado`, `hash_mercado`):
--     la cabeza de `mercado.ordenes` en la misma foto, como el ancla del ledger
--     IFPE. Nunca retrocede.
--  3. El desglose de cada hoja del efectivo (disponible, bloqueado,
--     comprometido), fuera del hash: la hoja sigue comprometiendo el total, y
--     el cliente ve de qué se componía su saldo en el corte.
--
--  Al COMMIT de cada corte nuevo, en la misma foto REPEATABLE READ que lo
--  generó, un trigger diferido exige:
--  - el ancla del mercado igual a su cabeza, sin retroceder;
--  - toda posición abierta con la cuenta IFPE de su cliente;
--  - las hojas de posiciones iguales a las posiciones abiertas vivas (activo,
--    cuenta, cantidad), en un árbol declarado por activo y ninguno de sobra;
--  - en cada árbol, índices 0..n-1 y el número de hojas y la suma declarados;
--  - el hash de cada hoja, recalculado aquí;
--  - el desglose de cada hoja del efectivo igual al de su cuenta.
--  Las raíces las recalcula el backend cada vez que carga un corte
--  (backend/src/reserva.rs), como la del efectivo.
--
--  Los cortes anteriores no cambian: sin árboles de activos, con el ancla del
--  mercado y el desglose en NULL. Un binario anterior ya no puede publicar
--  cortes (no escribe el ancla ni el desglose): el trigger los rechaza.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. Ancla del ledger del mercado en el corte
-- -----------------------------------------------------------------------------
ALTER TABLE ifpe.cortes_reserva
    ADD COLUMN secuencia_mercado BIGINT
        CONSTRAINT ck_cortes_reserva_secuencia_mercado CHECK (secuencia_mercado >= 0),
    ADD COLUMN hash_mercado CHAR(64)
        CONSTRAINT ck_cortes_reserva_hash_mercado CHECK (hash_mercado ~ '^[0-9a-f]{64}$'),
    ADD CONSTRAINT ck_cortes_reserva_ancla_mercado
        CHECK ((secuencia_mercado IS NULL) = (hash_mercado IS NULL));

COMMENT ON COLUMN ifpe.cortes_reserva.secuencia_mercado IS
    'Última orden del ledger del mercado incluida en el corte (0018); NULL en cortes anteriores.';
COMMENT ON COLUMN ifpe.cortes_reserva.hash_mercado IS
    'tx_hash de esa orden: ancla las posiciones del corte a la cadena del mercado (0018).';

-- -----------------------------------------------------------------------------
-- 2. Desglose de cada hoja del efectivo (fuera del hash)
-- -----------------------------------------------------------------------------
-- La tabla crece con cada corte: las restricciones se agregan NOT VALID para no
-- recorrerla (las filas existentes quedan en NULL, que las cumple).
ALTER TABLE ifpe.cortes_reserva_hojas
    ADD COLUMN saldo_disponible NUMERIC(18, 8),
    ADD COLUMN saldo_bloqueado NUMERIC(18, 8),
    ADD COLUMN saldo_comprometido NUMERIC(18, 8);
ALTER TABLE ifpe.cortes_reserva_hojas
    ADD CONSTRAINT ck_cortes_hojas_desglose CHECK (
        num_nulls(saldo_disponible, saldo_bloqueado, saldo_comprometido) = 3
        OR (num_nonnulls(saldo_disponible, saldo_bloqueado, saldo_comprometido) = 3
            AND saldo_disponible >= 0 AND saldo_bloqueado >= 0 AND saldo_comprometido >= 0
            AND saldo_disponible + saldo_bloqueado + saldo_comprometido = saldo_total)
    ) NOT VALID;

COMMENT ON COLUMN ifpe.cortes_reserva_hojas.saldo_disponible IS
    'Desglose del saldo de la hoja en el corte (0018): no entra en el hash; suma saldo_total.';

-- La vista de hojas también da el desglose (columnas al final: la vista conserva sus permisos).
CREATE OR REPLACE VIEW ifpe.v_hojas_reserva AS
SELECT c.id AS cuenta_id,
       c.cliente_id,
       (c.saldo_disponible + c.saldo_bloqueado + c.saldo_comprometido)::numeric(18, 8) AS saldo_total,
       c.saldo_disponible,
       c.saldo_bloqueado,
       c.saldo_comprometido
  FROM ifpe.cuentas c
 WHERE c.estado <> 'CERRADA';

-- -----------------------------------------------------------------------------
-- 3. Árboles de posiciones: uno por activo y corte, y sus hojas
-- -----------------------------------------------------------------------------
CREATE TABLE ifpe.cortes_reserva_activos (
    corte_id       UUID NOT NULL REFERENCES ifpe.cortes_reserva (id) ON DELETE RESTRICT,
    ticker         TEXT NOT NULL REFERENCES mercado.activos (ticker) ON DELETE RESTRICT,
    merkle_root    CHAR(64) NOT NULL CHECK (merkle_root ~ '^[0-9a-f]{64}$'),
    -- Solo hay árbol para un activo con al menos una posición abierta.
    num_hojas      INTEGER NOT NULL CHECK (num_hojas > 0),
    -- Unidades que el mercado debe a sus clientes en este activo.
    total_cantidad NUMERIC(28, 8) NOT NULL CHECK (total_cantidad > 0),

    PRIMARY KEY (corte_id, ticker)
);

COMMENT ON TABLE ifpe.cortes_reserva_activos IS
    'Raíz Merkle de sumas de las posiciones de cada activo en un corte (0018): el pasivo del mercado por activo.';

CREATE TABLE ifpe.cortes_reserva_posiciones (
    corte_id  UUID NOT NULL,
    ticker    TEXT NOT NULL,
    indice    INTEGER NOT NULL CHECK (indice >= 0),
    cuenta_id UUID NOT NULL REFERENCES ifpe.cuentas (id) ON DELETE RESTRICT,
    cantidad  NUMERIC(18, 8) NOT NULL CHECK (cantidad > 0),
    sal       CHAR(64) NOT NULL CHECK (sal ~ '^[0-9a-f]{64}$'),       -- aleatoria por hoja y corte (privacidad)
    hoja_hash CHAR(64) NOT NULL CHECK (hoja_hash ~ '^[0-9a-f]{64}$'),

    PRIMARY KEY (corte_id, ticker, indice),
    CONSTRAINT ux_cortes_posiciones_cuenta UNIQUE (corte_id, ticker, cuenta_id),
    CONSTRAINT fk_cortes_posiciones_arbol FOREIGN KEY (corte_id, ticker)
        REFERENCES ifpe.cortes_reserva_activos (corte_id, ticker) ON DELETE RESTRICT
);
-- La prueba de un cliente busca sus hojas en un corte.
CREATE INDEX ix_cortes_posiciones_cuenta ON ifpe.cortes_reserva_posiciones (corte_id, cuenta_id);

COMMENT ON TABLE ifpe.cortes_reserva_posiciones IS
    'Hojas de los árboles de posiciones (0018): cuenta, cantidad y sal; el hash lo verifica la base de datos.';

-- Publicados, son inmutables, como el corte y sus hojas.
CREATE TRIGGER trg_cortes_activos_inmutable BEFORE UPDATE OR DELETE ON ifpe.cortes_reserva_activos
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_cortes_activos_no_truncate BEFORE TRUNCATE ON ifpe.cortes_reserva_activos
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_cortes_posiciones_inmutable BEFORE UPDATE OR DELETE ON ifpe.cortes_reserva_posiciones
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_cortes_posiciones_no_truncate BEFORE TRUNCATE ON ifpe.cortes_reserva_posiciones
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- -----------------------------------------------------------------------------
-- 4. Validación al COMMIT de cada corte nuevo
-- -----------------------------------------------------------------------------
-- El generador usa REPEATABLE READ: este trigger diferido ve la misma foto
-- que se usó para construir las hojas.
CREATE FUNCTION ifpe.fn_verificar_posiciones_corte() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    corte        ifpe.cortes_reserva%ROWTYPE;
    cabeza       mercado.ledger_cabeza%ROWTYPE;
    ancla_previa BIGINT;
    malos        BIGINT;
BEGIN
    SELECT * INTO corte FROM ifpe.cortes_reserva WHERE id = NEW.id;

    -- El ancla del mercado es la cabeza de su ledger y no retrocede.
    SELECT * INTO cabeza FROM mercado.ledger_cabeza WHERE id;
    IF (corte.secuencia_mercado, corte.hash_mercado) IS DISTINCT FROM (cabeza.ultima_secuencia, cabeza.ultimo_hash) THEN
        RAISE EXCEPTION 'SC_CORTE_INVALIDO: el ancla del mercado del corte (%, %) no es la cabeza de su ledger (%, %)',
            corte.secuencia_mercado, corte.hash_mercado, cabeza.ultima_secuencia, cabeza.ultimo_hash
            USING ERRCODE = 'check_violation';
    END IF;
    SELECT max(secuencia_mercado) INTO ancla_previa FROM ifpe.cortes_reserva WHERE id <> NEW.id;
    IF ancla_previa IS NOT NULL AND corte.secuencia_mercado < ancla_previa THEN
        RAISE EXCEPTION 'SC_CORTE_INVALIDO: el ancla del mercado retrocede (% < % publicada antes): el ledger del mercado fue truncado',
            corte.secuencia_mercado, ancla_previa
            USING ERRCODE = 'check_violation';
    END IF;

    -- Toda posición abierta tiene la cuenta IFPE de su cliente: sin ella su
    -- hoja no tendría identidad, y el corte la omitiría.
    SELECT count(*) INTO malos
      FROM mercado.posiciones p
     WHERE p.cantidad > 0
       AND NOT EXISTS (SELECT 1 FROM ifpe.cuentas c WHERE c.cliente_id = p.cliente_id AND c.moneda = 'MXN');
    IF malos > 0 THEN
        RAISE EXCEPTION 'SC_CORTE_INVALIDO: % posiciones abiertas sin la cuenta IFPE de su cliente', malos
            USING ERRCODE = 'check_violation';
    END IF;

    -- Las hojas de posiciones son exactamente las posiciones abiertas vivas.
    SELECT count(*) INTO malos FROM (
        (SELECT p.ticker, c.id AS cuenta_id, p.cantidad
           FROM mercado.posiciones p
           JOIN ifpe.cuentas c ON c.cliente_id = p.cliente_id AND c.moneda = 'MXN'
          WHERE p.cantidad > 0
         EXCEPT
         SELECT ticker, cuenta_id, cantidad FROM ifpe.cortes_reserva_posiciones WHERE corte_id = NEW.id)
        UNION ALL
        (SELECT ticker, cuenta_id, cantidad FROM ifpe.cortes_reserva_posiciones WHERE corte_id = NEW.id
         EXCEPT
         SELECT p.ticker, c.id, p.cantidad
           FROM mercado.posiciones p
           JOIN ifpe.cuentas c ON c.cliente_id = p.cliente_id AND c.moneda = 'MXN'
          WHERE p.cantidad > 0)
    ) d;
    IF malos > 0 THEN
        RAISE EXCEPTION 'SC_CORTE_INVALIDO: las hojas de posiciones no coinciden con las posiciones abiertas (% diferencias)',
            malos
            USING ERRCODE = 'check_violation';
    END IF;

    -- Cada árbol declarado tiene sus hojas, con índices 0..n-1, su número y su suma.
    SELECT count(*) INTO malos
      FROM ifpe.cortes_reserva_activos a
      LEFT JOIN (SELECT ticker, count(*) AS n, sum(cantidad) AS suma, min(indice) AS minimo, max(indice) AS maximo
                   FROM ifpe.cortes_reserva_posiciones
                  WHERE corte_id = NEW.id
                  GROUP BY ticker) h USING (ticker)
     WHERE a.corte_id = NEW.id
       AND (h.n IS NULL OR h.n <> a.num_hojas OR h.suma <> a.total_cantidad
            OR h.minimo <> 0 OR h.maximo <> h.n - 1);
    IF malos > 0 THEN
        RAISE EXCEPTION 'SC_CORTE_INCOMPLETO: % árboles de posiciones del corte % no cuadran (hojas, suma o índices)',
            malos, NEW.id
            USING ERRCODE = 'check_violation';
    END IF;

    -- hoja = SHA256("SC-POR-v1:posicion" 0x00 || ticker || 0x00 || cuenta_id[16] || cantidad_u128_be[16] || sal[32])
    SELECT count(*) INTO malos
      FROM ifpe.cortes_reserva_posiciones h
     WHERE h.corte_id = NEW.id
       AND h.hoja_hash <> encode(sha256(
               convert_to('SC-POR-v1:posicion', 'UTF8') || '\x00'::bytea || convert_to(h.ticker, 'UTF8')
               || '\x00'::bytea || uuid_send(h.cuenta_id)
               || '\x0000000000000000'::bytea || int8send((h.cantidad * 100000000)::bigint)
               || decode(h.sal, 'hex')), 'hex');
    IF malos > 0 THEN
        RAISE EXCEPTION 'SC_CORTE_INVALIDO: % hojas de posiciones tienen un hash que no corresponde a su contenido', malos
            USING ERRCODE = 'check_violation';
    END IF;

    -- El desglose de cada hoja del efectivo es el de su cuenta en esta foto.
    SELECT count(*) INTO malos
      FROM ifpe.cortes_reserva_hojas h
      JOIN ifpe.cuentas c ON c.id = h.cuenta_id
     WHERE h.corte_id = NEW.id
       AND (h.saldo_disponible, h.saldo_bloqueado, h.saldo_comprometido)
           IS DISTINCT FROM (c.saldo_disponible, c.saldo_bloqueado, c.saldo_comprometido);
    IF malos > 0 THEN
        RAISE EXCEPTION 'SC_CORTE_INVALIDO: el desglose de % hojas del corte no es el de su cuenta', malos
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END
$$;

CREATE CONSTRAINT TRIGGER trg_cortes_posiciones_completas
    AFTER INSERT ON ifpe.cortes_reserva
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_verificar_posiciones_corte();

-- -----------------------------------------------------------------------------
-- 5. Permisos: el auditor (que genera los cortes) lee las posiciones y la
--    cabeza del ledger del mercado, y publica los árboles. Nada más.
-- -----------------------------------------------------------------------------
GRANT USAGE ON SCHEMA mercado TO sc_auditor;
GRANT SELECT ON mercado.posiciones, mercado.ledger_cabeza TO sc_auditor;
GRANT SELECT, INSERT ON ifpe.cortes_reserva_activos, ifpe.cortes_reserva_posiciones TO sc_auditor;
