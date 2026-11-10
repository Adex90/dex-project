-- =============================================================================
--  Synthetic Capital — 0014: catálogo del mercado (el explorador)
-- =============================================================================
--
--  El explorador del frontend (/dashboard/discover) lista el catálogo con
--  GET /api/v1/market/assets, que lee esta misma tabla: lo que se lista es lo
--  que se cotiza y se opera, con las reglas de 0013 (precio, incremento,
--  posición, KYC y PLD). Dos activos más, con precio SIMULADO como los
--  primeros, para que el catálogo cubra renta variable y un certificado del
--  gobierno federal:
--
--  - STSLA: una acción sintética de Tesla, en fracciones de 0.0001.
--  - CETES28: CETES a 28 días, en títulos enteros (valor nominal de 10 pesos,
--    al 7.25 % anual: 10 / (1 + 0.0725 × 28 / 360) = 9.9439; un día antes,
--    a 29 días, 9.9419).
--
--  Sin MODO_DEMO el backend no cotiza, no opera ni lista un precio SIMULADO,
--  así que en producción estos renglones esperan al oráculo de precios.
--  Solo datos: ni el esquema ni las funciones cambian.
-- =============================================================================

INSERT INTO mercado.activos (ticker, nombre, clase, decimales_cantidad, precio, precio_cierre_previo,
                             capitalizacion, volumen_24h, rendimiento_anual, fuente_precio) VALUES
    ('STSLA',   'Synthetic Tesla, 1 share', 'RENTA_VARIABLE', 4, 4415.30, 4523.80, 215000000.00, 96500000.00, NULL,   'SIMULADO'),
    ('CETES28', 'CETES, 28 days',           'RENTA_FIJA',     0,  9.9439,  9.9419,          NULL, 18400000.00, 7.2500, 'SIMULADO');
