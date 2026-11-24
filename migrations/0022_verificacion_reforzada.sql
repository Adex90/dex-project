-- =============================================================================
--  Synthetic Capital — 0022: verificación reforzada (step-up) antes de las
--  acciones sensibles
-- =============================================================================
--
--  Hasta ahora una sesión abierta (0019) bastaba para sacar dinero de la
--  cuenta (retiros a una cuenta vinculada, 0016; transferencias) o para
--  cambiar a los beneficiarios de la cuenta (0020). Desde este cambio esas
--  acciones piden, justo antes, volver a comprobar una credencial: el código
--  TOTP vigente si el cliente activó el segundo factor, o su contraseña si no.
--  Una comprobación correcta abre en la sesión una ventana corta (5 minutos)
--  durante la que las acciones sensibles se aceptan; quien tiene un token
--  robado, pero no la credencial, no las hace.
--
--  1. `kyc.sesiones.step_up_valido_hasta`: el fin de esa ventana. Lo pone la
--     base de datos: escribir la columna (sc_auth, al comprobar la
--     credencial) la deja en `least(now() + kyc.vigencia_step_up(),
--     expira_en)`, sea cual sea el valor escrito, y solo en una sesión
--     vigente; escribir NULL la cierra. Una sesión nace sin ventana.
--  2. Cada ventana abierta deja `STEP_UP_VERIFICADO` en la bitácora del
--     cliente, con la sesión, la IP y el dispositivo de la petición (los pasa
--     el backend en `sc.origen_ip` y `sc.origen_dispositivo`, locales a la
--     transacción; sin ellos, los de la sesión) y, en `detalle`, los segundos
--     de la ventana. Lo escribe un trigger: no hay ventana sin su evento, y una
--     política impide a sc_auth registrar uno sin su ventana. Las
--     comprobaciones fallidas (`STEP_UP_FALLIDO`) las registra el backend, que
--     las cuenta en el mismo cupo que el inicio de sesión (5 intentos, 15
--     minutos de bloqueo, y la sesión que lo agota se cierra).
--  3. La base de datos repite la regla donde la puede comprobar por sí sola:
--     una designación de beneficiarios (0020) guarda su sesión, y esa sesión
--     debe ser del mismo cliente, seguir vigente y tener una ventana abierta
--     (`SC_STEP_UP_REQUERIDO`), la escriba quien la escriba. Los retiros y las
--     transferencias no guardan su sesión: ahí la regla la aplica el backend,
--     antes de tocar nada.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. La ventana, en la sesión
-- -----------------------------------------------------------------------------
-- Cuánto dura una verificación reforzada. Una función (y no un número en el
-- backend) para que la base de datos, que pone la fecha, sea la única fuente.
CREATE FUNCTION kyc.vigencia_step_up() RETURNS INTERVAL
LANGUAGE sql IMMUTABLE PARALLEL SAFE
AS $$ SELECT interval '5 minutes' $$;

COMMENT ON FUNCTION kyc.vigencia_step_up() IS
    'Duración de la ventana de verificación reforzada (0022): 5 minutos.';

-- Agregar una columna es DDL: las filas siguen inmutables (0019).
ALTER TABLE kyc.sesiones
    ADD COLUMN step_up_valido_hasta TIMESTAMPTZ,
    -- La ventana no sobrevive a su sesión.
    ADD CONSTRAINT ck_sesiones_step_up CHECK (step_up_valido_hasta <= expira_en);

COMMENT ON COLUMN kyc.sesiones.step_up_valido_hasta IS
    'Fin de la ventana de verificación reforzada (0022); la pone la base de datos. NULL: sin ventana.';

-- Una sesión nace sin ventana. Escribir la columna abre una nueva, con la
-- fecha de la base de datos, en una sesión vigente (no revocada ni vencida,
-- tampoco por esta misma sentencia); NULL la cierra. El trigger de 0019
-- (`trg_sesiones_proteger`) corre antes y sigue protegiendo lo demás.
CREATE FUNCTION kyc.fn_step_up_sesion() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.step_up_valido_hasta IS NOT NULL THEN
            RAISE EXCEPTION 'SC_STEP_UP_INVALIDO: la sesión % nace sin verificación reforzada', NEW.id
                USING ERRCODE = 'check_violation';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.step_up_valido_hasta IS NULL THEN
        RETURN NEW;
    END IF;
    IF OLD.revocada_en IS NOT NULL OR NEW.revocada_en IS NOT NULL OR OLD.expira_en <= now() THEN
        RAISE EXCEPTION 'SC_STEP_UP_SESION_NO_VIGENTE: la sesión % no está vigente', OLD.id
            USING ERRCODE = 'check_violation';
    END IF;
    NEW.step_up_valido_hasta := least(now() + kyc.vigencia_step_up(), OLD.expira_en);
    RETURN NEW;
END
$$;
-- `UPDATE OF`: corre siempre que la sentencia escriba la columna, aunque el
-- valor escrito coincida con el anterior.
CREATE TRIGGER trg_sesiones_step_up BEFORE INSERT OR UPDATE OF step_up_valido_hasta ON kyc.sesiones
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_step_up_sesion();

-- -----------------------------------------------------------------------------
-- 2. Bitácora de seguridad: cada ventana, y cada intento fallido
-- -----------------------------------------------------------------------------
ALTER TABLE kyc.eventos_autenticacion DROP CONSTRAINT ck_eventos_autenticacion_evento;
ALTER TABLE kyc.eventos_autenticacion ADD CONSTRAINT ck_eventos_autenticacion_evento CHECK (evento IN (
    -- 0010
    'MFA_SECRETO_GENERADO',
    'MFA_ACTIVADO',
    'MFA_VERIFICADO',
    'MFA_CODIGO_RECHAZADO',
    'MFA_DESACTIVADO',
    'RESTABLECIMIENTO_SOLICITADO',
    'CONTRASENA_RESTABLECIDA',
    -- 0019
    'INICIO_SESION',
    'INICIO_FALLIDO',
    'CUENTA_BLOQUEADA',
    'SESION_CERRADA',
    'SESION_REVOCADA',
    'SESIONES_REVOCADAS',
    'CONTRASENA_CAMBIADA',
    'CAMBIO_CONTRASENA_RECHAZADO',
    'PREFERENCIAS_ACTUALIZADAS',
    -- 0020: `detalle` es la versión guardada
    'BENEFICIARIOS_DESIGNADOS',
    -- 0021: `detalle` es la versión del W-8BEN
    'W8BEN_CERTIFICADO',
    -- 0022: `detalle` son los segundos de la ventana abierta
    'STEP_UP_VERIFICADO',
    'STEP_UP_FALLIDO'));

-- Corre con el rol de quien abre la ventana (sc_auth, o el DBA), que queda en
-- `registrado_por`; si la ventana no llega al COMMIT, su evento tampoco. La
-- IP y el dispositivo son los de la petición que verificó, no los del inicio
-- de sesión: un token usado desde otro lugar se nota en la bitácora.
CREATE FUNCTION kyc.fn_auditar_step_up() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, sesion_id, dispositivo, detalle)
    VALUES (NEW.cliente_id, 'STEP_UP_VERIFICADO',
            coalesce(nullif(current_setting('sc.origen_ip', true), '')::inet, NEW.ip),
            NEW.id,
            coalesce(nullif(current_setting('sc.origen_dispositivo', true), ''), NEW.dispositivo),
            floor(extract(epoch FROM NEW.step_up_valido_hasta - now()))::bigint::text);
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_sesiones_step_up_auditar AFTER UPDATE OF step_up_valido_hasta ON kyc.sesiones
    FOR EACH ROW WHEN (NEW.step_up_valido_hasta IS NOT NULL)
    EXECUTE FUNCTION kyc.fn_auditar_step_up();

-- sc_auth registra `STEP_UP_VERIFICADO` solo junto a su ventana: la de una
-- sesión de ese mismo cliente, abierta en esta transacción o antes y aún
-- vigente. Los demás eventos de sc_auth no cambian.
CREATE POLICY step_up_con_ventana ON kyc.eventos_autenticacion AS RESTRICTIVE FOR INSERT TO sc_auth
    WITH CHECK (evento <> 'STEP_UP_VERIFICADO' OR EXISTS (
        SELECT 1 FROM kyc.sesiones s
         WHERE s.id = eventos_autenticacion.sesion_id
           AND s.cliente_id = eventos_autenticacion.cliente_id
           AND s.step_up_valido_hasta > now()));

-- -----------------------------------------------------------------------------
-- 3. Beneficiarios: una designación nace en una sesión con su ventana abierta
-- -----------------------------------------------------------------------------
-- Corre antes de `trg_designaciones_preparar` (orden alfabético): primero
-- quién escribe, después qué escribe. Sin excepción por rol: una designación
-- solo la hace el propio cliente, desde su sesión.
CREATE FUNCTION kyc.fn_exigir_step_up_designacion() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM kyc.sesiones s
         WHERE s.id = NEW.sesion_id
           AND s.cliente_id = NEW.cliente_id
           AND s.revocada_en IS NULL
           AND s.expira_en > now()
           AND s.step_up_valido_hasta > now()) THEN
        RAISE EXCEPTION 'SC_STEP_UP_REQUERIDO: cambiar los beneficiarios pide verificar de nuevo tu identidad en esta sesión'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_designaciones_exigir_step_up BEFORE INSERT ON kyc.designaciones_beneficiarios
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_exigir_step_up_designacion();

-- -----------------------------------------------------------------------------
-- 4. Privilegios
-- -----------------------------------------------------------------------------
-- sc_auth abre (y cierra) la ventana; el valor lo pone la base de datos.
GRANT UPDATE (step_up_valido_hasta) ON kyc.sesiones TO sc_auth;
-- sc_kyc la lee al validar cada petición.
GRANT SELECT (step_up_valido_hasta) ON kyc.sesiones TO sc_kyc;
