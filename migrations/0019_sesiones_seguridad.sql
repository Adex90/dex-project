-- =============================================================================
--  Synthetic Capital — 0019: sesiones por dispositivo y centro de seguridad
-- =============================================================================
--
--  Hasta ahora un token de acceso (JWT de vida corta, revalidado contra la base
--  de datos en cada petición) no tenía registro propio: la única revocación era
--  cortar todos los de una persona (`kyc.credenciales.tokens_validos_desde`).
--  Este cambio da a cada inicio de sesión su fila, para que el cliente vea sus
--  dispositivos y cierre uno, o todos los demás, al instante.
--
--  1. `kyc.sesiones`: una por inicio de sesión. El token lleva su id (`sid`) y
--     cada petición la consulta: revocada, vencida o inactiva más allá de la
--     preferencia del cliente, el token deja de valer. Guarda un resumen del
--     dispositivo (navegador y sistema, derivado del User-Agent; nunca el
--     User-Agent completo), la IP del inicio, cuándo se abrió y su último uso.
--     Una revocación es definitiva y lleva su motivo; fuera de ella solo cambia
--     el último uso, que nunca retrocede. Las filas no se borran.
--  2. `kyc.preferencias_seguridad`: el cierre por inactividad que elige el
--     cliente (5, 15, 30 o 60 minutos; sin fila, el backend aplica 30). Al
--     cambiarla, las sesiones ya inactivas según la anterior se revocan
--     (`INACTIVIDAD`): alargar el plazo no revive una sesión abandonada.
--  3. `kyc.eventos_autenticacion` (0010) registra además los inicios de sesión
--     (correctos, fallidos y el bloqueo de la cuenta), los cierres y las
--     revocaciones, el cambio de contraseña y su rechazo, y el cambio de
--     preferencias, con la sesión, el dispositivo y un detalle numérico
--     (cuántas sesiones, cuántos minutos) cuando aplican. Sigue inmutable.
--  4. El cliente (rol sc_cliente, 0011) lee sus sesiones, sus eventos y su
--     preferencia, y nada más, por RLS; nunca escribe. Escribe sc_auth (el
--     inicio de sesión, el cierre, la revocación); sc_kyc lee lo que la
--     validación de cada petición necesita.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. Sesiones
-- -----------------------------------------------------------------------------
CREATE TABLE kyc.sesiones (
    -- El `sid` del token de acceso.
    id                UUID PRIMARY KEY,
    cliente_id        UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    -- Las pone la base de datos (sc_auth no las elige): el mismo instante.
    creada_en         TIMESTAMPTZ NOT NULL DEFAULT now(),
    ultimo_uso_en     TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- El `exp` del token: después, la sesión ya no vale aunque nadie la cierre.
    expira_en         TIMESTAMPTZ NOT NULL,
    -- La sesión verificó el segundo factor.
    mfa               BOOLEAN NOT NULL,
    -- "Chrome en macOS": navegador y sistema, derivados del User-Agent.
    dispositivo       TEXT NOT NULL CONSTRAINT ck_sesiones_dispositivo CHECK (char_length(dispositivo) BETWEEN 1 AND 80),
    ip                INET,
    revocada_en       TIMESTAMPTZ,
    motivo_revocacion TEXT CONSTRAINT ck_sesiones_motivo CHECK (motivo_revocacion IN (
                          'CIERRE',                    -- el cliente cerró esta sesión
                          'REVOCADA',                  -- el cliente la cerró desde otra
                          'OTRAS_REVOCADAS',           -- "cerrar todas las demás"
                          'CONTRASENA_CAMBIADA',       -- cambió la contraseña desde otra sesión
                          'CONTRASENA_RESTABLECIDA',   -- restableció la contraseña por correo
                          'MFA_ACTIVADO',              -- activó el segundo factor
                          'INACTIVIDAD',               -- inactiva al cambiar la preferencia (no revive)
                          'BLOQUEO',                   -- agotó los intentos al cambiar la contraseña
                          'OPERADOR')),                -- un operador revocó sus sesiones
    CONSTRAINT ck_sesiones_vigencia CHECK (expira_en > creada_en),
    CONSTRAINT ck_sesiones_uso CHECK (ultimo_uso_en >= creada_en),
    CONSTRAINT ck_sesiones_revocacion CHECK ((revocada_en IS NULL) = (motivo_revocacion IS NULL))
);
CREATE INDEX ix_sesiones_cliente ON kyc.sesiones (cliente_id, creada_en DESC);

COMMENT ON TABLE kyc.sesiones IS
    'Una fila por inicio de sesión (0019): el token de acceso lleva su id y cada petición la valida.';

-- Solo cambian el último uso (hacia adelante) y, una vez, la revocación.
CREATE FUNCTION kyc.fn_proteger_sesion() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'SC_SESION_INMUTABLE: las sesiones no se borran (%)', OLD.id
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF (NEW.id, NEW.cliente_id, NEW.creada_en, NEW.expira_en, NEW.mfa, NEW.dispositivo, NEW.ip)
       IS DISTINCT FROM (OLD.id, OLD.cliente_id, OLD.creada_en, OLD.expira_en, OLD.mfa, OLD.dispositivo, OLD.ip) THEN
        RAISE EXCEPTION 'SC_SESION_INMUTABLE: de la sesión % solo cambian el último uso y la revocación', OLD.id
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF OLD.revocada_en IS NOT NULL
       AND (NEW.revocada_en, NEW.motivo_revocacion) IS DISTINCT FROM (OLD.revocada_en, OLD.motivo_revocacion) THEN
        RAISE EXCEPTION 'SC_SESION_INMUTABLE: la revocación de la sesión % es definitiva', OLD.id
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF NEW.ultimo_uso_en < OLD.ultimo_uso_en THEN
        RAISE EXCEPTION 'SC_SESION_INMUTABLE: el último uso de la sesión % no retrocede', OLD.id
            USING ERRCODE = 'restrict_violation';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_sesiones_proteger BEFORE UPDATE OR DELETE ON kyc.sesiones
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_proteger_sesion();
CREATE TRIGGER trg_sesiones_no_truncate BEFORE TRUNCATE ON kyc.sesiones
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- -----------------------------------------------------------------------------
-- 2. Preferencias de seguridad
-- -----------------------------------------------------------------------------
CREATE TABLE kyc.preferencias_seguridad (
    cliente_id          UUID PRIMARY KEY REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    -- Minutos sin peticiones tras los que una sesión deja de valer.
    inactividad_minutos SMALLINT NOT NULL
        CONSTRAINT ck_preferencias_inactividad CHECK (inactividad_minutos IN (5, 15, 30, 60)),
    actualizado_en      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TRIGGER trg_preferencias_seguridad_actualizado_en BEFORE UPDATE ON kyc.preferencias_seguridad
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_tocar_actualizado_en();

COMMENT ON TABLE kyc.preferencias_seguridad IS
    'Cierre por inactividad que eligió el cliente (0019); sin fila, el backend aplica 30 minutos.';

-- -----------------------------------------------------------------------------
-- 3. Bitácora de eventos: más eventos, con sesión, dispositivo y detalle
-- -----------------------------------------------------------------------------
-- La bitácora es inmutable (filas); agregar columnas es DDL y no la toca.
ALTER TABLE kyc.eventos_autenticacion
    ADD COLUMN sesion_id UUID REFERENCES kyc.sesiones (id) ON DELETE RESTRICT,
    ADD COLUMN dispositivo TEXT
        CONSTRAINT ck_eventos_autenticacion_dispositivo CHECK (char_length(dispositivo) BETWEEN 1 AND 80),
    -- Un número: cuántas sesiones se revocaron, o los minutos de inactividad elegidos.
    ADD COLUMN detalle TEXT CONSTRAINT ck_eventos_autenticacion_detalle CHECK (detalle ~ '^[0-9]{1,6}$');

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
    'PREFERENCIAS_ACTUALIZADAS'));

-- -----------------------------------------------------------------------------
-- 4. Privilegios y RLS
-- -----------------------------------------------------------------------------
-- sc_auth abre, usa y revoca sesiones, guarda la preferencia y escribe la bitácora.
-- Una sesión nace vigente y con las fechas de la base de datos.
GRANT SELECT ON kyc.sesiones TO sc_auth;
GRANT INSERT (id, cliente_id, expira_en, mfa, dispositivo, ip) ON kyc.sesiones TO sc_auth;
GRANT UPDATE (ultimo_uso_en, revocada_en, motivo_revocacion) ON kyc.sesiones TO sc_auth;
GRANT SELECT ON kyc.preferencias_seguridad TO sc_auth;
GRANT INSERT (cliente_id, inactividad_minutos) ON kyc.preferencias_seguridad TO sc_auth;
GRANT UPDATE (inactividad_minutos) ON kyc.preferencias_seguridad TO sc_auth;
GRANT INSERT (sesion_id, dispositivo, detalle) ON kyc.eventos_autenticacion TO sc_auth;

-- sc_kyc valida cada petición: la sesión del token y la inactividad permitida.
GRANT SELECT (id, cliente_id, expira_en, ultimo_uso_en, revocada_en) ON kyc.sesiones TO sc_kyc;
GRANT SELECT (cliente_id, inactividad_minutos) ON kyc.preferencias_seguridad TO sc_kyc;

-- El cliente lee lo suyo (sin `registrado_por`, un dato interno de la bitácora).
GRANT SELECT (id, cliente_id, creada_en, ultimo_uso_en, expira_en, mfa, dispositivo, ip, revocada_en,
              motivo_revocacion) ON kyc.sesiones TO sc_cliente;
GRANT SELECT (cliente_id, inactividad_minutos, actualizado_en) ON kyc.preferencias_seguridad TO sc_cliente;
GRANT SELECT (id, cliente_id, evento, ip, ocurrido_en, sesion_id, dispositivo, detalle)
    ON kyc.eventos_autenticacion TO sc_cliente;

-- Como en 0011: una política abierta para los roles con privilegios y una
-- RESTRICTIVE para sc_cliente, que solo ve las filas del cliente actual.
ALTER TABLE kyc.sesiones ENABLE ROW LEVEL SECURITY;
ALTER TABLE kyc.preferencias_seguridad ENABLE ROW LEVEL SECURITY;
ALTER TABLE kyc.eventos_autenticacion ENABLE ROW LEVEL SECURITY;
CREATE POLICY por_privilegios ON kyc.sesiones USING (true) WITH CHECK (true);
CREATE POLICY por_privilegios ON kyc.preferencias_seguridad USING (true) WITH CHECK (true);
CREATE POLICY por_privilegios ON kyc.eventos_autenticacion USING (true) WITH CHECK (true);
CREATE POLICY solo_del_cliente ON kyc.sesiones AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual()) WITH CHECK (cliente_id = nucleo.cliente_actual());
CREATE POLICY solo_del_cliente ON kyc.preferencias_seguridad AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual()) WITH CHECK (cliente_id = nucleo.cliente_actual());
CREATE POLICY solo_del_cliente ON kyc.eventos_autenticacion AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual()) WITH CHECK (cliente_id = nucleo.cliente_actual());
