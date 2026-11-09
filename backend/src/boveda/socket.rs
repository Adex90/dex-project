//! Una conexión WebSocket de la bóveda.
//!
//! 1. Foto completa (`vault.snapshot`) con la versión leída.
//! 2. Después, cada cambio con versión mayor que la última enviada; una
//!    versión vieja o repetida no se envía.
//! 3. Foto completa otra vez cuando el navegador pide `{"type": "resync"}`
//!    (como mucho una por segundo: [`Tiempos::resync_minimo`]),
//!    cuando se publica un corte de reserva (misma versión, prueba nueva),
//!    cuando el socket se queda atrás o cuando LISTEN se reconecta.
//! 4. Latido: ping cada 25 s, que mantiene viva la conexión en nginx y en el
//!    balanceador; sin ninguna trama del navegador en 75 s, se cierra
//!    ([`Tiempos`]).
//! 5. La sesión manda: al vencer el JWT, o si la base de datos ya no la
//!    acepta (se revisa cada minuto), se cierra con 4001 y el navegador
//!    vuelve a autenticarse.

use std::time::Duration;

use axum::{
    body::Bytes,
    extract::ws::{close_code, CloseFrame, Message, WebSocket},
};
use tokio::{
    sync::broadcast::error::RecvError,
    time::{interval_at, sleep_until, Instant, MissedTickBehavior},
};

use super::{
    central::{esperar_apagado, Evento, Suscripcion},
    consulta::Lectura,
    modelos::{Boveda, Mensaje, MensajeCliente},
};
use crate::{
    auth::{self, jwt::Claims},
    error::AppError,
};

/// Subprotocolo que el navegador debe pedir (y que el servidor confirma).
pub const PROTOCOLO: &str = "sc.vault.v1";
/// Prefijo de la entrada del subprotocolo que lleva el JWT: `bearer.<jwt>`.
pub const PREFIJO_TOKEN: &str = "bearer.";
/// Sesión vencida o revocada (rango 4000-4999, propio de la aplicación).
pub const CIERRE_SESION: u16 = 4001;

/// Los plazos de un socket: los de producción son [`Tiempos::default`]; las
/// pruebas los acortan con [`super::CentralBoveda::con_tiempos`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tiempos {
    /// Cada cuánto se envía un ping. Menor que los plazos de inactividad de
    /// nginx (`proxy_read_timeout` 90 s) y del balanceador.
    pub latido: Duration,
    /// Sin ninguna trama del navegador (un pong cuenta) en este lapso, se cierra.
    pub silencio_maximo: Duration,
    /// Cada cuánto se vuelve a confirmar la sesión contra la base de datos.
    pub revalidacion: Duration,
    /// Mínimo entre dos `resync` que pide el navegador: los que llegan antes
    /// se juntan en uno al cumplirse. Sin esto, un cliente que los pidiera sin
    /// parar ocuparía el pool de la base de datos con lecturas de su bóveda.
    pub resync_minimo: Duration,
}

impl Default for Tiempos {
    fn default() -> Self {
        Self {
            latido: Duration::from_secs(25),
            silencio_maximo: Duration::from_secs(75),
            revalidacion: Duration::from_secs(60),
            resync_minimo: Duration::from_secs(1),
        }
    }
}

pub async fn atender(mut socket: WebSocket, mut suscripcion: Suscripcion, claims: Claims) {
    let cliente_id = suscripcion.cliente_id;
    match conversar(&mut socket, &mut suscripcion, &claims).await {
        Ok(Some(cierre)) => {
            let _ = socket.send(Message::Close(Some(cierre))).await;
        }
        // El navegador cerró o se fue: nada más que decir.
        Ok(None) => {}
        Err(e) => tracing::debug!(%cliente_id, error = %e, "socket de la bóveda cerrado con error"),
    }
}

fn cierre(code: u16, razon: &'static str) -> CloseFrame {
    CloseFrame { code, reason: razon.into() }
}

async fn enviar(socket: &mut WebSocket, mensaje: &Mensaje) -> Result<(), axum::Error> {
    let texto = serde_json::to_string(mensaje).expect("los mensajes de la bóveda siempre se serializan");
    socket.send(Message::Text(texto.into())).await
}

/// La última foto enviada: con ella se reenvía el estado cuando solo cambia
/// la prueba de reserva, sin volver a la base de datos.
struct Enviado {
    seq: i64,
    boveda: Boveda,
}

impl Enviado {
    fn aplicar(&mut self, mensaje: &Mensaje) {
        self.seq = mensaje.seq();
        match mensaje {
            Mensaje::Instantanea { boveda, .. } => self.boveda = boveda.clone(),
            Mensaje::Spei { saldo, .. } => self.boveda.saldo = saldo.clone(),
        }
    }

    fn foto(&self) -> Mensaje {
        Mensaje::Instantanea { seq: self.seq, boveda: self.boveda.clone() }
    }
}

async fn leer(suscripcion: &Suscripcion) -> Result<Lectura, AppError> {
    suscripcion.central().leer(suscripcion.cliente_id).await
}

/// Foto completa, si la versión leída no es más vieja que la enviada (nunca lo
/// es: las versiones solo suben). Un error de lectura no cierra el socket: el
/// siguiente evento o `resync` lo vuelve a intentar.
async fn resincronizar(
    socket: &mut WebSocket,
    suscripcion: &Suscripcion,
    enviado: &mut Enviado,
) -> Result<(), axum::Error> {
    match leer(suscripcion).await {
        Ok(lectura) if lectura.seq >= enviado.seq => {
            let foto = Mensaje::Instantanea { seq: lectura.seq, boveda: lectura.boveda };
            enviar(socket, &foto).await?;
            enviado.aplicar(&foto);
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(cliente_id = %suscripcion.cliente_id, error = %e.detalle(), "no se pudo resincronizar la bóveda")
        }
    }
    Ok(())
}

/// Lo que dura la conversación. `Ok(Some(cierre))`: el servidor cierra con ese
/// código; `Ok(None)`: el navegador ya cerró.
async fn conversar(
    socket: &mut WebSocket,
    suscripcion: &mut Suscripcion,
    claims: &Claims,
) -> Result<Option<CloseFrame>, axum::Error> {
    let mut enviado = match leer(suscripcion).await {
        Ok(lectura) => Enviado { seq: lectura.seq, boveda: lectura.boveda },
        Err(e) => {
            tracing::warn!(cliente_id = %suscripcion.cliente_id, error = %e.detalle(), "no se pudo leer la bóveda");
            return Ok(Some(cierre(close_code::ERROR, "no se pudo leer la bóveda")));
        }
    };
    enviar(socket, &enviado.foto()).await?;

    let tiempos = suscripcion.central().tiempos();
    let ahora = Instant::now();
    let restante = u64::try_from(claims.exp - chrono::Utc::now().timestamp()).unwrap_or(0);
    let vence = ahora + Duration::from_secs(restante);
    let mut latido = interval_at(ahora + tiempos.latido, tiempos.latido);
    latido.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut revision = interval_at(ahora + tiempos.revalidacion, tiempos.revalidacion);
    revision.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut ultimo_contacto = ahora;
    let mut apagado = suscripcion.senal_de_apagado();
    let mut resync_pedido = false;
    let mut proximo_resync = ahora;

    loop {
        tokio::select! {
            evento = suscripcion.recibir() => match evento {
                Ok(Evento::Cambio(mensaje)) => {
                    if mensaje.seq() > enviado.seq {
                        enviar(socket, &mensaje).await?;
                        enviado.aplicar(&mensaje);
                    }
                }
                Ok(Evento::Reserva(prueba)) => {
                    enviado.boveda.prueba_reserva = (*prueba).clone();
                    enviar(socket, &enviado.foto()).await?;
                }
                // Se perdieron eventos (LISTEN reconectado, o este socket se quedó atrás).
                Ok(Evento::Resincronizar) | Err(RecvError::Lagged(_)) => {
                    resincronizar(socket, suscripcion, &mut enviado).await?;
                }
                Err(RecvError::Closed) => return Ok(Some(cierre(close_code::AWAY, "el servidor se reinicia"))),
            },
            entrante = socket.recv() => match entrante {
                None | Some(Err(_)) => return Ok(None),
                // tungstenite ya encoló la respuesta al cierre: la envía la siguiente
                // lectura, que después termina (cierre limpio para el navegador).
                Some(Ok(Message::Close(_))) => {
                    let _ = tokio::time::timeout(Duration::from_secs(1), socket.recv()).await;
                    return Ok(None);
                }
                Some(Ok(Message::Text(texto))) => {
                    ultimo_contacto = Instant::now();
                    resync_pedido |= MensajeCliente::pide_resincronizar(texto.as_str());
                }
                // Pong, Ping (tungstenite ya respondió) o binario: solo cuentan como señal de vida.
                Some(Ok(_)) => ultimo_contacto = Instant::now(),
            },
            // De inmediato, o al cumplirse `resync_minimo` desde el anterior.
            () = sleep_until(proximo_resync), if resync_pedido => {
                resync_pedido = false;
                resincronizar(socket, suscripcion, &mut enviado).await?;
                proximo_resync = Instant::now() + tiempos.resync_minimo;
            }
            _ = latido.tick() => {
                if ultimo_contacto.elapsed() > tiempos.silencio_maximo {
                    return Ok(Some(cierre(close_code::AWAY, "sin respuesta")));
                }
                socket.send(Message::Ping(Bytes::new())).await?;
            }
            _ = revision.tick() => {
                match auth::revalidar(suscripcion.central().pool(), claims).await {
                    Ok(_) => {}
                    Err(AppError::NoAutenticado | AppError::Prohibido | AppError::MfaRequerido) => {
                        return Ok(Some(cierre(CIERRE_SESION, "sesión revocada")));
                    }
                    // Un fallo pasajero de la base de datos no cierra la sesión.
                    Err(e) => tracing::warn!(error = %e.detalle(), "no se pudo revalidar la sesión del socket"),
                }
            }
            () = sleep_until(vence) => return Ok(Some(cierre(CIERRE_SESION, "sesión vencida"))),
            () = esperar_apagado(&mut apagado) => return Ok(Some(cierre(close_code::AWAY, "el servidor se reinicia"))),
        }
    }
}
