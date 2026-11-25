-- =============================================================================
--  Synthetic Capital — 0026: la bóveda avisa de cada paso de un SPEI
-- =============================================================================
--
--  El rastreador SPEI del cliente (roadmap 17, la página Transfers) vuelve a
--  leer sus transferencias cada vez que su bóveda avisa por el WebSocket
--  (migración 0012: la cuenta sube de versión). Un SPEI que mueve el saldo ya
--  avisaba: el depósito abonado, el retiro y el reembolso de una orden que
--  regresó. Los pasos que no lo mueven no avisaban, y la pantalla seguía
--  diciendo "Processing" de un retiro ya liquidado:
--
--  1. Una orden de `ifpe.spei_outbound` cambia de estado (PENDIENTE ->
--     ENVIADA -> LIQUIDADA; DEVUELTA y RECHAZADA traen además su reembolso) o
--     recibe su fecha de operación (con ella, Banxico encuentra su CEP).
--  2. Un SPEI que llegó a una cuenta y se devolvió al emisor
--     (`ifpe.spei_inbound` DEVUELTO) no toca el saldo.
--
--  Ambos suben `ifpe.cuentas.version_ordenes` de la cuenta del cliente, como
--  las órdenes de mercado (0015), los planes de ahorro (0023), los avisos de
--  precio (0024) y las aclaraciones (0025). La versión de la bóveda sube una
--  sola vez por transacción (0012): un reembolso no avisa dos veces. El aviso
--  no dice nada más: el cliente lee su SPEI con su rol y RLS, y un depósito
--  devuelto no lleva causa (migración 0025, aviso indebido).
--
--  Candados: el despachador y el webhook de estado bloquean la orden y luego
--  la cuenta, como ya lo hace el reembolso de una orden devuelta. Si otra
--  transacción tiene la cuenta y espera la orden (abrir una aclaración sobre
--  ella toma su llave foránea), una de las dos aborta por deadlock y se
--  reintenta completa (`db::con_reintentos`).
--
--  SECURITY DEFINER: sc_spei cambia órdenes y registra SPEI recibidos, pero no
--  actualiza cuentas.
-- =============================================================================

CREATE FUNCTION ifpe.fn_avisar_boveda_spei() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, pg_temp
AS $$
BEGIN
    UPDATE ifpe.cuentas SET version_ordenes = version_ordenes + 1 WHERE id = NEW.cuenta_id;
    RETURN NULL;
END
$$;
COMMENT ON FUNCTION ifpe.fn_avisar_boveda_spei() IS
    'Sube la versión de la bóveda de la cuenta de un SPEI (0026): el rastreador del cliente vuelve a leerlo.';

-- Solo un cambio real: el despachador reprograma las órdenes con
-- `fecha_operacion = coalesce(fecha_operacion, ...)` sin cambiar nada.
CREATE TRIGGER trg_spei_outbound_avisar AFTER UPDATE OF estado, fecha_operacion ON ifpe.spei_outbound
    FOR EACH ROW
    WHEN ((NEW.estado, NEW.fecha_operacion) IS DISTINCT FROM (OLD.estado, OLD.fecha_operacion))
    EXECUTE FUNCTION ifpe.fn_avisar_boveda_spei();

-- Abonado ya avisa con el saldo; la devolución de una orden avisa en la orden.
CREATE TRIGGER trg_spei_inbound_avisar AFTER INSERT ON ifpe.spei_inbound
    FOR EACH ROW WHEN (NEW.estado = 'DEVUELTO' AND NEW.cuenta_id IS NOT NULL)
    EXECUTE FUNCTION ifpe.fn_avisar_boveda_spei();

REVOKE ALL ON FUNCTION ifpe.fn_avisar_boveda_spei() FROM PUBLIC;
