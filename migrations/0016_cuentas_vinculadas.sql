-- =============================================================================
--  Synthetic Capital — 0016: cuentas vinculadas (lista blanca de retiros)
-- =============================================================================
--
--  Un retiro a cuenta propia (SPEI de la cuenta IFPE del cliente a su banco)
--  solo puede ir a una cuenta vinculada: una CLABE externa a nombre del propio
--  cliente. Es la regla PLD del retiro: el dinero sale solo hacia su titular.
--
--  1. `ifpe.cuentas_vinculadas`, la lista blanca. Al vincularse, la base de
--     datos exige que:
--     - el cliente pueda operar (`kyc.puede_operar`: KYC verificado y fuera de
--       la Lista de Personas Bloqueadas);
--     - el titular sea el cliente: su nombre, comparado sin acentos,
--       mayúsculas, signos ni espacios repetidos (`ifpe.nombre_comparable`),
--       es el del cliente en kyc.clientes (el mismo con el que firma sus SPEI);
--     - la CLABE sea válida, externa (nunca una de Synthetic Capital,
--       `ifpe.clabes_asignadas`) y esté activa para un solo cliente: dos
--       clientes, aunque se llamen igual, no comparten una cuenta de retiro.
--     Una cuenta vinculada no se borra ni cambia: se da de baja (ACTIVA ->
--     BAJA, con la fecha) y, para volver a usarla, se vincula de nuevo.
--  2. `ifpe.spei_outbound.cuenta_vinculada_id`: la orden de un retiro a
--     cuenta propia apunta a su cuenta vinculada. La base de datos revisa que
--     sea del mismo cliente, esté activa, tenga la CLABE de la orden y que el
--     beneficiario sea su titular; una baja concurrente espera a la orden. El
--     vínculo de una orden no cambia. El retiro sigue siendo el de 0005: un
--     RETIRO del ledger que descuenta el disponible (y la reserva) y una orden
--     PENDIENTE que el despachador lleva a LIQUIDADA, o reembolsa si se
--     rechaza o se devuelve. Las transferencias a terceros (/spei/transfer)
--     no llevan cuenta vinculada: pagan a otra persona, y PLD las evalúa por
--     su lado.
--  3. Permisos: sc_ifpe vincula, da de baja y crea órdenes con el vínculo;
--     sc_cliente solo lee sus cuentas (RLS como en 0011); sc_spei y
--     sc_auditor leen.
--
--  El backend (backend/src/patrimonio y backend/src/spei/outbound.rs) exige
--  la cuenta vinculada en todo retiro a cuenta propia (POST
--  /api/v1/wealth/withdraw y POST /api/v1/ifpe/retiros) y, con MODO_DEMO,
--  vincula una cuenta de demostración a nombre del cliente.
--
--  Orden de candados: el de 0005 (cuenta IFPE -> ... -> reserva); la cuenta
--  vinculada se bloquea (FOR SHARE) al insertar la orden, después de todo eso.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. El nombre comparable
-- -----------------------------------------------------------------------------
-- Mayúsculas, sin acentos ni diéresis (Ñ -> N), todo lo que no sea letra o
-- dígito como un espacio, sin espacios repetidos ni en los extremos. Es la
-- misma limpieza que el backend aplica al nombre de un SPEI (spei::texto_spei),
-- sin los signos que SPEI sí admite: "José-María Núñez" -> "JOSE MARIA NUNEZ".
-- translate antes de upper: el resultado no depende de la configuración
-- regional de la base de datos.
CREATE FUNCTION ifpe.nombre_comparable(p_nombre TEXT) RETURNS TEXT
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE
AS $$
    SELECT btrim(regexp_replace(
               upper(translate(p_nombre, 'áàäâéèëêíìïîóòöôúùüûñÁÀÄÂÉÈËÊÍÌÏÎÓÒÖÔÚÙÜÛÑ',
                                         'aaaaeeeeiiiioooouuuunAAAAEEEEIIIIOOOOUUUUN')),
               '[^A-Z0-9]+', ' ', 'g'))
$$;

COMMENT ON FUNCTION ifpe.nombre_comparable(TEXT) IS
    'Nombre para comparar titulares (0016): mayúsculas, sin acentos ni signos, espacios simples.';

-- -----------------------------------------------------------------------------
-- 2. Cuentas vinculadas
-- -----------------------------------------------------------------------------
CREATE TABLE ifpe.cuentas_vinculadas (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cliente_id UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    clabe      CHAR(18) NOT NULL CHECK (nucleo.clabe_valida(clabe)),
    -- Clave de 3 dígitos del banco: la de la CLABE (012 = BBVA México).
    banco      CHAR(3) NOT NULL,
    -- Titular de la cuenta en su banco: el cliente (ver el trigger de alta).
    titular    TEXT NOT NULL CHECK (length(btrim(titular)) BETWEEN 1 AND 120),
    -- Cómo se vinculó. DEMO: con MODO_DEMO, una CLABE de demostración a nombre
    -- del cliente. VERIFICACION: titularidad confirmada con el banco (flujo
    -- por construir; ver docs/FRONTEND_ROADMAP.md, módulo 16).
    origen     TEXT NOT NULL CHECK (origen IN ('DEMO', 'VERIFICACION')),
    estado     TEXT NOT NULL DEFAULT 'ACTIVA' CHECK (estado IN ('ACTIVA', 'BAJA')),
    creado_en  TIMESTAMPTZ NOT NULL DEFAULT now(),
    baja_en    TIMESTAMPTZ,

    CONSTRAINT ck_cuentas_vinculadas_banco CHECK (banco = substr(clabe, 1, 3)),
    CONSTRAINT ck_cuentas_vinculadas_baja CHECK ((estado = 'BAJA') = (baja_en IS NOT NULL))
);
-- Una CLABE externa está activa para un solo cliente (y una sola vez).
CREATE UNIQUE INDEX ux_cuentas_vinculadas_activa ON ifpe.cuentas_vinculadas (clabe) WHERE estado = 'ACTIVA';
CREATE INDEX ix_cuentas_vinculadas_cliente ON ifpe.cuentas_vinculadas (cliente_id, creado_en);

COMMENT ON TABLE ifpe.cuentas_vinculadas IS
    'Lista blanca de retiros (0016): CLABEs externas a nombre del cliente. Sin borrado; la baja es definitiva.';

-- Alta: el cliente opera, el titular es el cliente y la CLABE es externa.
CREATE FUNCTION ifpe.fn_validar_cuenta_vinculada() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_cliente TEXT;
BEGIN
    IF NEW.estado <> 'ACTIVA' OR NEW.baja_en IS NOT NULL THEN
        RAISE EXCEPTION 'SC_CUENTA_VINCULADA: una cuenta se vincula activa';
    END IF;
    IF NOT kyc.puede_operar(NEW.cliente_id) THEN
        RAISE EXCEPTION 'SC_CUENTA_VINCULADA_KYC: el cliente % no puede operar y no vincula cuentas', NEW.cliente_id;
    END IF;
    IF EXISTS (SELECT 1 FROM ifpe.clabes_asignadas WHERE clabe = NEW.clabe) THEN
        RAISE EXCEPTION 'SC_CUENTA_VINCULADA_PROPIA: la CLABE % es de Synthetic Capital, no una cuenta externa', NEW.clabe;
    END IF;
    SELECT ifpe.nombre_comparable(concat_ws(' ', nombre, apellido_paterno, apellido_materno))
      INTO v_cliente FROM kyc.clientes WHERE id = NEW.cliente_id;
    IF coalesce(v_cliente, '') = '' OR ifpe.nombre_comparable(NEW.titular) IS DISTINCT FROM v_cliente THEN
        RAISE EXCEPTION 'SC_CUENTA_VINCULADA_TITULAR: el titular de la cuenta no es el cliente %', NEW.cliente_id;
    END IF;
    NEW.creado_en := now();
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_cuentas_vinculadas_validar BEFORE INSERT ON ifpe.cuentas_vinculadas
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_validar_cuenta_vinculada();

-- Sin borrado ni cambios: solo la baja (ACTIVA -> BAJA), que fija su fecha.
CREATE FUNCTION ifpe.fn_proteger_cuenta_vinculada() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'SC_CUENTA_VINCULADA_INMUTABLE: una cuenta vinculada no se borra (se da de baja)';
    END IF;
    IF (NEW.id, NEW.cliente_id, NEW.clabe, NEW.banco, NEW.titular, NEW.origen, NEW.creado_en)
       IS DISTINCT FROM
       (OLD.id, OLD.cliente_id, OLD.clabe, OLD.banco, OLD.titular, OLD.origen, OLD.creado_en) THEN
        RAISE EXCEPTION 'SC_CUENTA_VINCULADA_INMUTABLE: los datos de la cuenta vinculada % no cambian', OLD.id;
    END IF;
    IF OLD.estado = 'ACTIVA' AND NEW.estado = 'BAJA' THEN
        NEW.baja_en := now();
    ELSIF NEW.estado IS DISTINCT FROM OLD.estado OR NEW.baja_en IS DISTINCT FROM OLD.baja_en THEN
        RAISE EXCEPTION 'SC_CUENTA_VINCULADA_INMUTABLE: la cuenta vinculada % solo pasa de ACTIVA a BAJA (una baja es definitiva)',
            OLD.id;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_cuentas_vinculadas_proteger BEFORE UPDATE OR DELETE ON ifpe.cuentas_vinculadas
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_proteger_cuenta_vinculada();
CREATE TRIGGER trg_cuentas_vinculadas_no_truncate BEFORE TRUNCATE ON ifpe.cuentas_vinculadas
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- -----------------------------------------------------------------------------
-- 3. La orden SPEI de un retiro a cuenta propia apunta a su cuenta vinculada
-- -----------------------------------------------------------------------------
ALTER TABLE ifpe.spei_outbound
    ADD COLUMN cuenta_vinculada_id UUID REFERENCES ifpe.cuentas_vinculadas (id) ON DELETE RESTRICT;
CREATE INDEX ix_spei_outbound_cuenta_vinculada ON ifpe.spei_outbound (cuenta_vinculada_id)
    WHERE cuenta_vinculada_id IS NOT NULL;

COMMENT ON COLUMN ifpe.spei_outbound.cuenta_vinculada_id IS
    'Retiro a cuenta propia (0016): la cuenta vinculada (lista blanca) a la que va la orden. NULL: pago a un tercero.';

-- Al crear la orden: cuenta del mismo cliente, activa, con la CLABE de la
-- orden y con el beneficiario por titular. Después, el vínculo no cambia.
CREATE FUNCTION ifpe.fn_validar_orden_vinculada() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v            ifpe.cuentas_vinculadas;
    titular      TEXT;
    beneficiario TEXT;
BEGIN
    IF TG_OP = 'UPDATE' THEN
        IF NEW.cuenta_vinculada_id IS DISTINCT FROM OLD.cuenta_vinculada_id THEN
            RAISE EXCEPTION 'SC_SPEI_INMUTABLE: la cuenta vinculada de la orden % no cambia', OLD.clave_rastreo;
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.cuenta_vinculada_id IS NULL THEN
        RETURN NEW;
    END IF;
    -- FOR SHARE: una baja concurrente espera a que esta orden termine, o esta
    -- orden ve la baja confirmada y se rechaza.
    SELECT * INTO v FROM ifpe.cuentas_vinculadas WHERE id = NEW.cuenta_vinculada_id FOR SHARE;
    IF v.id IS NULL OR v.cliente_id <> NEW.cliente_id OR v.estado <> 'ACTIVA' OR v.clabe <> NEW.beneficiario_clabe THEN
        RAISE EXCEPTION 'SC_SPEI_CUENTA_VINCULADA: la orden % no va a una cuenta vinculada activa de su cliente',
            NEW.clave_rastreo;
    END IF;
    titular := ifpe.nombre_comparable(v.titular);
    beneficiario := ifpe.nombre_comparable(NEW.beneficiario_nombre);
    -- SPEI admite 40 caracteres en el nombre: uno más largo llega cortado (39
    -- o 40 caracteres sin el espacio final) y entonces es el inicio del titular.
    IF NOT (beneficiario = titular
            OR (char_length(NEW.beneficiario_nombre) >= 39 AND beneficiario <> ''
                AND starts_with(titular, beneficiario))) THEN
        RAISE EXCEPTION 'SC_SPEI_CUENTA_VINCULADA: el beneficiario de la orden % no es el titular de su cuenta vinculada',
            NEW.clave_rastreo;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_spei_outbound_cuenta_vinculada
    BEFORE INSERT OR UPDATE OF cuenta_vinculada_id ON ifpe.spei_outbound
    FOR EACH ROW EXECUTE FUNCTION ifpe.fn_validar_orden_vinculada();

-- -----------------------------------------------------------------------------
-- 4. Permisos y RLS
-- -----------------------------------------------------------------------------
GRANT SELECT ON ifpe.cuentas_vinculadas TO sc_ifpe, sc_spei, sc_auditor;
GRANT INSERT (id, cliente_id, clabe, banco, titular, origen) ON ifpe.cuentas_vinculadas TO sc_ifpe;
GRANT UPDATE (estado) ON ifpe.cuentas_vinculadas TO sc_ifpe;
GRANT INSERT (cuenta_vinculada_id) ON ifpe.spei_outbound TO sc_ifpe;

-- sc_cliente lee sus cuentas vinculadas y nada más (como en 0011).
GRANT SELECT ON ifpe.cuentas_vinculadas TO sc_cliente;
ALTER TABLE ifpe.cuentas_vinculadas ENABLE ROW LEVEL SECURITY;
CREATE POLICY por_privilegios ON ifpe.cuentas_vinculadas USING (true) WITH CHECK (true);
CREATE POLICY solo_del_cliente ON ifpe.cuentas_vinculadas AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual())
    WITH CHECK (cliente_id = nucleo.cliente_actual());
