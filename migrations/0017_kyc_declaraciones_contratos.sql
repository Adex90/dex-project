-- =============================================================================
--  Synthetic Capital — 0017: declaraciones PLD y contratos en el onboarding
-- =============================================================================
--
--  El envío del expediente KYC (PENDIENTE -> EN_REVISION, POST /kyc/verificar)
--  lleva además:
--
--  1. Las declaraciones del cliente que pide la normativa PLD (conocimiento
--     del cliente): su ocupación, el origen de sus recursos (catálogo) y si
--     es, o fue en el último año, una persona políticamente expuesta (PEP), o
--     su cónyuge, familiar o colaborador cercano. Las declara el cliente y
--     viajan con su expediente; la determinación de cumplimiento
--     (`kyc.clientes.es_pep` y el nivel de riesgo) es aparte.
--  2. La aceptación de los contratos vigentes (`kyc.parametros.contratos_version`):
--     el contrato de la cuenta y de custodia con Synthetic Capital, S.A.P.I.
--     de C.V., el aviso de privacidad y la declaración bajo protesta de decir
--     verdad (datos verídicos y recursos propios). Queda la versión aceptada y
--     el instante, que fija la base de datos, junto a la IP y el navegador del
--     envío (0006): una firma electrónica simple.
--
--  La base de datos exige, al enviar, las tres declaraciones y la versión
--  vigente de los contratos, y fija el instante de la aceptación. Después del
--  envío no cambian, como la identidad (0007). Un expediente que declara PEP
--  nunca se verifica de forma automática: lo aprueba un operador (diligencia
--  reforzada). Al quedar VERIFICADO, la ocupación y el origen de los recursos
--  pasan a kyc.clientes, como la identidad (0006), y una declaración PEP marca
--  al cliente como PEP con riesgo ALTO (`ck_clientes_pep_riesgo_alto`, 0001):
--  el motor PLD (0009) le aplica su modificador y la bitácora registra el
--  aumento de riesgo.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. Versión vigente de los contratos
-- -----------------------------------------------------------------------------
ALTER TABLE kyc.parametros
    ADD COLUMN contratos_version TEXT NOT NULL DEFAULT '2026-11'
        CONSTRAINT ck_parametros_contratos_version CHECK (contratos_version ~ '^[0-9]{4}-[0-9]{2}$');

COMMENT ON COLUMN kyc.parametros.contratos_version IS
    'Versión vigente de los contratos del onboarding (0017): la que un envío debe aceptar.';

-- -----------------------------------------------------------------------------
-- 2. Declaraciones y aceptación en el expediente
-- -----------------------------------------------------------------------------
ALTER TABLE kyc.kyc_perfiles
    ADD COLUMN ocupacion TEXT
        CONSTRAINT ck_perfiles_ocupacion CHECK (char_length(btrim(ocupacion)) BETWEEN 2 AND 120),
    ADD COLUMN origen_recursos TEXT
        CONSTRAINT ck_perfiles_origen_recursos CHECK (origen_recursos IN (
            'SALARIO', 'NEGOCIO_PROPIO', 'HONORARIOS', 'INVERSIONES', 'PENSION', 'HERENCIA_O_DONACION',
            'VENTA_DE_BIENES', 'APOYO_FAMILIAR')),
    -- Declaración del cliente: es (o fue en el último año) PEP, o lo es su
    -- cónyuge, un familiar o un colaborador cercano.
    ADD COLUMN declara_pep BOOLEAN,
    ADD COLUMN contratos_version TEXT
        CONSTRAINT ck_perfiles_contratos_version CHECK (contratos_version ~ '^[0-9]{4}-[0-9]{2}$'),
    -- Lo fija el trigger al enviar: la aplicación no elige el instante.
    ADD COLUMN contratos_aceptados_en TIMESTAMPTZ;

COMMENT ON COLUMN kyc.kyc_perfiles.declara_pep IS
    'Declaración PEP del cliente (0017). La determinación de cumplimiento es kyc.clientes.es_pep.';

-- Requisitos del envío, congelamiento posterior y la regla PEP. Corre antes
-- que trg_perfiles_proteger (orden alfabético): un envío incompleto se
-- rechaza aquí con su propio código.
CREATE FUNCTION kyc.fn_declaraciones_perfil() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    vigente TEXT;
BEGIN
    -- Como la identidad (0007): solo cambian mientras el expediente está PENDIENTE.
    IF OLD.estado_kyc <> 'PENDIENTE'
       AND (NEW.ocupacion, NEW.origen_recursos, NEW.declara_pep, NEW.contratos_version, NEW.contratos_aceptados_en)
           IS DISTINCT FROM
           (OLD.ocupacion, OLD.origen_recursos, OLD.declara_pep, OLD.contratos_version, OLD.contratos_aceptados_en) THEN
        RAISE EXCEPTION 'SC_KYC_IDENTIDAD_CONGELADA: las declaraciones del expediente % no cambian en estado %',
            OLD.cliente_id, OLD.estado_kyc;
    END IF;

    IF OLD.estado_kyc = 'PENDIENTE' AND NEW.estado_kyc = 'EN_REVISION' THEN
        IF NEW.ocupacion IS NULL OR NEW.origen_recursos IS NULL OR NEW.declara_pep IS NULL THEN
            RAISE EXCEPTION 'SC_KYC_DECLARACIONES_INCOMPLETAS: el envío requiere tu ocupación, el origen de tus recursos y tu declaración PEP';
        END IF;
        SELECT contratos_version INTO vigente FROM kyc.parametros WHERE id;
        IF NEW.contratos_version IS DISTINCT FROM vigente THEN
            RAISE EXCEPTION 'SC_KYC_CONTRATOS_SIN_ACEPTAR: el envío requiere aceptar los contratos vigentes (versión %)', vigente;
        END IF;
        NEW.contratos_aceptados_en := clock_timestamp();
    END IF;

    -- Diligencia reforzada: un PEP declarado solo lo verifica un operador.
    IF NEW.estado_kyc = 'VERIFICADO' AND OLD.estado_kyc <> 'VERIFICADO' AND coalesce(NEW.declara_pep, FALSE)
       AND nullif(current_setting('sc.operador_id', true), '') IS NULL THEN
        RAISE EXCEPTION 'SC_KYC_PEP_REQUIERE_APROBACION: el expediente % declara PEP; solo un operador lo verifica',
            OLD.cliente_id;
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_perfiles_declaraciones BEFORE UPDATE ON kyc.kyc_perfiles
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_declaraciones_perfil();

-- -----------------------------------------------------------------------------
-- 3. Al verificar: las declaraciones pasan al cliente (y PEP => riesgo ALTO)
-- -----------------------------------------------------------------------------
-- La función de 0006, con la ocupación, el origen de los recursos y la PEP.
-- Nunca borra una marca PEP ni baja el riesgo: solo los eleva.
CREATE OR REPLACE FUNCTION kyc.fn_sincronizar_cliente() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    pep BOOLEAN := coalesce(NEW.declara_pep, FALSE);
BEGIN
    IF NEW.estado_kyc IS NOT DISTINCT FROM OLD.estado_kyc THEN
        RETURN NULL;
    END IF;
    IF NEW.estado_kyc = 'VERIFICADO' THEN
        -- La identidad comprobada reemplaza la declarada en el alta.
        UPDATE kyc.clientes SET
               estatus_kyc              = 'VERIFICADO',
               kyc_verificado_en        = NEW.verificado_en,
               nombre                   = NEW.nombres,
               apellido_paterno         = NEW.apellido_paterno,
               apellido_materno         = NEW.apellido_materno,
               fecha_nacimiento         = NEW.fecha_nacimiento,
               curp                     = NEW.curp,
               rfc                      = NEW.rfc,
               domicilio                = NEW.domicilio,
               nacionalidad             = NEW.nacionalidad,
               documento_identidad_tipo = 'INE',
               documento_identidad_numero = NEW.ine_clave_elector,
               ocupacion                = coalesce(NEW.ocupacion, ocupacion),
               origen_recursos          = coalesce(NEW.origen_recursos, origen_recursos),
               es_pep                   = es_pep OR pep,
               nivel_riesgo_pld         = CASE WHEN pep THEN 'ALTO' ELSE nivel_riesgo_pld END,
               riesgo_evaluado_en       = CASE WHEN pep AND nivel_riesgo_pld IS DISTINCT FROM 'ALTO'
                                               THEN clock_timestamp() ELSE riesgo_evaluado_en END
         WHERE id = NEW.cliente_id;
    ELSE
        -- SUSPENDIDO (medida de cumplimiento anterior a 0006) se conserva
        -- mientras el expediente siga rechazado.
        UPDATE kyc.clientes
           SET estatus_kyc = CASE WHEN estatus_kyc = 'SUSPENDIDO' AND NEW.estado_kyc = 'RECHAZADO'
                                  THEN 'SUSPENDIDO' ELSE NEW.estado_kyc::text END
         WHERE id = NEW.cliente_id;
    END IF;
    INSERT INTO kyc.kyc_historial
           (cliente_id, estado_anterior, estado_nuevo, verificacion_id, intentos, rol, operador_id, motivo)
    VALUES (NEW.cliente_id, OLD.estado_kyc, NEW.estado_kyc, NEW.verificacion_en_curso, NEW.intentos,
            coalesce(NEW.actualizado_por_rol, session_user), NEW.revisado_por, NEW.motivo_rechazo);
    RETURN NULL;
END
$$;

-- -----------------------------------------------------------------------------
-- 4. Permisos
-- -----------------------------------------------------------------------------
-- sc_kyc escribe las declaraciones y la versión aceptada (no el instante).
GRANT UPDATE (ocupacion, origen_recursos, declara_pep, contratos_version) ON kyc.kyc_perfiles TO sc_kyc;
