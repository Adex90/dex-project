-- =============================================================================
--  Synthetic Capital — 0007: endurecimiento del KYC (revisión adversarial de 0006)
-- =============================================================================
--
--  1. El proveedor simulado solo vale en demostración. `kyc.parametros.
--     permite_proveedor_simulado` lo fija el arranque (MODO_DEMO); con él en
--     FALSE la base de datos rechaza cualquier resultado del proveedor
--     'simulado' (su OCR repite lo que el cliente declaró: no prueba nada).
--  2. RETORNO_DE_SOFOM y TRASPASO_A_SOFOM solo los escribe sc_puente, y cada
--     RETORNO_DE_SOFOM debe ser el vencimiento de un contrato real: su espejo
--     es el RETORNO_A_IFPE del contrato (mismo monto, misma transacción) y el
--     destino es la cuenta IFPE que aportó a ese contrato. Antes, la excepción
--     de KYC para vencimientos permitía abonar a cualquier cuenta.
--  3. Un contrato de mutuo pertenece al dueño de sus dos cuentas, y una cuenta
--     de la SOFOM no cambia de dueño.
--  4. Créditos: el acreditado no cambia después del alta (el filtro RLS de
--     0006 solo miraba el INSERT) y desembolsar exige su KYC vigente.
--  5. Expediente:
--     - reabrirlo (operador) deja al menos un intento;
--     - si el proveedor falla sin dar resultado, el intento se devuelve;
--     - una CURP con demasiados resultados RECHAZADO en OTRAS cuentas
--       (últimos 30 días) ya no se envía (ataques de prueba y error);
--     - cada intento con una identidad ya registrada en otra cuenta cuenta
--       contra un límite propio: averiguar si una CURP es cliente del banco
--       deja de ser gratis.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. Proveedor simulado solo en demostración
-- -----------------------------------------------------------------------------
ALTER TABLE kyc.parametros ADD COLUMN permite_proveedor_simulado BOOLEAN NOT NULL DEFAULT FALSE;

-- CURP del expediente al registrar el resultado (para contar fallas por
-- identidad entre cuentas). NULL en resultados anteriores a 0007.
ALTER TABLE kyc.kyc_auditoria_biometrica ADD COLUMN curp_declarada VARCHAR(18);
CREATE INDEX ix_auditoria_curp ON kyc.kyc_auditoria_biometrica (curp_declarada, "timestamp")
    WHERE resultado = 'RECHAZADO';

CREATE OR REPLACE FUNCTION kyc.fn_registrar_auditoria() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    p   kyc.kyc_perfiles;
    par kyc.parametros;
BEGIN
    SELECT * INTO p FROM kyc.kyc_perfiles WHERE cliente_id = NEW.cliente_id;
    IF p.cliente_id IS NULL OR p.estado_kyc <> 'EN_REVISION'
       OR p.verificacion_en_curso IS DISTINCT FROM NEW.verificacion_id THEN
        RAISE EXCEPTION 'SC_KYC_VERIFICACION_AJENA: la verificación % no es la que está en curso para el cliente %',
            NEW.verificacion_id, NEW.cliente_id;
    END IF;
    SELECT * INTO par FROM kyc.parametros WHERE id;
    IF NEW.proveedor = 'simulado' AND NOT par.permite_proveedor_simulado THEN
        RAISE EXCEPTION 'SC_KYC_PROVEEDOR_NO_PERMITIDO: el proveedor simulado solo se admite en modo demostración';
    END IF;
    NEW.umbral_aplicado          := par.umbral_score_minimo;
    NEW.umbral_revision_aplicado := par.umbral_revision;
    NEW.ip_origen                := p.envio_ip;
    NEW.user_agent               := p.envio_user_agent;
    NEW.evidencia                := p.evidencia;
    NEW.curp_declarada           := p.curp;
    NEW."timestamp"              := clock_timestamp();
    IF NEW.resultado = 'APROBADO' AND NOT (
           NEW.prueba_vida_exitosa AND NEW.documento_valido AND NEW.coincide_identidad
           AND NEW.curp_ocr IS NOT DISTINCT FROM p.curp
           AND NEW.clave_elector_ocr IS NOT DISTINCT FROM p.ine_clave_elector
           AND coalesce(NEW.score_similitud, 0) >= par.umbral_score_minimo) THEN
        RAISE EXCEPTION 'SC_KYC_RESULTADO_INCONSISTENTE: un resultado APROBADO exige prueba de vida, documento válido, identidad coincidente y score >= %',
            par.umbral_score_minimo;
    END IF;
    RETURN NEW;
END
$$;

-- -----------------------------------------------------------------------------
-- 2. Vencimientos de mutuo ligados a su contrato
-- -----------------------------------------------------------------------------
-- Solo el puente mueve efectivo entre la bóveda y la SOFOM (RESTRICTIVE: se
-- suma a la política de KYC de 0006).
CREATE POLICY tx_puente_exclusivo ON ifpe.transacciones AS RESTRICTIVE FOR INSERT
    WITH CHECK (tipo NOT IN ('TRASPASO_A_SOFOM', 'RETORNO_DE_SOFOM')
                OR pg_has_role(current_user, 'sc_puente', 'MEMBER'));

CREATE FUNCTION ifpe.fn_verificar_retorno_de_sofom() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    t      ifpe.transacciones;
    t_xmin xid;
    s      sofom.transacciones;
    s_xmin xid;
    c      sofom.contratos_mutuo;
BEGIN
    SELECT * INTO t FROM ifpe.transacciones WHERE id = NEW.id;
    SELECT x.xmin INTO t_xmin FROM ifpe.transacciones x WHERE x.id = NEW.id;
    SELECT * INTO s FROM sofom.transacciones WHERE id = t.ref_movimiento_espejo;
    SELECT x.xmin INTO s_xmin FROM sofom.transacciones x WHERE x.id = t.ref_movimiento_espejo;
    IF s.id IS NULL OR s.tipo <> 'RETORNO_A_IFPE' OR s.monto <> t.monto
       OR s.ref_movimiento_espejo IS DISTINCT FROM t.id OR s_xmin IS DISTINCT FROM t_xmin THEN
        RAISE EXCEPTION 'SC_RETORNO_SIN_CONTRATO: el RETORNO_DE_SOFOM % no tiene su RETORNO_A_IFPE en la misma transacción', t.id;
    END IF;
    SELECT * INTO c FROM sofom.contratos_mutuo WHERE id = s.contrato_id;
    IF c.id IS NULL OR c.cuenta_ifpe_origen_id <> t.cuenta_destino_id OR c.cuenta_sofom_id <> s.cuenta_id THEN
        RAISE EXCEPTION 'SC_RETORNO_SIN_CONTRATO: el RETORNO_DE_SOFOM % no regresa a la cuenta que aportó al contrato', t.id;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_ifpe_tx_retorno_ligado
    AFTER INSERT ON ifpe.transacciones
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW WHEN (NEW.tipo = 'RETORNO_DE_SOFOM')
    EXECUTE FUNCTION ifpe.fn_verificar_retorno_de_sofom();

-- -----------------------------------------------------------------------------
-- 3. Titular de contratos y cuentas de la SOFOM
-- -----------------------------------------------------------------------------
-- Diferido: la política RLS del alta (KYC) responde primero. El nombre del
-- trigger lo hace correr antes que los demás diferidos del contrato (orden
-- alfabético): un contrato ajeno se reporta como tal, no como un descuadre.
CREATE FUNCTION sofom.fn_verificar_titular_mutuo() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM sofom.cuentas WHERE id = NEW.cuenta_sofom_id AND cliente_id = NEW.cliente_id)
       OR NOT EXISTS (SELECT 1 FROM ifpe.cuentas WHERE id = NEW.cuenta_ifpe_origen_id AND cliente_id = NEW.cliente_id) THEN
        RAISE EXCEPTION 'SC_MUTUO_TITULAR: las cuentas del contrato % no son de su titular', NEW.folio;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_mutuo_a_titular
    AFTER INSERT ON sofom.contratos_mutuo
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_verificar_titular_mutuo();

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM sofom.contratos_mutuo m
         WHERE NOT EXISTS (SELECT 1 FROM sofom.cuentas s WHERE s.id = m.cuenta_sofom_id AND s.cliente_id = m.cliente_id)
            OR NOT EXISTS (SELECT 1 FROM ifpe.cuentas i WHERE i.id = m.cuenta_ifpe_origen_id AND i.cliente_id = m.cliente_id)) THEN
        RAISE EXCEPTION 'SC_MUTUO_TITULAR: hay contratos existentes cuyas cuentas no son de su titular; revisar antes de migrar';
    END IF;
END
$$;

CREATE FUNCTION sofom.fn_titular_inmutable() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF NEW.cliente_id IS DISTINCT FROM OLD.cliente_id THEN
        RAISE EXCEPTION 'SC_CUENTA_INMUTABLE: la cuenta SOFOM % no cambia de dueño', OLD.id;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_sofom_cuentas_titular_inmutable BEFORE UPDATE OF cliente_id ON sofom.cuentas
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_titular_inmutable();

-- -----------------------------------------------------------------------------
-- 4. Créditos: acreditado fijo y KYC vigente al desembolsar
-- -----------------------------------------------------------------------------
CREATE FUNCTION sofom.fn_credito_kyc() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF NEW.acreditado_id IS DISTINCT FROM OLD.acreditado_id THEN
        RAISE EXCEPTION 'SC_CREDITO_INMUTABLE: el acreditado del crédito % no cambia', OLD.folio;
    END IF;
    IF OLD.estado = 'EN_FONDEO' AND NEW.estado = 'VIGENTE' AND NOT kyc.puede_operar(NEW.acreditado_id) THEN
        RAISE EXCEPTION 'SC_KYC_NO_VERIFICADO: el acreditado del crédito % no tiene su KYC verificado', OLD.folio;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_creditos_kyc BEFORE UPDATE OF acreditado_id, estado ON sofom.creditos
    FOR EACH ROW EXECUTE FUNCTION sofom.fn_credito_kyc();

-- -----------------------------------------------------------------------------
-- 5. Expediente: intentos
-- -----------------------------------------------------------------------------
CREATE TABLE kyc.kyc_conflictos_identidad (
    id          BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    cliente_id  UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    ocurrido_en TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX ix_conflictos_cliente ON kyc.kyc_conflictos_identidad (cliente_id, ocurrido_en);
CREATE TRIGGER trg_conflictos_inmutable BEFORE UPDATE OR DELETE ON kyc.kyc_conflictos_identidad
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- La invoca el backend cuando un envío choca con una identidad registrada en
-- otra cuenta (ese envío se revierte, este registro no).
CREATE FUNCTION kyc.registrar_conflicto_identidad(p_cliente UUID) RETURNS void
LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    INSERT INTO kyc.kyc_conflictos_identidad (cliente_id) VALUES (p_cliente)
$$;

CREATE FUNCTION kyc.conflictos_recientes(p_cliente UUID) RETURNS BIGINT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT count(*) FROM kyc.kyc_conflictos_identidad
     WHERE cliente_id = p_cliente AND ocurrido_en > clock_timestamp() - interval '30 days'
$$;

-- Resultados RECHAZADO de esa CURP en OTRAS cuentas (últimos 30 días).
CREATE FUNCTION kyc.rechazos_de_identidad(p_curp TEXT, p_cliente UUID) RETURNS BIGINT
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT count(*) FROM kyc.kyc_auditoria_biometrica
     WHERE curp_declarada = p_curp AND cliente_id <> p_cliente AND resultado = 'RECHAZADO'
       AND "timestamp" > clock_timestamp() - interval '30 days'
$$;

CREATE OR REPLACE FUNCTION kyc.fn_proteger_perfil() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    verificador BOOLEAN := pg_has_role(current_user, 'sc_kyc_verificador', 'MEMBER');
    par         kyc.parametros;
    a           kyc.kyc_auditoria_biometrica;
    a_xmin      xid;
    operador    UUID;
BEGIN
    IF NEW.cliente_id IS DISTINCT FROM OLD.cliente_id THEN
        RAISE EXCEPTION 'SC_KYC_INMUTABLE: el expediente % no cambia de cliente', OLD.cliente_id;
    END IF;
    -- Columnas que solo administra este trigger.
    IF NEW.estado_kyc IS NOT DISTINCT FROM OLD.estado_kyc THEN
        NEW.intentos              := OLD.intentos;
        NEW.verificacion_en_curso := OLD.verificacion_en_curso;
        NEW.enviado_en            := OLD.enviado_en;
        NEW.verificado_en         := OLD.verificado_en;
        NEW.revisado_por          := OLD.revisado_por;
        IF OLD.estado_kyc <> 'PENDIENTE' THEN
            NEW.envio_ip          := OLD.envio_ip;
            NEW.envio_user_agent  := OLD.envio_user_agent;
            NEW.evidencia         := OLD.evidencia;
        END IF;
    END IF;
    NEW.actualizado_por_rol := current_user;

    -- La identidad solo se edita mientras el expediente está PENDIENTE.
    IF (NEW.nombres, NEW.apellido_paterno, NEW.apellido_materno, NEW.curp, NEW.rfc, NEW.fecha_nacimiento,
        NEW.sexo, NEW.entidad_nacimiento, NEW.nacionalidad, NEW.domicilio, NEW.ine_clave_elector, NEW.ine_cic,
        NEW.ine_identificador_ciudadano, NEW.ine_emision, NEW.ine_vigencia)
       IS DISTINCT FROM
       (OLD.nombres, OLD.apellido_paterno, OLD.apellido_materno, OLD.curp, OLD.rfc, OLD.fecha_nacimiento,
        OLD.sexo, OLD.entidad_nacimiento, OLD.nacionalidad, OLD.domicilio, OLD.ine_clave_elector, OLD.ine_cic,
        OLD.ine_identificador_ciudadano, OLD.ine_emision, OLD.ine_vigencia)
       AND NOT (OLD.estado_kyc = 'PENDIENTE' AND NEW.estado_kyc = 'PENDIENTE') THEN
        RAISE EXCEPTION 'SC_KYC_IDENTIDAD_CONGELADA: la identidad del expediente % no cambia en estado %',
            OLD.cliente_id, OLD.estado_kyc;
    END IF;

    IF NEW.estado_kyc IS NOT DISTINCT FROM OLD.estado_kyc THEN
        RETURN NEW;
    END IF;

    SELECT * INTO par FROM kyc.parametros WHERE id;
    operador := nullif(current_setting('sc.operador_id', true), '')::uuid;

    IF OLD.estado_kyc = 'PENDIENTE' AND NEW.estado_kyc = 'EN_REVISION' THEN
        -- Envío a revisión (el cliente, vía sc_kyc).
        IF OLD.intentos >= par.max_intentos THEN
            RAISE EXCEPTION 'SC_KYC_SIN_INTENTOS: el expediente % agotó sus % intentos', OLD.cliente_id, par.max_intentos;
        END IF;
        IF kyc.conflictos_recientes(OLD.cliente_id) >= par.max_intentos THEN
            RAISE EXCEPTION 'SC_KYC_SIN_INTENTOS: demasiados envíos con identidades registradas en otra cuenta';
        END IF;
        IF kyc.rechazos_de_identidad(NEW.curp, OLD.cliente_id) >= par.max_intentos THEN
            RAISE EXCEPTION 'SC_KYC_IDENTIDAD_BLOQUEADA: esa CURP acumula verificaciones rechazadas en otras cuentas; acude a cumplimiento';
        END IF;
        NEW.intentos              := OLD.intentos + 1;
        NEW.verificacion_en_curso := gen_random_uuid();
        NEW.enviado_en            := clock_timestamp();
        NEW.motivo_rechazo        := NULL;
    ELSIF verificador AND (
             (OLD.estado_kyc = 'PENDIENTE'   AND NEW.estado_kyc = 'RECHAZADO')
          OR (OLD.estado_kyc = 'EN_REVISION' AND NEW.estado_kyc IN ('VERIFICADO', 'RECHAZADO', 'PENDIENTE'))
          OR (OLD.estado_kyc = 'VERIFICADO'  AND NEW.estado_kyc IN ('RECHAZADO', 'PENDIENTE'))
          OR (OLD.estado_kyc = 'RECHAZADO'   AND NEW.estado_kyc = 'PENDIENTE')) THEN
        IF OLD.estado_kyc = 'RECHAZADO' THEN
            -- Reabrir un expediente rechazado le devuelve sus intentos.
            NEW.intentos := 0;
        ELSIF NEW.estado_kyc = 'PENDIENTE' AND operador IS NOT NULL THEN
            -- Un operador que pide un nuevo envío deja al menos un intento.
            NEW.intentos := LEAST(OLD.intentos, par.max_intentos - 1);
        ELSIF OLD.estado_kyc = 'EN_REVISION' AND NEW.estado_kyc = 'PENDIENTE'
              AND current_setting('sc.reembolsar_intento', true) = 'si' THEN
            -- El proveedor falló sin evaluar nada: el intento se devuelve.
            IF EXISTS (SELECT 1 FROM kyc.kyc_auditoria_biometrica WHERE verificacion_id = OLD.verificacion_en_curso) THEN
                RAISE EXCEPTION 'SC_KYC_TRANSICION: el envío % ya tiene resultado; su intento no se devuelve',
                    OLD.verificacion_en_curso;
            END IF;
            NEW.intentos := GREATEST(OLD.intentos - 1, 0);
        ELSE
            NEW.intentos := OLD.intentos;
        END IF;
        NEW.verificacion_en_curso := OLD.verificacion_en_curso;
        NEW.enviado_en            := OLD.enviado_en;
        IF NEW.estado_kyc <> 'RECHAZADO' THEN
            NEW.motivo_rechazo := NULL;
        END IF;
    ELSE
        RAISE EXCEPTION 'SC_KYC_TRANSICION: % -> % no permitida para %', OLD.estado_kyc, NEW.estado_kyc, current_user;
    END IF;

    NEW.revisado_por := operador;
    IF operador IS NOT NULL AND NOT EXISTS (
           SELECT 1 FROM kyc.credenciales WHERE cliente_id = operador AND rol = 'OPERADOR') THEN
        RAISE EXCEPTION 'SC_KYC_OPERADOR_INVALIDO: % no es un operador', operador;
    END IF;
    IF operador = NEW.cliente_id THEN
        RAISE EXCEPTION 'SC_KYC_AUTODICTAMEN: un operador no dictamina su propio expediente';
    END IF;

    IF NEW.estado_kyc = 'VERIFICADO' THEN
        -- Evidencia del envío vigente (verificacion_en_curso no cambia aquí).
        SELECT * INTO a FROM kyc.kyc_auditoria_biometrica WHERE verificacion_id = OLD.verificacion_en_curso;
        SELECT x.xmin INTO a_xmin FROM kyc.kyc_auditoria_biometrica x WHERE x.verificacion_id = OLD.verificacion_en_curso;
        IF a.id IS NULL OR a.cliente_id <> NEW.cliente_id THEN
            RAISE EXCEPTION 'SC_KYC_SIN_EVIDENCIA: no hay resultado biométrico para la verificación en curso de %',
                NEW.cliente_id;
        END IF;
        IF NOT (a.prueba_vida_exitosa AND a.documento_valido AND a.coincide_identidad
                AND a.curp_ocr IS NOT DISTINCT FROM NEW.curp
                AND a.clave_elector_ocr IS NOT DISTINCT FROM NEW.ine_clave_elector) THEN
            RAISE EXCEPTION 'SC_KYC_EVIDENCIA_INSUFICIENTE: el resultado % no acredita prueba de vida, documento e identidad',
                a.verificacion_id;
        END IF;
        IF a.proveedor = 'simulado' AND NOT par.permite_proveedor_simulado THEN
            RAISE EXCEPTION 'SC_KYC_PROVEEDOR_NO_PERMITIDO: un resultado del proveedor simulado no verifica fuera de demostración';
        END IF;
        IF operador IS NULL THEN
            -- Automática: resultado APROBADO, score >= umbral vigente, y
            -- registrado en esta misma transacción.
            IF a.resultado <> 'APROBADO' OR coalesce(a.score_similitud, 0) < par.umbral_score_minimo
               OR a_xmin IS DISTINCT FROM pg_current_xact_id()::xid THEN
                RAISE EXCEPTION 'SC_KYC_EVIDENCIA_INSUFICIENTE: la verificación automática exige un resultado APROBADO de esta transacción con score >= %',
                    par.umbral_score_minimo;
            END IF;
        ELSIF coalesce(a.score_similitud, 0) < par.umbral_revision THEN
            RAISE EXCEPTION 'SC_KYC_EVIDENCIA_INSUFICIENTE: score % por debajo del umbral de revisión %; ni un operador puede verificarlo',
                a.score_similitud, par.umbral_revision;
        END IF;
        NEW.verificado_en := clock_timestamp();
    ELSE
        NEW.verificado_en := NULL;
    END IF;
    RETURN NEW;
END
$$;
