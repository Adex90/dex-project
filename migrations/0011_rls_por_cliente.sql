-- =============================================================================
--  0011 — Row-Level Security por cliente: un cliente solo ve sus propias filas
-- =============================================================================
--
-- Las lecturas que el backend hace a nombre de un cliente (su perfil, su
-- cuenta, sus movimientos, su posición en la SOFOM, sus SPEI) corren con el
-- rol `sc_cliente` y el id del cliente en la variable de transacción
-- `sc.cliente_actual` (db::begin_lectura_cliente). Las políticas RESTRICTIVE
-- de esta migración, solo para sc_cliente, dejan ver una fila únicamente si es
-- de ese cliente, pida lo que pida la consulta:
--
--   tabla                   la fila es del cliente si...
--   kyc.clientes            id es el cliente
--   ifpe.cuentas            cliente_id es el cliente
--   ifpe.transacciones      la cuenta de origen o la de destino es suya
--   ifpe.spei_outbound      cliente_id es el cliente
--   ifpe.spei_inbound       la cuenta abonada es suya
--   sofom.cuentas           cliente_id es el cliente
--   sofom.contratos_mutuo   cliente_id es el cliente
--   sofom.transacciones     la cuenta SOFOM es suya
--
-- Falla cerrado: sin la variable (o vacía), sc_cliente no ve ninguna fila; con
-- un valor que no es un UUID, la consulta falla.
--
-- Solo lectura. sc_cliente tiene SELECT y nada más: un cliente nunca escribe
-- filas directamente. El dinero se mueve con el ledger (sc_ifpe, sc_spei,
-- sc_puente...), que valida saldos, reserva y PLD, y el expediente con sc_kyc.
-- Nadie borra movimientos, cuentas ni expedientes: el ledger es inmutable y
-- los registros se conservan por ley. Aun así las políticas son FOR ALL
-- (USING y WITH CHECK): si algún día se concede escritura a sc_cliente, queda
-- limitada a sus filas y no puede pasar una fila a otro cliente.
--
-- Los demás roles no cambian. Las tablas que aún no tenían RLS reciben una
-- política permisiva para todos (los GRANT siguen decidiendo quién lee o
-- escribe); las restrictivas aplican solo a sc_cliente.
--
-- Alcance: la variable la escribe el backend con el id de la sesión
-- autenticada. Protege contra errores de la aplicación (un id equivocado, un
-- WHERE olvidado); no contra inyección de SQL dentro de esa misma transacción,
-- que podría reescribir la variable. Contra eso, toda consulta va
-- parametrizada (sqlx).
-- =============================================================================

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'sc_cliente') THEN
        CREATE ROLE sc_cliente NOLOGIN;
    END IF;
    EXECUTE format('GRANT sc_cliente TO %I', current_user);
END
$$;

-- -----------------------------------------------------------------------------
-- El cliente de la transacción
-- -----------------------------------------------------------------------------

CREATE FUNCTION nucleo.cliente_actual() RETURNS UUID
LANGUAGE sql STABLE PARALLEL SAFE
AS $$ SELECT nullif(current_setting('sc.cliente_actual', true), '')::uuid $$;

COMMENT ON FUNCTION nucleo.cliente_actual() IS
    'Cliente a cuyo nombre corre la transacción (sc.cliente_actual, SET LOCAL); NULL si no hay.';

REVOKE ALL ON FUNCTION nucleo.cliente_actual() FROM PUBLIC;

-- -----------------------------------------------------------------------------
-- Privilegios de sc_cliente: lectura de las tablas del cliente, nada más
-- -----------------------------------------------------------------------------

GRANT USAGE ON SCHEMA nucleo, kyc, ifpe, sofom TO sc_cliente;
GRANT EXECUTE ON FUNCTION nucleo.cliente_actual() TO sc_cliente;
GRANT SELECT ON kyc.clientes, ifpe.cuentas, ifpe.transacciones, ifpe.spei_outbound, ifpe.spei_inbound,
                sofom.cuentas, sofom.contratos_mutuo, sofom.transacciones
    TO sc_cliente;

-- -----------------------------------------------------------------------------
-- RLS en las tablas que no la tenían: todo igual para los demás roles
-- -----------------------------------------------------------------------------
-- ifpe.transacciones y sofom.contratos_mutuo ya la tienen (0006), con una
-- política de lectura abierta a todos que también cubre a sc_cliente.

ALTER TABLE kyc.clientes        ENABLE ROW LEVEL SECURITY;
ALTER TABLE ifpe.cuentas        ENABLE ROW LEVEL SECURITY;
ALTER TABLE ifpe.spei_outbound  ENABLE ROW LEVEL SECURITY;
ALTER TABLE ifpe.spei_inbound   ENABLE ROW LEVEL SECURITY;
ALTER TABLE sofom.cuentas       ENABLE ROW LEVEL SECURITY;
ALTER TABLE sofom.transacciones ENABLE ROW LEVEL SECURITY;

CREATE POLICY por_privilegios ON kyc.clientes        USING (true) WITH CHECK (true);
CREATE POLICY por_privilegios ON ifpe.cuentas        USING (true) WITH CHECK (true);
CREATE POLICY por_privilegios ON ifpe.spei_outbound  USING (true) WITH CHECK (true);
CREATE POLICY por_privilegios ON ifpe.spei_inbound   USING (true) WITH CHECK (true);
CREATE POLICY por_privilegios ON sofom.cuentas       USING (true) WITH CHECK (true);
CREATE POLICY por_privilegios ON sofom.transacciones USING (true) WITH CHECK (true);

-- -----------------------------------------------------------------------------
-- sc_cliente: solo las filas del cliente actual
-- -----------------------------------------------------------------------------

CREATE POLICY solo_del_cliente ON kyc.clientes AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (id = nucleo.cliente_actual())
    WITH CHECK (id = nucleo.cliente_actual());

CREATE POLICY solo_del_cliente ON ifpe.cuentas AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual())
    WITH CHECK (cliente_id = nucleo.cliente_actual());

-- Una transferencia entre clientes la ven los dos: cada uno, su lado.
CREATE POLICY solo_del_cliente ON ifpe.transacciones AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cuenta_origen_id  IN (SELECT id FROM ifpe.cuentas WHERE cliente_id = nucleo.cliente_actual())
        OR cuenta_destino_id IN (SELECT id FROM ifpe.cuentas WHERE cliente_id = nucleo.cliente_actual()))
    WITH CHECK (cuenta_origen_id  IN (SELECT id FROM ifpe.cuentas WHERE cliente_id = nucleo.cliente_actual())
             OR cuenta_destino_id IN (SELECT id FROM ifpe.cuentas WHERE cliente_id = nucleo.cliente_actual()));

CREATE POLICY solo_del_cliente ON ifpe.spei_outbound AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual())
    WITH CHECK (cliente_id = nucleo.cliente_actual());

-- Un abono sin cuenta (en revisión o devuelto) no es de nadie: no se ve.
CREATE POLICY solo_del_cliente ON ifpe.spei_inbound AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cuenta_id IN (SELECT id FROM ifpe.cuentas WHERE cliente_id = nucleo.cliente_actual()))
    WITH CHECK (cuenta_id IN (SELECT id FROM ifpe.cuentas WHERE cliente_id = nucleo.cliente_actual()));

CREATE POLICY solo_del_cliente ON sofom.cuentas AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual())
    WITH CHECK (cliente_id = nucleo.cliente_actual());

CREATE POLICY solo_del_cliente ON sofom.contratos_mutuo AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cliente_id = nucleo.cliente_actual())
    WITH CHECK (cliente_id = nucleo.cliente_actual());

CREATE POLICY solo_del_cliente ON sofom.transacciones AS RESTRICTIVE FOR ALL TO sc_cliente
    USING (cuenta_id IN (SELECT id FROM sofom.cuentas WHERE cliente_id = nucleo.cliente_actual()))
    WITH CHECK (cuenta_id IN (SELECT id FROM sofom.cuentas WHERE cliente_id = nucleo.cliente_actual()));
