-- =============================================================================
--  Synthetic Capital — 0027: llaves de acceso (passkeys) y códigos de recuperación
-- =============================================================================
--
--  Roadmap 19. Un cliente registra llaves de acceso (WebAuthn: Face ID, Touch
--  ID, Windows Hello, una llave de seguridad) para entrar sin contraseña y para
--  la verificación reforzada (0022), y genera códigos de recuperación de un
--  solo uso para cuando no tiene sus dispositivos. El personal interno sigue
--  con su segundo factor TOTP obligatorio: ni llaves ni códigos.
--
--  1. `kyc.usuarios_webauthn`: el identificador WebAuthn (`user.id`) de cada
--     cliente: 32 bytes aleatorios (ni su id ni su correo), el mismo para todas
--     sus llaves; los autenticadores lo guardan, así que nunca cambia.
--  2. `kyc.credenciales_webauthn`: cada llave: el id de la credencial, su clave
--     pública (COSE, tal como la entregó el autenticador) y su algoritmo
--     (ES256, EdDSA o RS256), el contador de firmas, el nombre que le dio el
--     cliente, los transportes, si puede respaldarse y si lo está, quién la
--     registró (la sesión de 0019, la IP y el dispositivo), sus usos (cuántos
--     y el último) y la revocación. La base de datos exige:
--     - que la registre (y la revoque) el propio cliente, desde una sesión
--       vigente con su ventana de verificación reforzada abierta; sin sesión,
--       solo el dueño del esquema la revoca (soporte: quien perdió todos sus
--       dispositivos y no tiene códigos de recuperación, como el
--       restablecimiento del segundo factor de 0010);
--     - solo clientes, y a lo más `kyc.maximo_passkeys()` (10) activas;
--     - que nada cambie salvo el uso (de uno en uno, con la fecha de la base
--       de datos, y el contador, que solo avanza: uno que no avanza delata un
--       autenticador clonado), el respaldo que reporta el autenticador al
--       usarla y, una vez, la revocación. Nunca se borra.
--  3. `kyc.desafios_webauthn`: el desafío (32 bytes aleatorios) de cada
--     registro, inicio de sesión o verificación reforzada: vale
--     `kyc.vigencia_desafio_webauthn()` (5 minutos) y se usa una sola vez. Los
--     vencidos hace más de un día se pueden borrar (limpieza).
--  4. Códigos de recuperación: `kyc.lotes_recuperacion` (cada generación, con
--     la sesión que tenía su ventana abierta, la IP y el dispositivo; la nueva
--     reemplaza a la anterior y sus códigos dejan de valer) y
--     `kyc.codigos_recuperacion` (el SHA-256 de cada código normalizado, nunca
--     el código; exactamente `kyc.codigos_por_lote()` (8) por lote, cada uno
--     usado a lo más una vez y solo mientras su lote siga vigente).
--  5. Bitácora (0019): PASSKEY_REGISTRADA y PASSKEY_REVOCADA (`detalle`: las
--     llaves activas que quedan), CODIGOS_RECUPERACION_GENERADOS (`detalle`: 8)
--     y CODIGO_RECUPERACION_USADO (`detalle`: los que quedan) los escriben los
--     triggers de cada tabla, con la sesión, la IP y el dispositivo; sc_auth no
--     puede escribirlos sin el cambio que registran, ni INICIO_SESION_PASSKEY
--     sin una llave usada en la misma transacción. PASSKEY_RECHAZADA y
--     CODIGO_RECUPERACION_RECHAZADO los escribe el backend.
--  6. Privilegios y RLS: sc_auth registra, usa y revoca; el cliente lee lo
--     suyo y el auditor lee todo, ambos sin claves, hashes ni desafíos.
-- =============================================================================

-- -----------------------------------------------------------------------------
-- 0. Parámetros y la sesión con su ventana
-- -----------------------------------------------------------------------------
CREATE FUNCTION kyc.maximo_passkeys() RETURNS INTEGER
LANGUAGE sql IMMUTABLE PARALLEL SAFE
AS $$ SELECT 10 $$;

CREATE FUNCTION kyc.vigencia_desafio_webauthn() RETURNS INTERVAL
LANGUAGE sql IMMUTABLE PARALLEL SAFE
AS $$ SELECT interval '5 minutes' $$;

CREATE FUNCTION kyc.codigos_por_lote() RETURNS INTEGER
LANGUAGE sql IMMUTABLE PARALLEL SAFE
AS $$ SELECT 8 $$;

COMMENT ON FUNCTION kyc.maximo_passkeys() IS 'Llaves de acceso activas por cliente, a lo más (0027).';
COMMENT ON FUNCTION kyc.vigencia_desafio_webauthn() IS 'Vigencia de un desafío WebAuthn (0027): 5 minutos.';
COMMENT ON FUNCTION kyc.codigos_por_lote() IS 'Códigos de recuperación por generación (0027): 8.';

-- Lo que piden registrar o revocar una llave y generar códigos: una sesión del
-- cliente, vigente, con su ventana de verificación reforzada abierta (0022).
-- SECURITY DEFINER: la lee quien no lee las sesiones.
CREATE FUNCTION kyc.sesion_con_ventana(p_cliente UUID, p_sesion UUID) RETURNS BOOLEAN
LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
    SELECT EXISTS (
        SELECT 1 FROM kyc.sesiones s
         WHERE s.id = p_sesion
           AND s.cliente_id = p_cliente
           AND s.revocada_en IS NULL
           AND s.expira_en > now()
           AND s.step_up_valido_hasta > now())
$$;

-- Llaves y códigos son de los clientes: el personal entra con su TOTP.
CREATE FUNCTION kyc.exigir_cliente_para_credencial(p_cliente UUID) RETURNS void
LANGUAGE plpgsql STABLE SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF (SELECT rol FROM kyc.credenciales WHERE cliente_id = p_cliente) IS DISTINCT FROM 'CLIENTE' THEN
        RAISE EXCEPTION 'SC_SOLO_CLIENTES: las llaves de acceso y los códigos de recuperación son de los clientes'
            USING ERRCODE = 'check_violation';
    END IF;
END
$$;

-- -----------------------------------------------------------------------------
-- 1. El identificador WebAuthn de cada cliente
-- -----------------------------------------------------------------------------
CREATE TABLE kyc.usuarios_webauthn (
    cliente_id    UUID PRIMARY KEY REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    identificador BYTEA NOT NULL
                  CONSTRAINT ux_usuarios_webauthn_identificador UNIQUE
                  CONSTRAINT ck_usuarios_webauthn_identificador CHECK (octet_length(identificador) = 32),
    creado_en     TIMESTAMPTZ NOT NULL DEFAULT now()
);

COMMENT ON TABLE kyc.usuarios_webauthn IS
    'El user.id WebAuthn de cada cliente (0027): 32 bytes aleatorios; los autenticadores lo guardan y nunca cambia.';

CREATE FUNCTION kyc.fn_nuevo_usuario_webauthn() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM kyc.exigir_cliente_para_credencial(NEW.cliente_id);
    NEW.creado_en := now();
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_usuarios_webauthn_nuevo BEFORE INSERT ON kyc.usuarios_webauthn
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_nuevo_usuario_webauthn();
CREATE TRIGGER trg_usuarios_webauthn_inmutable BEFORE UPDATE OR DELETE ON kyc.usuarios_webauthn
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_usuarios_webauthn_no_truncate BEFORE TRUNCATE ON kyc.usuarios_webauthn
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- -----------------------------------------------------------------------------
-- 2. Las llaves de acceso
-- -----------------------------------------------------------------------------
CREATE TABLE kyc.credenciales_webauthn (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cliente_id           UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    -- El id de la credencial (rawId): único en todo el sistema.
    credencial           BYTEA NOT NULL
                         CONSTRAINT ux_credenciales_webauthn_credencial UNIQUE
                         CONSTRAINT ck_credenciales_webauthn_credencial
                             CHECK (octet_length(credencial) BETWEEN 16 AND 1023),
    -- La clave pública COSE_Key tal como la entregó el autenticador.
    clave_publica        BYTEA NOT NULL
                         CONSTRAINT ck_credenciales_webauthn_clave CHECK (octet_length(clave_publica) BETWEEN 32 AND 1024),
    -- COSE: -7 ES256 (ECDSA P-256 con SHA-256), -8 EdDSA (Ed25519), -257 RS256.
    algoritmo            SMALLINT NOT NULL
                         CONSTRAINT ck_credenciales_webauthn_algoritmo CHECK (algoritmo IN (-7, -8, -257)),
    -- El contador de firmas del autenticador (32 bits sin signo); 0 si no cuenta.
    contador             BIGINT NOT NULL DEFAULT 0
                         CONSTRAINT ck_credenciales_webauthn_contador CHECK (contador BETWEEN 0 AND 4294967295),
    -- Cuántas veces se ha usado: cada uso suma uno (y la base de datos pone su
    -- fecha en `ultimo_uso_en`).
    usos                 BIGINT NOT NULL DEFAULT 0
                         CONSTRAINT ck_credenciales_webauthn_usos CHECK (usos >= 0),
    -- "iPhone de Ana", "YubiKey": lo escribe el cliente.
    nombre               TEXT NOT NULL
                         CONSTRAINT ck_credenciales_webauthn_nombre
                             CHECK (char_length(nombre) BETWEEN 1 AND 60 AND nombre = btrim(nombre)
                                    AND nombre !~ '[[:cntrl:]]'),
    transportes          TEXT[] NOT NULL DEFAULT '{}'
                         CONSTRAINT ck_credenciales_webauthn_transportes
                             CHECK (transportes <@ ARRAY['usb', 'nfc', 'ble', 'internal', 'hybrid', 'smart-card']::text[]
                                    AND cardinality(transportes) <= 6),
    -- El modelo del autenticador (ceros si no lo reveló).
    aaguid               UUID NOT NULL,
    -- Banderas BE y BS: puede respaldarse (una llave sincronizada) y lo está.
    respaldo_elegible    BOOLEAN NOT NULL,
    respaldada           BOOLEAN NOT NULL,
    -- Quién la registró: la sesión con su ventana abierta, la IP y el dispositivo.
    sesion_id            UUID NOT NULL REFERENCES kyc.sesiones (id) ON DELETE RESTRICT,
    ip                   INET,
    dispositivo          TEXT NOT NULL
                         CONSTRAINT ck_credenciales_webauthn_dispositivo CHECK (char_length(dispositivo) BETWEEN 1 AND 80),
    creada_en            TIMESTAMPTZ NOT NULL DEFAULT now(),
    ultimo_uso_en        TIMESTAMPTZ,
    revocada_en          TIMESTAMPTZ,
    revocada_sesion_id   UUID REFERENCES kyc.sesiones (id) ON DELETE RESTRICT,
    revocada_ip          INET,
    revocada_dispositivo TEXT
                         CONSTRAINT ck_credenciales_webauthn_revocada_dispositivo
                             CHECK (char_length(revocada_dispositivo) BETWEEN 1 AND 80),
    CONSTRAINT ck_credenciales_webauthn_respaldo CHECK (respaldo_elegible OR NOT respaldada),
    CONSTRAINT ck_credenciales_webauthn_uso CHECK ((usos = 0) = (ultimo_uso_en IS NULL) AND ultimo_uso_en >= creada_en),
    -- Revocada desde una sesión (con su dispositivo y su IP) o, sin sesión, por
    -- el dueño del esquema (soporte).
    CONSTRAINT ck_credenciales_webauthn_revocacion CHECK (
        (revocada_en IS NOT NULL OR revocada_sesion_id IS NULL)
        AND (revocada_sesion_id IS NULL) = (revocada_dispositivo IS NULL)
        AND (revocada_sesion_id IS NOT NULL OR revocada_ip IS NULL)
        AND revocada_en >= creada_en)
);
CREATE INDEX ix_credenciales_webauthn_cliente ON kyc.credenciales_webauthn (cliente_id) WHERE revocada_en IS NULL;

COMMENT ON TABLE kyc.credenciales_webauthn IS
    'Llaves de acceso WebAuthn de los clientes (0027): nunca se borran; solo cambian su uso y, una vez, la revocación.';

-- Nace del propio cliente, en una sesión con su ventana abierta, sin usar ni
-- revocar, con la fecha de la base de datos.
CREATE FUNCTION kyc.fn_registrar_passkey() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM kyc.exigir_cliente_para_credencial(NEW.cliente_id);
    IF NOT kyc.sesion_con_ventana(NEW.cliente_id, NEW.sesion_id) THEN
        RAISE EXCEPTION 'SC_STEP_UP_REQUERIDO: registrar una llave de acceso pide verificar de nuevo tu identidad en esta sesión'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF NEW.usos <> 0 OR NEW.ultimo_uso_en IS NOT NULL OR NEW.revocada_en IS NOT NULL OR NEW.revocada_sesion_id IS NOT NULL
       OR NEW.revocada_ip IS NOT NULL OR NEW.revocada_dispositivo IS NOT NULL THEN
        RAISE EXCEPTION 'SC_PASSKEY_INCOHERENTE: una llave de acceso nace sin usar ni revocar'
            USING ERRCODE = 'check_violation';
    END IF;
    -- De una en una por cliente: el máximo no se cruza.
    PERFORM pg_advisory_xact_lock(hashtextextended('kyc.passkeys:' || NEW.cliente_id::text, 0));
    IF (SELECT count(*) FROM kyc.credenciales_webauthn
         WHERE cliente_id = NEW.cliente_id AND revocada_en IS NULL) >= kyc.maximo_passkeys() THEN
        RAISE EXCEPTION 'SC_LIMITE_PASSKEYS: a lo más % llaves de acceso activas; revoca una para registrar otra',
            kyc.maximo_passkeys()
            USING ERRCODE = 'check_violation';
    END IF;
    NEW.creada_en := now();
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_credenciales_webauthn_a_registrar BEFORE INSERT ON kyc.credenciales_webauthn
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_registrar_passkey();

-- Solo cambian su uso (`usos` suma uno; el contador avanza; el respaldo, el que
-- reporta el autenticador) y, una vez, su revocación (desde una sesión con su
-- ventana o, sin sesión, por el dueño del esquema). Un uso se marca con
-- `usos`, no con la fecha: dos usos en una misma transacción tendrían la misma
-- `now()`. Corre con los privilegios de quien actualiza (como 0010): el dueño
-- del esquema es miembro del dueño de `kyc`; los roles sc_* no lo son.
CREATE FUNCTION kyc.fn_proteger_passkey() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_uso BOOLEAN := NEW.usos IS DISTINCT FROM OLD.usos;
BEGIN
    IF (NEW.id, NEW.cliente_id, NEW.credencial, NEW.clave_publica, NEW.algoritmo, NEW.nombre, NEW.transportes,
        NEW.aaguid, NEW.respaldo_elegible, NEW.sesion_id, NEW.ip, NEW.dispositivo, NEW.creada_en)
       IS DISTINCT FROM
       (OLD.id, OLD.cliente_id, OLD.credencial, OLD.clave_publica, OLD.algoritmo, OLD.nombre, OLD.transportes,
        OLD.aaguid, OLD.respaldo_elegible, OLD.sesion_id, OLD.ip, OLD.dispositivo, OLD.creada_en) THEN
        RAISE EXCEPTION 'SC_PASSKEY_INMUTABLE: de la llave % solo cambian su uso y su revocación', OLD.id
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF OLD.revocada_en IS NOT NULL THEN
        RAISE EXCEPTION 'SC_PASSKEY_REVOCADA: la llave % está revocada', OLD.id
            USING ERRCODE = 'restrict_violation';
    END IF;
    IF NEW.revocada_en IS NOT NULL THEN
        -- Revocar no es usar.
        IF v_uso OR NEW.contador <> OLD.contador OR NEW.respaldada <> OLD.respaldada
           OR NEW.ultimo_uso_en IS DISTINCT FROM OLD.ultimo_uso_en THEN
            RAISE EXCEPTION 'SC_PASSKEY_INCOHERENTE: revocar la llave % no la usa', OLD.id
                USING ERRCODE = 'check_violation';
        END IF;
        IF NEW.revocada_sesion_id IS NULL THEN
            IF NOT pg_has_role(current_user, (SELECT nspowner FROM pg_namespace WHERE nspname = 'kyc'), 'MEMBER') THEN
                RAISE EXCEPTION 'SC_STEP_UP_REQUERIDO: revocar una llave de acceso pide verificar de nuevo tu identidad en esta sesión'
                    USING ERRCODE = 'insufficient_privilege';
            END IF;
        ELSIF NOT kyc.sesion_con_ventana(OLD.cliente_id, NEW.revocada_sesion_id) THEN
            RAISE EXCEPTION 'SC_STEP_UP_REQUERIDO: revocar una llave de acceso pide verificar de nuevo tu identidad en esta sesión'
                USING ERRCODE = 'insufficient_privilege';
        END IF;
        NEW.revocada_en := now();
        RETURN NEW;
    END IF;
    IF (NEW.revocada_sesion_id, NEW.revocada_ip, NEW.revocada_dispositivo) IS DISTINCT FROM (NULL::uuid, NULL::inet, NULL::text) THEN
        RAISE EXCEPTION 'SC_PASSKEY_INCOHERENTE: los datos de la revocación van con ella'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NOT v_uso THEN
        IF NEW.contador <> OLD.contador OR NEW.respaldada <> OLD.respaldada
           OR NEW.ultimo_uso_en IS DISTINCT FROM OLD.ultimo_uso_en THEN
            RAISE EXCEPTION 'SC_PASSKEY_INMUTABLE: el contador, el respaldo y el último uso de la llave % cambian solo al usarla',
                OLD.id
                USING ERRCODE = 'restrict_violation';
        END IF;
        RETURN NEW;
    END IF;
    IF NEW.usos <> OLD.usos + 1 THEN
        RAISE EXCEPTION 'SC_PASSKEY_INCOHERENTE: los usos de la llave % se cuentan de uno en uno', OLD.id
            USING ERRCODE = 'check_violation';
    END IF;
    -- Un uso: el contador avanza (si el autenticador cuenta). Uno que no avanza
    -- puede ser una copia de la llave.
    IF NOT (NEW.contador > OLD.contador OR (NEW.contador = 0 AND OLD.contador = 0)) THEN
        RAISE EXCEPTION 'SC_PASSKEY_CONTADOR: el contador de la llave % no avanza (% -> %): pudo clonarse',
            OLD.id, OLD.contador, NEW.contador
            USING ERRCODE = 'check_violation';
    END IF;
    NEW.ultimo_uso_en := now();
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_credenciales_webauthn_b_proteger BEFORE UPDATE ON kyc.credenciales_webauthn
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_proteger_passkey();
CREATE TRIGGER trg_credenciales_webauthn_no_borrar BEFORE DELETE ON kyc.credenciales_webauthn
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_credenciales_webauthn_no_truncate BEFORE TRUNCATE ON kyc.credenciales_webauthn
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- Corre con el rol de quien registra o revoca (sc_auth), que queda en
-- `registrado_por`; si el cambio no llega al COMMIT, su evento tampoco.
CREATE FUNCTION kyc.fn_auditar_passkey() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_activas BIGINT;
BEGIN
    SELECT count(*) INTO v_activas FROM kyc.credenciales_webauthn
     WHERE cliente_id = NEW.cliente_id AND revocada_en IS NULL;
    IF TG_OP = 'INSERT' THEN
        INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, sesion_id, dispositivo, detalle)
        VALUES (NEW.cliente_id, 'PASSKEY_REGISTRADA', NEW.ip, NEW.sesion_id, NEW.dispositivo, v_activas::text);
    ELSIF NEW.revocada_en IS NOT NULL AND OLD.revocada_en IS NULL THEN
        INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, sesion_id, dispositivo, detalle)
        VALUES (NEW.cliente_id, 'PASSKEY_REVOCADA', NEW.revocada_ip, NEW.revocada_sesion_id, NEW.revocada_dispositivo,
                v_activas::text);
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_credenciales_webauthn_c_auditar AFTER INSERT OR UPDATE OF revocada_en ON kyc.credenciales_webauthn
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_auditar_passkey();

-- -----------------------------------------------------------------------------
-- 3. Los desafíos: 5 minutos, una sola vez
-- -----------------------------------------------------------------------------
CREATE TABLE kyc.desafios_webauthn (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    proposito  TEXT NOT NULL
               CONSTRAINT ck_desafios_webauthn_proposito CHECK (proposito IN ('REGISTRO', 'INICIO', 'STEP_UP')),
    desafio    BYTEA NOT NULL
               CONSTRAINT ux_desafios_webauthn_desafio UNIQUE
               CONSTRAINT ck_desafios_webauthn_desafio CHECK (octet_length(desafio) = 32),
    -- Un registro y una verificación reforzada son de una sesión; un inicio de
    -- sesión, de nadie todavía (la llave dirá de quién).
    cliente_id UUID REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    sesion_id  UUID REFERENCES kyc.sesiones (id) ON DELETE RESTRICT,
    creado_en  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expira_en  TIMESTAMPTZ NOT NULL DEFAULT now(),
    usado_en   TIMESTAMPTZ,
    CONSTRAINT ck_desafios_webauthn_dueno CHECK (
        (proposito = 'INICIO') = (cliente_id IS NULL) AND (cliente_id IS NULL) = (sesion_id IS NULL)),
    CONSTRAINT ck_desafios_webauthn_uso CHECK (usado_en IS NULL OR usado_en BETWEEN creado_en AND expira_en)
);
CREATE INDEX ix_desafios_webauthn_expira ON kyc.desafios_webauthn (expira_en);

COMMENT ON TABLE kyc.desafios_webauthn IS
    'Desafíos WebAuthn (0027): vigentes 5 minutos, de un solo uso; los vencidos hace más de un día se pueden borrar.';

CREATE FUNCTION kyc.fn_desafio_webauthn() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.usado_en IS NOT NULL THEN
            RAISE EXCEPTION 'SC_DESAFIO_INCOHERENTE: un desafío nace sin usar' USING ERRCODE = 'check_violation';
        END IF;
        IF NEW.cliente_id IS NOT NULL AND NOT EXISTS (
            SELECT 1 FROM kyc.sesiones s
             WHERE s.id = NEW.sesion_id AND s.cliente_id = NEW.cliente_id
               AND s.revocada_en IS NULL AND s.expira_en > now()) THEN
            RAISE EXCEPTION 'SC_DESAFIO_INCOHERENTE: el desafío es de una sesión vigente del cliente'
                USING ERRCODE = 'check_violation';
        END IF;
        NEW.creado_en := now();
        NEW.expira_en := now() + kyc.vigencia_desafio_webauthn();
        RETURN NEW;
    END IF;
    IF TG_OP = 'DELETE' THEN
        IF OLD.expira_en > now() - interval '1 day' THEN
            RAISE EXCEPTION 'SC_DESAFIO_VIGENTE: solo se borran los desafíos vencidos hace más de un día'
                USING ERRCODE = 'restrict_violation';
        END IF;
        RETURN OLD;
    END IF;
    IF (NEW.id, NEW.proposito, NEW.desafio, NEW.cliente_id, NEW.sesion_id, NEW.creado_en, NEW.expira_en)
       IS DISTINCT FROM (OLD.id, OLD.proposito, OLD.desafio, OLD.cliente_id, OLD.sesion_id, OLD.creado_en, OLD.expira_en)
       OR NEW.usado_en IS NULL THEN
        RAISE EXCEPTION 'SC_DESAFIO_INMUTABLE: de un desafío solo se marca su uso' USING ERRCODE = 'restrict_violation';
    END IF;
    IF OLD.usado_en IS NOT NULL THEN
        RAISE EXCEPTION 'SC_DESAFIO_USADO: el desafío % ya se usó', OLD.id USING ERRCODE = 'check_violation';
    END IF;
    IF OLD.expira_en <= now() THEN
        RAISE EXCEPTION 'SC_DESAFIO_VENCIDO: el desafío % venció', OLD.id USING ERRCODE = 'check_violation';
    END IF;
    NEW.usado_en := now();
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_desafios_webauthn BEFORE INSERT OR UPDATE OR DELETE ON kyc.desafios_webauthn
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_desafio_webauthn();
CREATE TRIGGER trg_desafios_webauthn_no_truncate BEFORE TRUNCATE ON kyc.desafios_webauthn
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- -----------------------------------------------------------------------------
-- 4. Códigos de recuperación
-- -----------------------------------------------------------------------------
CREATE TABLE kyc.lotes_recuperacion (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cliente_id     UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    -- Quién los generó: la sesión con su ventana abierta, la IP y el dispositivo.
    sesion_id      UUID NOT NULL REFERENCES kyc.sesiones (id) ON DELETE RESTRICT,
    ip             INET,
    dispositivo    TEXT NOT NULL
                   CONSTRAINT ck_lotes_recuperacion_dispositivo CHECK (char_length(dispositivo) BETWEEN 1 AND 80),
    creado_en      TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Una generación nueva reemplaza a esta: sus códigos dejan de valer.
    reemplazado_en TIMESTAMPTZ,
    CONSTRAINT ck_lotes_recuperacion_reemplazo CHECK (reemplazado_en >= creado_en)
);
-- Uno vigente por cliente.
CREATE UNIQUE INDEX ux_lotes_recuperacion_vigente ON kyc.lotes_recuperacion (cliente_id) WHERE reemplazado_en IS NULL;

CREATE TABLE kyc.codigos_recuperacion (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    lote_id           UUID NOT NULL REFERENCES kyc.lotes_recuperacion (id) ON DELETE RESTRICT,
    cliente_id        UUID NOT NULL REFERENCES kyc.clientes (id) ON DELETE RESTRICT,
    -- SHA-256 del código normalizado (16 símbolos base32, 80 bits): nunca el código.
    hash              BYTEA NOT NULL CONSTRAINT ck_codigos_recuperacion_hash CHECK (octet_length(hash) = 32),
    creado_en         TIMESTAMPTZ NOT NULL DEFAULT now(),
    usado_en          TIMESTAMPTZ,
    -- La sesión que abrió (inicio de sesión) o verificó (verificación reforzada).
    usado_sesion_id   UUID REFERENCES kyc.sesiones (id) ON DELETE RESTRICT,
    usado_ip          INET,
    usado_dispositivo TEXT
                      CONSTRAINT ck_codigos_recuperacion_dispositivo CHECK (char_length(usado_dispositivo) BETWEEN 1 AND 80),
    CONSTRAINT ux_codigos_recuperacion_hash UNIQUE (cliente_id, hash),
    CONSTRAINT ck_codigos_recuperacion_uso CHECK (
        (usado_en IS NULL) = (usado_sesion_id IS NULL)
        AND (usado_en IS NULL) = (usado_dispositivo IS NULL)
        AND (usado_en IS NOT NULL OR usado_ip IS NULL)
        AND usado_en >= creado_en)
);
CREATE INDEX ix_codigos_recuperacion_lote ON kyc.codigos_recuperacion (lote_id);

COMMENT ON TABLE kyc.lotes_recuperacion IS
    'Cada generación de códigos de recuperación (0027): la vigente es la última; nunca se borran.';
COMMENT ON TABLE kyc.codigos_recuperacion IS
    'Códigos de recuperación (0027): el SHA-256 de cada uno, 8 por lote, de un solo uso mientras su lote siga vigente.';

-- Una generación nace del propio cliente, en una sesión con su ventana
-- abierta, y reemplaza a la vigente.
CREATE FUNCTION kyc.fn_generar_lote_recuperacion() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    PERFORM kyc.exigir_cliente_para_credencial(NEW.cliente_id);
    IF NOT kyc.sesion_con_ventana(NEW.cliente_id, NEW.sesion_id) THEN
        RAISE EXCEPTION 'SC_STEP_UP_REQUERIDO: generar códigos de recuperación pide verificar de nuevo tu identidad en esta sesión'
            USING ERRCODE = 'insufficient_privilege';
    END IF;
    IF NEW.reemplazado_en IS NOT NULL THEN
        RAISE EXCEPTION 'SC_LOTE_INCOHERENTE: una generación nace vigente' USING ERRCODE = 'check_violation';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended('kyc.recuperacion:' || NEW.cliente_id::text, 0));
    UPDATE kyc.lotes_recuperacion SET reemplazado_en = now()
     WHERE cliente_id = NEW.cliente_id AND reemplazado_en IS NULL;
    NEW.creado_en := now();
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_lotes_recuperacion_a_generar BEFORE INSERT ON kyc.lotes_recuperacion
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_generar_lote_recuperacion();

-- Solo cambia, una vez, el reemplazo (con la fecha de la base de datos).
CREATE FUNCTION kyc.fn_proteger_lote_recuperacion() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF (NEW.id, NEW.cliente_id, NEW.sesion_id, NEW.ip, NEW.dispositivo, NEW.creado_en)
       IS DISTINCT FROM (OLD.id, OLD.cliente_id, OLD.sesion_id, OLD.ip, OLD.dispositivo, OLD.creado_en)
       OR NEW.reemplazado_en IS NULL OR OLD.reemplazado_en IS NOT NULL THEN
        RAISE EXCEPTION 'SC_LOTE_INMUTABLE: de una generación solo se marca, una vez, su reemplazo'
            USING ERRCODE = 'restrict_violation';
    END IF;
    NEW.reemplazado_en := now();
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_lotes_recuperacion_b_proteger BEFORE UPDATE ON kyc.lotes_recuperacion
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_proteger_lote_recuperacion();
CREATE TRIGGER trg_lotes_recuperacion_no_borrar BEFORE DELETE ON kyc.lotes_recuperacion
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_lotes_recuperacion_no_truncate BEFORE TRUNCATE ON kyc.lotes_recuperacion
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- Al confirmar, cada generación tiene exactamente sus 8 códigos.
CREATE FUNCTION kyc.fn_lote_recuperacion_completo() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_codigos BIGINT;
BEGIN
    SELECT count(*) INTO v_codigos FROM kyc.codigos_recuperacion WHERE lote_id = NEW.id;
    IF v_codigos <> kyc.codigos_por_lote() THEN
        RAISE EXCEPTION 'SC_LOTE_INCOMPLETO: la generación % tiene % códigos (son %)', NEW.id, v_codigos, kyc.codigos_por_lote()
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END
$$;
CREATE CONSTRAINT TRIGGER trg_lotes_recuperacion_completo AFTER INSERT ON kyc.lotes_recuperacion
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_lote_recuperacion_completo();

-- Un código entra en la generación de su cliente, la vigente, en la misma
-- transacción que la creó, hasta completar 8; nace sin usar.
CREATE FUNCTION kyc.fn_nuevo_codigo_recuperacion() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM kyc.lotes_recuperacion l
         WHERE l.id = NEW.lote_id AND l.cliente_id = NEW.cliente_id
           AND l.reemplazado_en IS NULL AND l.creado_en = now()) THEN
        RAISE EXCEPTION 'SC_LOTE_CERRADO: los códigos entran solo en la generación que se está creando'
            USING ERRCODE = 'check_violation';
    END IF;
    IF (SELECT count(*) FROM kyc.codigos_recuperacion WHERE lote_id = NEW.lote_id) >= kyc.codigos_por_lote() THEN
        RAISE EXCEPTION 'SC_LOTE_CERRADO: una generación tiene % códigos', kyc.codigos_por_lote()
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.usado_en IS NOT NULL OR NEW.usado_sesion_id IS NOT NULL OR NEW.usado_ip IS NOT NULL
       OR NEW.usado_dispositivo IS NOT NULL THEN
        RAISE EXCEPTION 'SC_CODIGO_INCOHERENTE: un código nace sin usar' USING ERRCODE = 'check_violation';
    END IF;
    NEW.creado_en := now();
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_codigos_recuperacion_a_nuevo BEFORE INSERT ON kyc.codigos_recuperacion
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_nuevo_codigo_recuperacion();

-- Solo se usa, una vez, y solo mientras su generación siga vigente.
CREATE FUNCTION kyc.fn_usar_codigo_recuperacion() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    IF (NEW.id, NEW.lote_id, NEW.cliente_id, NEW.hash, NEW.creado_en)
       IS DISTINCT FROM (OLD.id, OLD.lote_id, OLD.cliente_id, OLD.hash, OLD.creado_en)
       OR NEW.usado_en IS NULL THEN
        RAISE EXCEPTION 'SC_CODIGO_INMUTABLE: de un código solo se marca su uso' USING ERRCODE = 'restrict_violation';
    END IF;
    IF OLD.usado_en IS NOT NULL THEN
        RAISE EXCEPTION 'SC_CODIGO_USADO: el código ya se usó' USING ERRCODE = 'check_violation';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM kyc.lotes_recuperacion l WHERE l.id = OLD.lote_id AND l.reemplazado_en IS NULL) THEN
        RAISE EXCEPTION 'SC_CODIGO_REEMPLAZADO: una generación nueva reemplazó a la de este código'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM kyc.sesiones s
         WHERE s.id = NEW.usado_sesion_id AND s.cliente_id = OLD.cliente_id
           AND s.revocada_en IS NULL AND s.expira_en > now()) THEN
        RAISE EXCEPTION 'SC_CODIGO_INCOHERENTE: un código se usa en una sesión vigente de su cliente'
            USING ERRCODE = 'check_violation';
    END IF;
    NEW.usado_en := now();
    RETURN NEW;
END
$$;
CREATE TRIGGER trg_codigos_recuperacion_b_usar BEFORE UPDATE ON kyc.codigos_recuperacion
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_usar_codigo_recuperacion();
CREATE TRIGGER trg_codigos_recuperacion_no_borrar BEFORE DELETE ON kyc.codigos_recuperacion
    FOR EACH ROW EXECUTE FUNCTION nucleo.fn_ledger_inmutable();
CREATE TRIGGER trg_codigos_recuperacion_no_truncate BEFORE TRUNCATE ON kyc.codigos_recuperacion
    FOR EACH STATEMENT EXECUTE FUNCTION nucleo.fn_ledger_inmutable();

-- La bitácora: cada generación (8) y cada uso (los que quedan), con la sesión,
-- la IP y el dispositivo; con el rol de quien actúa.
CREATE FUNCTION kyc.fn_auditar_recuperacion() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_quedan BIGINT;
BEGIN
    IF TG_TABLE_NAME = 'lotes_recuperacion' THEN
        INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, sesion_id, dispositivo, detalle)
        VALUES (NEW.cliente_id, 'CODIGOS_RECUPERACION_GENERADOS', NEW.ip, NEW.sesion_id, NEW.dispositivo,
                kyc.codigos_por_lote()::text);
    ELSIF NEW.usado_en IS NOT NULL AND OLD.usado_en IS NULL THEN
        SELECT count(*) INTO v_quedan FROM kyc.codigos_recuperacion WHERE lote_id = NEW.lote_id AND usado_en IS NULL;
        INSERT INTO kyc.eventos_autenticacion (cliente_id, evento, ip, sesion_id, dispositivo, detalle)
        VALUES (NEW.cliente_id, 'CODIGO_RECUPERACION_USADO', NEW.usado_ip, NEW.usado_sesion_id, NEW.usado_dispositivo,
                v_quedan::text);
    END IF;
    RETURN NULL;
END
$$;
CREATE TRIGGER trg_lotes_recuperacion_c_auditar AFTER INSERT ON kyc.lotes_recuperacion
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_auditar_recuperacion();
CREATE TRIGGER trg_codigos_recuperacion_c_auditar AFTER UPDATE OF usado_en ON kyc.codigos_recuperacion
    FOR EACH ROW EXECUTE FUNCTION kyc.fn_auditar_recuperacion();

-- -----------------------------------------------------------------------------
-- 5. Bitácora de seguridad
-- -----------------------------------------------------------------------------
-- La bitácora es inmutable (filas); cambiar su catálogo es DDL y no la toca.
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
    'STEP_UP_FALLIDO',
    -- 0025: `detalle` es el folio de la aclaración
    'ACLARACION_SPEI_ABIERTA',
    'ACLARACION_SPEI_EN_REVISION',
    'ACLARACION_SPEI_RESUELTA',
    'ACLARACION_SPEI_RECHAZADA',
    -- 0027: `detalle` son las llaves activas, los códigos generados o los que quedan
    'PASSKEY_REGISTRADA',
    'PASSKEY_REVOCADA',
    'PASSKEY_RECHAZADA',
    'INICIO_SESION_PASSKEY',
    'CODIGOS_RECUPERACION_GENERADOS',
    'CODIGO_RECUPERACION_USADO',
    'CODIGO_RECUPERACION_RECHAZADO'));

-- sc_auth no escribe estos eventos sin el cambio que registran, hecho en la
-- misma transacción (la fecha de la base de datos es la de la transacción):
-- una llave registrada o revocada desde esa sesión, una generación de esa
-- sesión, un código usado en ella, una llave usada al iniciar sesión. Los
-- demás eventos de sc_auth no cambian.
CREATE POLICY credenciales_con_su_cambio ON kyc.eventos_autenticacion AS RESTRICTIVE FOR INSERT TO sc_auth
    WITH CHECK (CASE evento
        WHEN 'PASSKEY_REGISTRADA' THEN EXISTS (
            SELECT 1 FROM kyc.credenciales_webauthn p
             WHERE p.cliente_id = eventos_autenticacion.cliente_id AND p.creada_en = now()
               AND p.sesion_id = eventos_autenticacion.sesion_id)
        WHEN 'PASSKEY_REVOCADA' THEN EXISTS (
            SELECT 1 FROM kyc.credenciales_webauthn p
             WHERE p.cliente_id = eventos_autenticacion.cliente_id AND p.revocada_en = now()
               AND p.revocada_sesion_id = eventos_autenticacion.sesion_id)
        WHEN 'INICIO_SESION_PASSKEY' THEN EXISTS (
            SELECT 1 FROM kyc.credenciales_webauthn p
             WHERE p.cliente_id = eventos_autenticacion.cliente_id AND p.ultimo_uso_en = now()
               AND p.revocada_en IS NULL)
        WHEN 'CODIGOS_RECUPERACION_GENERADOS' THEN EXISTS (
            SELECT 1 FROM kyc.lotes_recuperacion l
             WHERE l.cliente_id = eventos_autenticacion.cliente_id AND l.creado_en = now()
               AND l.sesion_id = eventos_autenticacion.sesion_id)
        WHEN 'CODIGO_RECUPERACION_USADO' THEN EXISTS (
            SELECT 1 FROM kyc.codigos_recuperacion c
             WHERE c.cliente_id = eventos_autenticacion.cliente_id AND c.usado_en = now()
               AND c.usado_sesion_id = eventos_autenticacion.sesion_id)
        ELSE true
    END);

-- -----------------------------------------------------------------------------
-- 6. Privilegios y RLS
-- -----------------------------------------------------------------------------
REVOKE ALL ON kyc.usuarios_webauthn, kyc.credenciales_webauthn, kyc.desafios_webauthn, kyc.lotes_recuperacion,
              kyc.codigos_recuperacion FROM PUBLIC;
REVOKE ALL ON FUNCTION kyc.sesion_con_ventana(UUID, UUID) FROM PUBLIC;
REVOKE ALL ON FUNCTION kyc.exigir_cliente_para_credencial(UUID) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION kyc.sesion_con_ventana(UUID, UUID) TO sc_auth;
GRANT EXECUTE ON FUNCTION kyc.exigir_cliente_para_credencial(UUID) TO sc_auth;

-- sc_auth: registra, usa y revoca llaves; crea y usa desafíos (y borra los
-- vencidos); genera y usa códigos.
GRANT SELECT ON kyc.usuarios_webauthn TO sc_auth;
GRANT INSERT (cliente_id, identificador) ON kyc.usuarios_webauthn TO sc_auth;
GRANT SELECT ON kyc.credenciales_webauthn TO sc_auth;
GRANT INSERT (id, cliente_id, credencial, clave_publica, algoritmo, contador, nombre, transportes, aaguid,
              respaldo_elegible, respaldada, sesion_id, ip, dispositivo) ON kyc.credenciales_webauthn TO sc_auth;
GRANT UPDATE (usos, contador, respaldada, revocada_en, revocada_sesion_id, revocada_ip, revocada_dispositivo)
    ON kyc.credenciales_webauthn TO sc_auth;
GRANT SELECT, DELETE ON kyc.desafios_webauthn TO sc_auth;
GRANT INSERT (id, proposito, desafio, cliente_id, sesion_id) ON kyc.desafios_webauthn TO sc_auth;
GRANT UPDATE (usado_en) ON kyc.desafios_webauthn TO sc_auth;
GRANT SELECT ON kyc.lotes_recuperacion TO sc_auth;
GRANT INSERT (id, cliente_id, sesion_id, ip, dispositivo) ON kyc.lotes_recuperacion TO sc_auth;
GRANT UPDATE (reemplazado_en) ON kyc.lotes_recuperacion TO sc_auth;
GRANT SELECT ON kyc.codigos_recuperacion TO sc_auth;
GRANT INSERT (id, lote_id, cliente_id, hash) ON kyc.codigos_recuperacion TO sc_auth;
GRANT UPDATE (usado_en, usado_sesion_id, usado_ip, usado_dispositivo) ON kyc.codigos_recuperacion TO sc_auth;

-- El cliente lee lo suyo y el auditor todo: nombres, fechas y usos; nunca las
-- claves, los hashes, los identificadores ni los desafíos.
GRANT SELECT (id, cliente_id, nombre, transportes, respaldo_elegible, respaldada, dispositivo, creada_en, usos,
              ultimo_uso_en, revocada_en) ON kyc.credenciales_webauthn TO sc_cliente, sc_auditor;
GRANT SELECT (id, cliente_id, creado_en, reemplazado_en) ON kyc.lotes_recuperacion TO sc_cliente, sc_auditor;
GRANT SELECT (id, lote_id, cliente_id, creado_en, usado_en) ON kyc.codigos_recuperacion TO sc_cliente, sc_auditor;

-- Como en 0011 y 0019: una política abierta para los roles con privilegios y
-- una RESTRICTIVE para sc_cliente, que solo ve las filas del cliente actual.
ALTER TABLE kyc.usuarios_webauthn ENABLE ROW LEVEL SECURITY;
ALTER TABLE kyc.credenciales_webauthn ENABLE ROW LEVEL SECURITY;
ALTER TABLE kyc.desafios_webauthn ENABLE ROW LEVEL SECURITY;
ALTER TABLE kyc.lotes_recuperacion ENABLE ROW LEVEL SECURITY;
ALTER TABLE kyc.codigos_recuperacion ENABLE ROW LEVEL SECURITY;
CREATE POLICY por_privilegios ON kyc.usuarios_webauthn USING (true) WITH CHECK (true);
CREATE POLICY por_privilegios ON kyc.credenciales_webauthn USING (true) WITH CHECK (true);
CREATE POLICY por_privilegios ON kyc.desafios_webauthn USING (true) WITH CHECK (true);
CREATE POLICY por_privilegios ON kyc.lotes_recuperacion USING (true) WITH CHECK (true);
CREATE POLICY por_privilegios ON kyc.codigos_recuperacion USING (true) WITH CHECK (true);
CREATE POLICY solo_del_cliente ON kyc.credenciales_webauthn AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual()) WITH CHECK (cliente_id = nucleo.cliente_actual());
CREATE POLICY solo_del_cliente ON kyc.lotes_recuperacion AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual()) WITH CHECK (cliente_id = nucleo.cliente_actual());
CREATE POLICY solo_del_cliente ON kyc.codigos_recuperacion AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual()) WITH CHECK (cliente_id = nucleo.cliente_actual());
