-- =============================================================================
--  Synthetic Capital — 0010: segundo factor (TOTP) y restablecimiento de contraseña
-- =============================================================================
--
--  1. `kyc.credenciales`:
--     - `totp_secret`: el secreto TOTP **cifrado por la aplicación** (AES-256-GCM,
--       clave fuera de la base de datos, `MFA_CLAVE_CIFRADO`). El formato se
--       verifica: un secreto en claro (base32) no cabe en la columna.
--     - `mfa_enabled`: el segundo factor está activo (el primer código ya se verificó).
--     - `password_reset_token`: el **hash** SHA-256 del token de restablecimiento,
--       nunca el token; `token_expires_at`, su vencimiento (un solo uso).
--     - `totp_ultimo_paso`: último paso de 30 s aceptado; un código no se acepta
--       dos veces (anti-repetición).
--  2. Reglas (trigger): no se activa MFA sin secreto; con MFA activo la aplicación
--     no puede desactivarlo ni cambiar el secreto: solo el dueño del esquema (el
--     DBA, p. ej. por pérdida del dispositivo), y queda en la bitácora.
--  3. Rol `sc_auth`: el único rol transaccional que lee o escribe los secretos.
--     `sc_kyc` cambia su SELECT de tabla por uno por columna, sin las columnas
--     secretas: el alta y el expediente KYC ya no pueden leerlas.
--  4. `kyc.eventos_autenticacion`: bitácora inmutable de eventos de seguridad
--     (inscripción y uso del segundo factor, restablecimientos).
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 1. Rol sc_auth
-- -----------------------------------------------------------------------------
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sc_auth') THEN
        CREATE ROLE sc_auth NOLOGIN;
    END IF;
    EXECUTE format('GRANT sc_auth TO %I', current_user);
END
$$;

-- -----------------------------------------------------------------------------
-- 2. Columnas de segundo factor y restablecimiento
-- -----------------------------------------------------------------------------
-- Formato del cifrado v1: "v1:" + base64 de nonce (12) || texto cifrado (20) || etiqueta (16),
-- 48 bytes = 64 caracteres exactos. Un secreto base32 de 20 bytes (32 caracteres) no lo cumple.
ALTER TABLE kyc.credenciales
    ADD COLUMN totp_secret          VARCHAR(255)
        CONSTRAINT ck_credenciales_totp_cifrado CHECK (totp_secret ~ '^v1:[A-Za-z0-9+/]{64}$'),
    ADD COLUMN mfa_enabled          BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN password_reset_token VARCHAR(64)
        CONSTRAINT ck_credenciales_reset_hash CHECK (password_reset_token ~ '^[0-9a-f]{64}$'),
    ADD COLUMN token_expires_at     TIMESTAMPTZ,
    ADD COLUMN totp_ultimo_paso     BIGINT
        CONSTRAINT ck_credenciales_totp_paso CHECK (totp_ultimo_paso > 0),
    ADD CONSTRAINT ck_credenciales_reset_par
        CHECK ((password_reset_token IS NULL) = (token_expires_at IS NULL));

COMMENT ON COLUMN kyc.credenciales.totp_secret IS
    'Secreto TOTP cifrado por la aplicación (AES-256-GCM, AAD = cliente). Nunca en claro.';
COMMENT ON COLUMN kyc.credenciales.mfa_enabled IS
    'Segundo factor activo: el inicio de sesión exige un código TOTP.';
COMMENT ON COLUMN kyc.credenciales.password_reset_token IS
    'SHA-256 (hex) del token de restablecimiento vigente; el token solo viaja en el correo.';
COMMENT ON COLUMN kyc.credenciales.token_expires_at IS
    'Vencimiento de password_reset_token.';
COMMENT ON COLUMN kyc.credenciales.totp_ultimo_paso IS
    'Último paso TOTP (unix / 30) aceptado: un código no se acepta dos veces.';

-- El restablecimiento busca por el hash; dos tokens vigentes nunca coinciden.
CREATE UNIQUE INDEX ux_credenciales_reset ON kyc.credenciales (password_reset_token)
    WHERE password_reset_token IS NOT NULL;

-- -----------------------------------------------------------------------------
-- 3. Bitácora de eventos de autenticación (inmutable)
-- -----------------------------------------------------------------------------
CREATE TABLE kyc.eventos_autenticacion (
    id             BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    cliente_id     UUID NOT NULL REFERENCES kyc.clientes (id),
    evento         TEXT NOT NULL CONSTRAINT ck_eventos_autenticacion_evento CHECK (evento IN (
                       'MFA_SECRETO_GENERADO',
                       'MFA_ACTIVADO',
                       'MFA_VERIFICADO',
                       'MFA_CODIGO_RECHAZADO',
                       'MFA_DESACTIVADO',
                       'RESTABLECIMIENTO_SOLICITADO',
                       'CONTRASENA_RESTABLECIDA')),
    ip             INET,
    -- Rol de la transacción: sc_auth (la aplicación) o el dueño del esquema (el DBA).
    registrado_por TEXT NOT NULL DEFAULT current_user,
    ocurrido_en    TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);
CREATE INDEX ix_eventos_autenticacion_cliente ON kyc.eventos_autenticacion (cliente_id, id);
CREATE TRIGGER trg_eventos_autenticacion_inmutable BEFORE UPDATE OR DELETE ON kyc.eventos_autenticacion
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_eventos_autenticacion_no_truncate BEFORE TRUNCATE ON kyc.eventos_autenticacion
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- -----------------------------------------------------------------------------
-- 4. Reglas del segundo factor
-- -----------------------------------------------------------------------------
-- Se ejecuta con los privilegios de quien actualiza (no SECURITY DEFINER): el
-- dueño del esquema (o un superusuario) es miembro del dueño de `kyc`; los roles
-- sc_* con que actúa la aplicación no lo son.
CREATE FUNCTION kyc.fn_proteger_mfa() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    dueno BOOLEAN := pg_has_role(current_user, (SELECT nspowner FROM pg_namespace WHERE nspname = 'kyc'), 'MEMBER');
BEGIN
    IF NEW.mfa_enabled AND NEW.totp_secret IS NULL THEN
        RAISE EXCEPTION 'SC_MFA_SIN_SECRETO: el segundo factor de % no puede quedar activo sin un secreto TOTP',
            NEW.cliente_id USING ERRCODE = 'check_violation';
    END IF;
    IF TG_OP = 'UPDATE' AND OLD.mfa_enabled AND NOT dueno
       AND (NOT NEW.mfa_enabled OR NEW.totp_secret IS DISTINCT FROM OLD.totp_secret) THEN
        RAISE EXCEPTION 'SC_MFA_SOLO_DUENO: solo el dueño del esquema desactiva o cambia el segundo factor activo de %',
            OLD.cliente_id USING ERRCODE = 'insufficient_privilege';
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_credenciales_mfa BEFORE INSERT OR UPDATE ON kyc.credenciales
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_proteger_mfa();

-- Desactivar el segundo factor (solo el dueño, ver arriba) siempre queda registrado.
CREATE FUNCTION kyc.fn_auditar_mfa_desactivado() RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    INSERT INTO kyc.eventos_autenticacion (cliente_id, evento) VALUES (NEW.cliente_id, 'MFA_DESACTIVADO');
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_credenciales_mfa_desactivado AFTER UPDATE OF mfa_enabled ON kyc.credenciales
    FOR EACH ROW WHEN (OLD.mfa_enabled AND NOT NEW.mfa_enabled)
    EXECUTE FUNCTION kyc.fn_auditar_mfa_desactivado();

-- -----------------------------------------------------------------------------
-- 5. Privilegios
-- -----------------------------------------------------------------------------
-- sc_kyc: las columnas que ya usaba (alta, inicio de sesión, validación de la
-- sesión) más mfa_enabled. Nunca totp_secret, password_reset_token,
-- token_expires_at ni totp_ultimo_paso.
REVOKE SELECT ON kyc.credenciales FROM sc_kyc;
GRANT SELECT (cliente_id, password_hash, intentos_fallidos, bloqueado_hasta, ultimo_acceso, creado_en,
              actualizado_en, rol, tokens_validos_desde, mfa_enabled)
    ON kyc.credenciales TO sc_kyc;

-- sc_auth: segundo factor, restablecimiento y revocación de sesiones.
GRANT USAGE ON SCHEMA nucleo, kyc TO sc_auth;
GRANT SELECT (id, email, en_lista_bloqueadas) ON kyc.clientes TO sc_auth;
GRANT SELECT (cliente_id, rol, password_hash, intentos_fallidos, bloqueado_hasta, tokens_validos_desde,
              mfa_enabled, totp_secret, totp_ultimo_paso, password_reset_token, token_expires_at)
    ON kyc.credenciales TO sc_auth;
GRANT UPDATE (password_hash, intentos_fallidos, bloqueado_hasta, ultimo_acceso, tokens_validos_desde,
              mfa_enabled, totp_secret, totp_ultimo_paso, password_reset_token, token_expires_at)
    ON kyc.credenciales TO sc_auth;
-- `registrado_por` queda fuera: siempre es el rol de la transacción.
GRANT INSERT (cliente_id, evento, ip) ON kyc.eventos_autenticacion TO sc_auth;
