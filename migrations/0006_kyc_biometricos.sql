-- =============================================================================
--  Synthetic Capital — 0006: onboarding digital y KYC biométrico
-- =============================================================================
--
--  Identificación remota de personas físicas (CNBV / Ley Fintech, PLD/FT):
--  datos personales + INE (frente y reverso) + prueba de vida con selfie
--  comparada contra la fotografía de la credencial.
--
--  1. `kyc.estado_kyc` (ENUM) y `kyc.kyc_perfiles`: expediente de identidad de
--     cada cliente. Es la ÚNICA fuente del estado KYC; `kyc.clientes.
--     estatus_kyc` pasa a ser un espejo que mantiene la base de datos.
--  2. `kyc.kyc_auditoria_biometrica`: bitácora inmutable de cada resultado del
--     proveedor biométrico (score de similitud, prueba de vida, OCR de la INE,
--     IP de origen). Guarda huellas SHA-256 de las imágenes, nunca las imágenes.
--  3. `kyc.parametros`: umbral mínimo de similitud facial, umbral de revisión
--     manual y máximo de intentos. Solo el dueño del esquema los cambia.
--  4. Rol `sc_kyc_verificador`: el único que puede dictaminar (VERIFICADO,
--     RECHAZADO, reabrir). `sc_kyc` (las rutas del cliente) solo captura datos
--     de un expediente PENDIENTE y lo envía a revisión.
--  5. Reglas en la base de datos, no solo en el backend:
--     - máquina de estados y quién puede ejecutar cada transición;
--     - la identidad queda congelada desde el envío a revisión;
--     - VERIFICADO exige evidencia: un resultado del proveedor para la
--       verificación EN CURSO, con prueba de vida y documento válidos, OCR
--       que coincide con la CURP y la clave de elector declaradas, y score
--       >= umbral (automático, en la misma transacción) o >= umbral de
--       revisión con un operador identificado (manual);
--     - RLS en `kyc.kyc_perfiles`: `sc_kyc` solo ve como editables los
--       expedientes PENDIENTES.
--  6. Bloqueo estricto (REGLA DE NEGOCIO): Row-Level Security en
--     `ifpe.transacciones`, `sofom.contratos_mutuo` y `sofom.creditos`. Un
--     cliente sin KYC VERIFICADO no recibe depósitos (SPEI o internos), no
--     transfiere, no retira y no invierte en mutuo. La política se evalúa con
--     el estado vigente en la base de datos: verificar o revocar a un cliente
--     cambia lo que puede hacer en la misma transacción, sin que el backend
--     tenga que acordarse de nada.
--
--  Excepciones deliberadas (no son operaciones del cliente):
--     - DEPOSITO 'spei-reverso:<orden>': devuelve al cliente su propia orden
--       SPEI fallida (0005 la liga a la orden y al monto).
--     - RETIRO 'spei-correccion:<id>': corrección de un abono erróneo
--       autorizada por un operador (0005).
--     - RETORNO_DE_SOFOM, RETENCION, LIBERACION_RETENCION: vencimiento de un
--       contrato ya firmado y medidas de cumplimiento.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. Rol sc_kyc_verificador
-- -----------------------------------------------------------------------------
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sc_kyc_verificador') THEN
        CREATE ROLE sc_kyc_verificador NOLOGIN;
    END IF;
    EXECUTE format('GRANT sc_kyc_verificador TO %I', current_user);
END
$$;
GRANT USAGE ON SCHEMA nucleo, kyc TO sc_kyc_verificador;

-- -----------------------------------------------------------------------------
-- 2. Estado KYC y parámetros
-- -----------------------------------------------------------------------------
CREATE TYPE kyc.estado_kyc AS ENUM ('PENDIENTE', 'EN_REVISION', 'VERIFICADO', 'RECHAZADO');

CREATE TABLE kyc.parametros (
    id                  BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    -- Score mínimo de similitud (selfie vs. fotografía de la INE) para
    -- verificar sin intervención humana.
    umbral_score_minimo NUMERIC(5, 4) NOT NULL DEFAULT 0.8500
                            CHECK (umbral_score_minimo >= 0.5000 AND umbral_score_minimo <= 0.9999),
    -- Entre este valor y el mínimo, el expediente espera a un operador. Por
    -- debajo, ni un operador puede verificarlo.
    umbral_revision     NUMERIC(5, 4) NOT NULL DEFAULT 0.7000,
    max_intentos        SMALLINT NOT NULL DEFAULT 3 CHECK (max_intentos BETWEEN 1 AND 10),
    actualizado_en      TIMESTAMPTZ NOT NULL DEFAULT now(),

    CONSTRAINT ck_parametros_umbrales CHECK (umbral_revision >= 0.5000 AND umbral_revision <= umbral_score_minimo)
);
INSERT INTO kyc.parametros DEFAULT VALUES;
CREATE TRIGGER trg_parametros_actualizado_en BEFORE UPDATE ON kyc.parametros
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_tocar_actualizado_en();

-- -----------------------------------------------------------------------------
-- 3. Expediente KYC
-- -----------------------------------------------------------------------------
CREATE TABLE kyc.kyc_perfiles (
    cliente_id                  UUID PRIMARY KEY REFERENCES kyc.clientes (id) ON DELETE RESTRICT,

    -- Identidad declarada (y, una vez VERIFICADO, comprobada).
    nombres                     TEXT CHECK (length(btrim(nombres)) BETWEEN 1 AND 120),
    apellido_paterno            TEXT CHECK (length(btrim(apellido_paterno)) BETWEEN 1 AND 120),
    apellido_materno            TEXT CHECK (length(btrim(apellido_materno)) BETWEEN 1 AND 120),
    apellidos                   TEXT GENERATED ALWAYS AS (apellido_paterno || coalesce(' ' || apellido_materno, '')) STORED,
    curp                        VARCHAR(18) CHECK (curp ~ '^[A-Z]{4}[0-9]{6}[HMX][A-Z]{5}[A-Z0-9][0-9]$'),
    rfc                         VARCHAR(13) CHECK (rfc ~ '^[A-ZÑ&]{4}[0-9]{6}[A-Z0-9]{3}$'),
    fecha_nacimiento            DATE CHECK (fecha_nacimiento >= DATE '1900-01-01'),
    sexo                        CHAR(1) CHECK (sexo IN ('H', 'M', 'X')),
    entidad_nacimiento          CHAR(2) CHECK (entidad_nacimiento ~ '^[A-Z]{2}$'),
    nacionalidad                CHAR(2) NOT NULL DEFAULT 'MX' CHECK (nacionalidad ~ '^[A-Z]{2}$'),
    domicilio                   JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(domicilio) = 'object'),

    -- Credencial para votar (INE).
    ine_clave_elector           VARCHAR(18) CHECK (ine_clave_elector ~ '^[A-Z]{6}[0-9]{8}[HMX][0-9]{3}$'),
    ine_cic                     VARCHAR(9) CHECK (ine_cic ~ '^[0-9]{9}$'),
    ine_identificador_ciudadano VARCHAR(13) CHECK (ine_identificador_ciudadano ~ '^[0-9]{9}([0-9]{4})?$'),
    ine_emision                 SMALLINT CHECK (ine_emision BETWEEN 1990 AND 2100),
    ine_vigencia                SMALLINT CHECK (ine_vigencia BETWEEN 1990 AND 2100),

    -- Estado y verificación.
    estado_kyc                  kyc.estado_kyc NOT NULL DEFAULT 'PENDIENTE',
    intentos                    SMALLINT NOT NULL DEFAULT 0 CHECK (intentos >= 0),
    -- La genera la base de datos en cada envío a revisión: un resultado del
    -- proveedor solo vale para el envío vigente.
    verificacion_en_curso       UUID UNIQUE,
    enviado_en                  TIMESTAMPTZ,
    envio_ip                    INET,
    envio_user_agent            TEXT CHECK (length(envio_user_agent) <= 512),
    -- Huellas SHA-256 de las imágenes del envío vigente (INE y selfie).
    evidencia                   JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(evidencia) = 'object'),
    metadata_proveedor          JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(metadata_proveedor) = 'object'),
    motivo_rechazo              TEXT CHECK (length(motivo_rechazo) <= 500),
    verificado_en               TIMESTAMPTZ,
    -- Operador que ejecutó la última transición manual (NULL = automática).
    revisado_por                UUID REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    actualizado_por_rol         TEXT,
    creado_en                   TIMESTAMPTZ NOT NULL DEFAULT now(),
    actualizado_en              TIMESTAMPTZ NOT NULL DEFAULT now(),

    -- Un expediente solo se envía completo.
    CONSTRAINT ck_perfil_envio_completo CHECK (estado_kyc <> 'EN_REVISION' OR (
        nombres IS NOT NULL AND apellido_paterno IS NOT NULL AND curp IS NOT NULL AND rfc IS NOT NULL
        AND fecha_nacimiento IS NOT NULL AND sexo IS NOT NULL AND entidad_nacimiento IS NOT NULL
        AND ine_clave_elector IS NOT NULL AND ine_cic IS NOT NULL AND ine_emision IS NOT NULL
        AND ine_vigencia IS NOT NULL AND verificacion_en_curso IS NOT NULL AND enviado_en IS NOT NULL
        AND domicilio ?& ARRAY['calle', 'numero_exterior', 'colonia', 'municipio', 'estado', 'codigo_postal']
        AND domicilio ->> 'codigo_postal' ~ '^[0-9]{5}$'
        AND evidencia ?& ARRAY['ine_frente', 'ine_reverso', 'selfie']
    )),
    CONSTRAINT ck_perfil_vigencia      CHECK (ine_vigencia IS NULL OR ine_emision IS NULL OR ine_vigencia >= ine_emision),
    CONSTRAINT ck_perfil_verificado    CHECK (estado_kyc <> 'VERIFICADO' OR verificado_en IS NOT NULL),
    CONSTRAINT ck_perfil_rechazo       CHECK (estado_kyc <> 'RECHAZADO' OR motivo_rechazo IS NOT NULL)
);
-- La identidad es única entre expedientes enviados o verificados. Un dato
-- solo DECLARADO no bloquea a nadie: si no, cualquiera podría registrar la
-- CURP de otra persona para impedirle abrir su cuenta.
CREATE UNIQUE INDEX ux_perfiles_curp ON kyc.kyc_perfiles (curp) WHERE estado_kyc IN ('EN_REVISION', 'VERIFICADO');
CREATE UNIQUE INDEX ux_perfiles_clave_elector ON kyc.kyc_perfiles (ine_clave_elector)
    WHERE estado_kyc IN ('EN_REVISION', 'VERIFICADO');
CREATE UNIQUE INDEX ux_perfiles_rfc ON kyc.kyc_perfiles (rfc) WHERE estado_kyc IN ('EN_REVISION', 'VERIFICADO');
CREATE INDEX ix_perfiles_estado ON kyc.kyc_perfiles (estado_kyc, enviado_en);
CREATE TRIGGER trg_perfiles_actualizado_en BEFORE UPDATE ON kyc.kyc_perfiles
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_tocar_actualizado_en();

-- Mismo criterio en kyc.clientes: los CURP/RFC del alta son declarados; la
-- unicidad aplica a la identidad VERIFICADA (la copia el expediente).
ALTER TABLE kyc.clientes DROP CONSTRAINT clientes_curp_key;
ALTER TABLE kyc.clientes DROP CONSTRAINT clientes_rfc_key;
CREATE UNIQUE INDEX ux_clientes_curp_verificada ON kyc.clientes (curp) WHERE estatus_kyc = 'VERIFICADO';
CREATE UNIQUE INDEX ux_clientes_rfc_verificado ON kyc.clientes (rfc) WHERE estatus_kyc = 'VERIFICADO';

-- -----------------------------------------------------------------------------
-- 4. Auditoría biométrica (inmutable)
-- -----------------------------------------------------------------------------
CREATE TABLE kyc.kyc_auditoria_biometrica (
    id                       BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    cliente_id               UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    verificacion_id          UUID NOT NULL UNIQUE,
    proveedor                TEXT NOT NULL CHECK (length(proveedor) BETWEEN 1 AND 64),
    referencia_proveedor     TEXT CHECK (length(referencia_proveedor) <= 128),
    -- NULL: no hubo rostro comparable (p. ej. imagen ilegible).
    score_similitud          NUMERIC(5, 4) CHECK (score_similitud BETWEEN 0 AND 1),
    prueba_vida_exitosa      BOOLEAN NOT NULL,
    documento_valido         BOOLEAN NOT NULL,
    -- OCR de la INE: la base de datos lo compara con el expediente al verificar.
    curp_ocr                 VARCHAR(18),
    clave_elector_ocr        VARCHAR(18),
    -- Cruce completo hecho por el backend (nombres, fecha, sexo, MRZ, vigencia).
    coincide_identidad       BOOLEAN NOT NULL,
    resultado                TEXT NOT NULL CHECK (resultado IN ('APROBADO', 'REVISION', 'RECHAZADO')),
    -- Los fija el trigger con kyc.parametros: el proveedor no elige su umbral.
    umbral_aplicado          NUMERIC(5, 4) NOT NULL,
    umbral_revision_aplicado NUMERIC(5, 4) NOT NULL,
    -- Del envío del cliente (los copia el trigger desde el expediente).
    ip_origen                INET,
    user_agent               TEXT,
    evidencia                JSONB NOT NULL DEFAULT '{}'::jsonb,
    "timestamp"              TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    detalles_fallo           JSONB NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(detalles_fallo) = 'array'),
    -- Resultado firmado tal como llegó (cabecera X-Kyc-Firma y cuerpo).
    firma                    TEXT NOT NULL,
    resultado_crudo          JSONB NOT NULL
);
CREATE INDEX ix_auditoria_cliente ON kyc.kyc_auditoria_biometrica (cliente_id, "timestamp");

CREATE FUNCTION kyc.fn_registrar_auditoria() RETURNS trigger
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
    NEW.umbral_aplicado          := par.umbral_score_minimo;
    NEW.umbral_revision_aplicado := par.umbral_revision;
    NEW.ip_origen                := p.envio_ip;
    NEW.user_agent               := p.envio_user_agent;
    NEW.evidencia                := p.evidencia;
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
CREATE TRIGGER trg_auditoria_registrar BEFORE INSERT ON kyc.kyc_auditoria_biometrica
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_registrar_auditoria();
CREATE TRIGGER trg_auditoria_inmutable BEFORE UPDATE OR DELETE ON kyc.kyc_auditoria_biometrica
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_auditoria_no_truncate BEFORE TRUNCATE ON kyc.kyc_auditoria_biometrica
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- Historial de transiciones (lo escribe solo la base de datos).
CREATE TABLE kyc.kyc_historial (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    cliente_id       UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    estado_anterior  kyc.estado_kyc,
    estado_nuevo     kyc.estado_kyc NOT NULL,
    verificacion_id  UUID,
    intentos         SMALLINT NOT NULL,
    rol              TEXT NOT NULL,
    operador_id      UUID,
    motivo           TEXT,
    ocurrido_en      TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX ix_historial_cliente ON kyc.kyc_historial (cliente_id, id);
CREATE TRIGGER trg_historial_inmutable BEFORE UPDATE OR DELETE ON kyc.kyc_historial
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_historial_no_truncate BEFORE TRUNCATE ON kyc.kyc_historial
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- -----------------------------------------------------------------------------
-- 5. Reglas del expediente
-- -----------------------------------------------------------------------------
-- Se ejecuta con los privilegios de quien actualiza (no SECURITY DEFINER):
-- current_user es el rol de la transacción (sc_kyc o sc_kyc_verificador).
CREATE FUNCTION kyc.fn_proteger_perfil() RETURNS trigger
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
        NEW.intentos              := OLD.intentos + 1;
        NEW.verificacion_en_curso := gen_random_uuid();
        NEW.enviado_en            := clock_timestamp();
        NEW.motivo_rechazo        := NULL;
    ELSIF verificador AND (
             (OLD.estado_kyc = 'PENDIENTE'   AND NEW.estado_kyc = 'RECHAZADO')
          OR (OLD.estado_kyc = 'EN_REVISION' AND NEW.estado_kyc IN ('VERIFICADO', 'RECHAZADO', 'PENDIENTE'))
          OR (OLD.estado_kyc = 'VERIFICADO'  AND NEW.estado_kyc IN ('RECHAZADO', 'PENDIENTE'))
          OR (OLD.estado_kyc = 'RECHAZADO'   AND NEW.estado_kyc = 'PENDIENTE')) THEN
        -- Reabrir un expediente rechazado le devuelve sus intentos.
        IF OLD.estado_kyc = 'RECHAZADO' THEN
            NEW.intentos := 0;
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
CREATE TRIGGER trg_perfiles_proteger BEFORE UPDATE ON kyc.kyc_perfiles
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_proteger_perfil();
CREATE TRIGGER trg_perfiles_no_delete BEFORE DELETE ON kyc.kyc_perfiles
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- Espejo en kyc.clientes + historial. SECURITY DEFINER: ningún rol de la
-- aplicación puede escribir kyc.clientes.estatus_kyc directamente.
CREATE FUNCTION kyc.fn_sincronizar_cliente() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
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
               documento_identidad_numero = NEW.ine_clave_elector
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
CREATE TRIGGER trg_perfiles_sincronizar AFTER UPDATE OF estado_kyc ON kyc.kyc_perfiles
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_sincronizar_cliente();

-- Todo cliente nace con su expediente. sc_kyc no puede fijar estatus_kyc al
-- dar de alta (ver permisos): un alta que no es PENDIENTE solo puede venir
-- del dueño del esquema (operadores creados en el arranque).
CREATE FUNCTION kyc.fn_crear_perfil() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    estado kyc.estado_kyc;
BEGIN
    estado := CASE NEW.estatus_kyc
        WHEN 'VERIFICADO' THEN 'VERIFICADO'
        WHEN 'RECHAZADO'  THEN 'RECHAZADO'
        WHEN 'SUSPENDIDO' THEN 'RECHAZADO'
        WHEN 'PENDIENTE'  THEN 'PENDIENTE'
    END::kyc.estado_kyc;
    IF estado IS NULL THEN
        RAISE EXCEPTION 'SC_KYC_ALTA_INVALIDA: un cliente no se da de alta en estado %', NEW.estatus_kyc;
    END IF;
    INSERT INTO kyc.kyc_perfiles
           (cliente_id, nombres, apellido_paterno, apellido_materno, curp, rfc, fecha_nacimiento, nacionalidad,
            domicilio, estado_kyc, verificado_en, motivo_rechazo, metadata_proveedor, actualizado_por_rol)
    VALUES (NEW.id, NEW.nombre, NEW.apellido_paterno, NEW.apellido_materno, NEW.curp,
            CASE WHEN length(NEW.rfc) = 13 THEN NEW.rfc END, NEW.fecha_nacimiento, NEW.nacionalidad,
            CASE WHEN jsonb_typeof(NEW.domicilio) = 'object' THEN NEW.domicilio ELSE '{}'::jsonb END,
            estado,
            CASE WHEN estado = 'VERIFICADO' THEN NEW.kyc_verificado_en END,
            CASE WHEN estado = 'RECHAZADO' THEN 'alta administrativa en estado ' || NEW.estatus_kyc END,
            CASE WHEN estado = 'PENDIENTE' THEN '{}'::jsonb ELSE '{"origen": "ALTA_ADMINISTRATIVA"}'::jsonb END,
            session_user);
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_clientes_crear_perfil AFTER INSERT ON kyc.clientes
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_crear_perfil();

-- Al COMMIT, el espejo coincide con el expediente (nadie, ni el dueño del
-- esquema, cambia estatus_kyc por fuera del expediente).
CREATE FUNCTION kyc.fn_verificar_espejo_kyc() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    c kyc.clientes;
    e kyc.estado_kyc;
BEGIN
    SELECT * INTO c FROM kyc.clientes WHERE id = NEW.id;
    IF c.id IS NULL THEN
        RETURN NULL;
    END IF;
    SELECT estado_kyc INTO e FROM kyc.kyc_perfiles WHERE cliente_id = NEW.id;
    IF e IS NULL OR NOT (c.estatus_kyc = e::text OR (c.estatus_kyc = 'SUSPENDIDO' AND e = 'RECHAZADO')) THEN
        RAISE EXCEPTION 'SC_KYC_ESPEJO: estatus_kyc % del cliente % no coincide con su expediente (%)',
            c.estatus_kyc, NEW.id, e;
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_clientes_espejo_kyc
    AFTER INSERT OR UPDATE OF estatus_kyc ON kyc.clientes
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_verificar_espejo_kyc();

-- -----------------------------------------------------------------------------
-- 6. Expedientes de los clientes existentes
-- -----------------------------------------------------------------------------
-- EN_REVISION anterior a 0006 no tenía evidencia biométrica: vuelve a
-- PENDIENTE para completar el proceso nuevo. VERIFICADO se conserva (se
-- verificó con el proceso vigente entonces) y queda marcado como tal.
UPDATE kyc.clientes SET estatus_kyc = 'PENDIENTE' WHERE estatus_kyc = 'EN_REVISION';
INSERT INTO kyc.kyc_perfiles
       (cliente_id, nombres, apellido_paterno, apellido_materno, curp, rfc, fecha_nacimiento, nacionalidad,
        domicilio, ine_clave_elector, estado_kyc, verificado_en, motivo_rechazo, metadata_proveedor,
        actualizado_por_rol)
SELECT c.id, c.nombre, c.apellido_paterno, c.apellido_materno, c.curp,
       CASE WHEN length(c.rfc) = 13 THEN c.rfc END, c.fecha_nacimiento, c.nacionalidad,
       CASE WHEN jsonb_typeof(c.domicilio) = 'object' THEN c.domicilio ELSE '{}'::jsonb END,
       CASE WHEN c.documento_identidad_tipo = 'INE'
                 AND c.documento_identidad_numero ~ '^[A-Z]{6}[0-9]{8}[HMX][0-9]{3}$'
            THEN c.documento_identidad_numero END,
       CASE c.estatus_kyc WHEN 'VERIFICADO' THEN 'VERIFICADO' WHEN 'PENDIENTE' THEN 'PENDIENTE' ELSE 'RECHAZADO' END::kyc.estado_kyc,
       CASE WHEN c.estatus_kyc = 'VERIFICADO' THEN coalesce(c.kyc_verificado_en, c.creado_en) END,
       CASE WHEN c.estatus_kyc IN ('RECHAZADO', 'SUSPENDIDO') THEN 'estado ' || c.estatus_kyc || ' anterior a 0006' END,
       CASE WHEN c.estatus_kyc = 'PENDIENTE' THEN '{}'::jsonb
            ELSE jsonb_build_object('origen', 'MIGRACION_0006', 'estatus_previo', c.estatus_kyc) END,
       session_user
  FROM kyc.clientes c
 WHERE NOT EXISTS (SELECT 1 FROM kyc.kyc_perfiles p WHERE p.cliente_id = c.id);

-- -----------------------------------------------------------------------------
-- 7. Bloqueo por KYC en la base de datos (Row-Level Security)
-- -----------------------------------------------------------------------------
-- Un cliente opera si su expediente Y su espejo dicen VERIFICADO y no está en
-- la Lista de Personas Bloqueadas.
CREATE FUNCTION kyc.puede_operar(p_cliente UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT EXISTS (
        SELECT 1
          FROM kyc.clientes c
          JOIN kyc.kyc_perfiles p ON p.cliente_id = c.id
         WHERE c.id = p_cliente
           AND c.estatus_kyc = 'VERIFICADO'
           AND p.estado_kyc = 'VERIFICADO'
           AND NOT c.en_lista_bloqueadas)
$$;

CREATE FUNCTION ifpe.cuenta_puede_operar(p_cuenta UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT coalesce((SELECT kyc.puede_operar(cliente_id) FROM ifpe.cuentas WHERE id = p_cuenta), FALSE)
$$;

CREATE FUNCTION ifpe.kyc_permite_movimiento(p_tipo TEXT, p_origen UUID, p_destino UUID, p_clave TEXT)
RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT CASE p_tipo
        -- Entra dinero a la cuenta (SPEI recibido o depósito de demostración).
        WHEN 'DEPOSITO'         THEN starts_with(p_clave, 'spei-reverso:') OR ifpe.cuenta_puede_operar(p_destino)
        WHEN 'TRANSFERENCIA'    THEN ifpe.cuenta_puede_operar(p_origen) AND ifpe.cuenta_puede_operar(p_destino)
        WHEN 'RETIRO'           THEN starts_with(p_clave, 'spei-correccion:') OR ifpe.cuenta_puede_operar(p_origen)
        -- Inversión en mutuo.
        WHEN 'TRASPASO_A_SOFOM' THEN ifpe.cuenta_puede_operar(p_origen)
        -- RETORNO_DE_SOFOM, RETENCION, LIBERACION_RETENCION.
        ELSE TRUE
    END
$$;

ALTER TABLE ifpe.transacciones ENABLE ROW LEVEL SECURITY;
CREATE POLICY tx_lectura ON ifpe.transacciones FOR SELECT USING (true);
CREATE POLICY tx_alta_con_kyc ON ifpe.transacciones FOR INSERT
    WITH CHECK (ifpe.kyc_permite_movimiento(tipo, cuenta_origen_id, cuenta_destino_id, clave_idempotencia));
-- Sin políticas de UPDATE/DELETE: el ledger ya es inmutable (trigger) y RLS
-- además lo niega por omisión.

ALTER TABLE sofom.contratos_mutuo ENABLE ROW LEVEL SECURITY;
CREATE POLICY mutuo_lectura ON sofom.contratos_mutuo FOR SELECT USING (true);
CREATE POLICY mutuo_actualizacion ON sofom.contratos_mutuo FOR UPDATE USING (true) WITH CHECK (true);
CREATE POLICY mutuo_alta_con_kyc ON sofom.contratos_mutuo FOR INSERT WITH CHECK (kyc.puede_operar(cliente_id));

ALTER TABLE sofom.creditos ENABLE ROW LEVEL SECURITY;
CREATE POLICY creditos_lectura ON sofom.creditos FOR SELECT USING (true);
CREATE POLICY creditos_actualizacion ON sofom.creditos FOR UPDATE USING (true) WITH CHECK (true);
CREATE POLICY creditos_alta_con_kyc ON sofom.creditos FOR INSERT WITH CHECK (kyc.puede_operar(acreditado_id));

-- El expediente: sc_kyc solo edita expedientes PENDIENTES y solo puede
-- dejarlos PENDIENTES o enviarlos a revisión. Dictaminar es de
-- sc_kyc_verificador (el trigger valida además cada transición).
ALTER TABLE kyc.kyc_perfiles ENABLE ROW LEVEL SECURITY;
CREATE POLICY perfiles_lectura ON kyc.kyc_perfiles FOR SELECT USING (true);
CREATE POLICY perfiles_captura ON kyc.kyc_perfiles FOR UPDATE TO sc_kyc
    USING (estado_kyc = 'PENDIENTE')
    WITH CHECK (estado_kyc IN ('PENDIENTE', 'EN_REVISION'));
CREATE POLICY perfiles_dictamen ON kyc.kyc_perfiles FOR UPDATE TO sc_kyc_verificador
    USING (true) WITH CHECK (true);

-- -----------------------------------------------------------------------------
-- 8. Permisos
-- -----------------------------------------------------------------------------
-- sc_kyc ya no fija el estado KYC, el riesgo ni la identidad verificada de un
-- cliente: solo da de alta con los datos declarados y edita su perfil
-- transaccional.
REVOKE INSERT, UPDATE ON kyc.clientes FROM sc_kyc;
GRANT INSERT (id, tipo_persona, nombre, apellido_paterno, apellido_materno, fecha_nacimiento, nacionalidad,
              pais_residencia, rfc, curp, email, telefono, domicilio, actividad_economica, ocupacion,
              origen_recursos, destino_recursos, ingreso_mensual_estimado)
    ON kyc.clientes TO sc_kyc;
GRANT UPDATE (telefono, actividad_economica, ocupacion, origen_recursos, destino_recursos, ingreso_mensual_estimado)
    ON kyc.clientes TO sc_kyc;
GRANT SELECT ON kyc.kyc_perfiles, kyc.parametros, kyc.kyc_historial TO sc_kyc;
GRANT SELECT (id, cliente_id, verificacion_id, proveedor, score_similitud, prueba_vida_exitosa, documento_valido,
              coincide_identidad, resultado, umbral_aplicado, "timestamp", detalles_fallo)
    ON kyc.kyc_auditoria_biometrica TO sc_kyc;
GRANT UPDATE (nombres, apellido_paterno, apellido_materno, curp, rfc, fecha_nacimiento, sexo, entidad_nacimiento,
              nacionalidad, domicilio, ine_clave_elector, ine_cic, ine_identificador_ciudadano, ine_emision,
              ine_vigencia, envio_ip, envio_user_agent, evidencia, estado_kyc)
    ON kyc.kyc_perfiles TO sc_kyc;

-- sc_kyc_verificador: registra resultados del proveedor y dictamina.
GRANT SELECT ON kyc.clientes, kyc.kyc_perfiles, kyc.parametros, kyc.kyc_historial,
                kyc.kyc_auditoria_biometrica, kyc.alertas_pld TO sc_kyc_verificador;
GRANT SELECT (cliente_id, rol) ON kyc.credenciales TO sc_kyc_verificador;
GRANT INSERT (cliente_id, verificacion_id, proveedor, referencia_proveedor, score_similitud, prueba_vida_exitosa,
              documento_valido, curp_ocr, clave_elector_ocr, coincide_identidad, resultado, detalles_fallo,
              firma, resultado_crudo)
    ON kyc.kyc_auditoria_biometrica TO sc_kyc_verificador;
GRANT UPDATE (estado_kyc, motivo_rechazo, metadata_proveedor) ON kyc.kyc_perfiles TO sc_kyc_verificador;
GRANT UPDATE (nivel_riesgo_pld, riesgo_evaluado_en, nivel_cuenta) ON kyc.clientes TO sc_kyc_verificador;
