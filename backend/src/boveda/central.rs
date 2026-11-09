//! Los sockets de la bóveda abiertos en esta tarea, y lo que los alimenta.
//!
//! PostgreSQL avisa por `sc_boveda` (migración 0012) cada versión nueva de una
//! cuenta y cada corte de reserva publicado, solo si la transacción confirma
//! y en orden de confirmación. Cada tarea del backend escucha con una conexión
//! propia (LISTEN) y reparte el aviso entre los sockets que tiene abiertos
//! para ese cliente: funciona igual con una tarea que con muchas detrás del
//! balanceador, sin coordinación entre ellas.
//!
//! - Un cambio se lee una vez por cliente (no una por socket) y el mismo
//!   mensaje llega a todas sus pestañas.
//! - Si se cae la conexión de LISTEN, los avisos de ese lapso se pierden: al
//!   reconectar, cada socket recibe una foto completa.
//! - Topes: sockets por tarea (`HTTP_MAX_CONEXIONES`: un socket ya abierto
//!   deja de contar para el tope HTTP) y por cliente.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use serde::Deserialize;
use sqlx::{postgres::PgListener, PgPool};
use tokio::sync::{
    broadcast::{self, error::RecvError},
    watch, OwnedSemaphorePermit, Semaphore, SemaphorePermit,
};
use uuid::Uuid;

use super::{
    consulta::{self, Lectura},
    modelos::{Mensaje, PruebaReserva},
    socket::Tiempos,
};
use crate::{config::Config, error::AppResult};

/// Canal de PostgreSQL (migración 0012).
pub const CANAL: &str = "sc_boveda";
/// Sockets de un mismo cliente a la vez en una tarea (pestañas, dispositivos).
pub const MAX_SOCKETS_POR_CLIENTE: usize = 5;
/// Eventos que un socket lento puede acumular; si se rebasa, se resincroniza.
const CAPACIDAD: usize = 32;
/// Cada cuánto se sondea la conexión de LISTEN. Una conexión que solo espera
/// avisos no envía nada: RDS Proxy la cerraría por inactiva
/// (`idle_client_timeout`), y una que murió sin aviso del sistema operativo
/// (un NAT o un proxy que perdió su estado) no fallaría nunca, solo callaría.
const SONDEO: Duration = Duration::from_secs(30);
const ESPERA_SONDEO: Duration = Duration::from_secs(10);

/// Lo que la central entrega a cada socket.
#[derive(Debug, Clone)]
pub enum Evento {
    /// Cambio en la bóveda del cliente, ya leído: el mismo para todos sus sockets.
    Cambio(Arc<Mensaje>),
    /// Corte de reserva nuevo: cada socket reenvía su foto con esta prueba.
    Reserva(Arc<PruebaReserva>),
    /// Algo pudo perderse: cada socket lee y envía una foto completa.
    Resincronizar,
}

/// El aviso de PostgreSQL (`pg_notify` en la migración 0012).
#[derive(Debug, Deserialize)]
#[serde(tag = "tipo", rename_all = "lowercase")]
enum Aviso {
    Saldo { cliente_id: Uuid, seq: i64, xact: String },
    Reserva { corte_id: Uuid },
}

/// Por qué no se abre un socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rechazo {
    /// La tarea llegó a su tope de sockets.
    TareaLlena,
    /// El cliente ya tiene [`MAX_SOCKETS_POR_CLIENTE`] sockets en esta tarea.
    DemasiadosDelCliente,
}

pub struct CentralBoveda {
    pool: PgPool,
    config: Arc<Config>,
    canales: Mutex<HashMap<Uuid, broadcast::Sender<Evento>>>,
    cupo: Arc<Semaphore>,
    max_sockets: usize,
    apagado: watch::Sender<bool>,
    tiempos: Tiempos,
    /// Lecturas de bóvedas a la vez (fotos de los sockets y cambios avisados):
    /// la mitad del pool. Una ráfaga (LISTEN que reconecta y resincroniza
    /// todos los sockets, muchos cambios juntos) espera su turno en lugar de
    /// dejar sin conexiones a las peticiones HTTP.
    lecturas: Semaphore,
}

impl CentralBoveda {
    pub fn nueva(pool: PgPool, config: Arc<Config>) -> Arc<Self> {
        Self::con_tiempos(pool, config, Tiempos::default())
    }

    /// Con otros plazos de latido y revalidación (las pruebas los acortan).
    pub fn con_tiempos(pool: PgPool, config: Arc<Config>, tiempos: Tiempos) -> Arc<Self> {
        let max_sockets = config.http.max_conexiones;
        Arc::new(Self {
            pool,
            canales: Mutex::default(),
            cupo: Arc::new(Semaphore::new(max_sockets)),
            max_sockets,
            apagado: watch::channel(false).0,
            tiempos,
            lecturas: Semaphore::new(usize::try_from(config.max_conexiones / 2).unwrap_or(1).max(1)),
            config,
        })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn tiempos(&self) -> Tiempos {
        self.tiempos
    }

    /// La bóveda del cliente, en turno con las demás lecturas ([`Self::lecturas`]).
    pub async fn leer(&self, cliente_id: Uuid) -> AppResult<Lectura> {
        let _turno = self.turno().await;
        consulta::leer(&self.pool, &self.config, cliente_id).await
    }

    async fn turno(&self) -> SemaphorePermit<'_> {
        self.lecturas.acquire().await.expect("el semáforo de lecturas nunca se cierra")
    }

    /// Sockets abiertos en esta tarea.
    pub fn sockets(&self) -> usize {
        self.max_sockets - self.cupo.available_permits()
    }

    /// Inscribe un socket del cliente. Se inscribe antes de leer la foto
    /// inicial: un cambio que confirme entre la lectura y la inscripción no
    /// se pierde (el socket descarta lo que ya trae la foto, por versión).
    pub fn suscribir(self: &Arc<Self>, cliente_id: Uuid) -> Result<Suscripcion, Rechazo> {
        let permiso = Arc::clone(&self.cupo).try_acquire_owned().map_err(|_| Rechazo::TareaLlena)?;
        let mut canales = self.canales();
        let canal = canales.entry(cliente_id).or_insert_with(|| broadcast::channel(CAPACIDAD).0);
        if canal.receiver_count() >= MAX_SOCKETS_POR_CLIENTE {
            return Err(Rechazo::DemasiadosDelCliente);
        }
        Ok(Suscripcion {
            cliente_id,
            eventos: Some(canal.subscribe()),
            apagado: self.apagado.subscribe(),
            central: Arc::clone(self),
            _permiso: permiso,
        })
    }

    /// Apagado de la tarea: cada socket cierra con 1001 y el navegador se
    /// reconecta, ya a otra tarea; [`escuchar`] suelta su conexión.
    pub fn cerrar(&self) {
        let _ = self.apagado.send(true);
    }

    /// Tras [`Self::cerrar`]: espera, con plazo, a que todos los sockets hayan
    /// enviado su cierre. `false` si venció el plazo.
    pub async fn esperar_cierre(&self, plazo: Duration) -> bool {
        let total = u32::try_from(self.max_sockets).unwrap_or(u32::MAX);
        // Cada socket devuelve su permiso al terminar: tenerlos todos = no queda ninguno.
        tokio::time::timeout(plazo, self.cupo.acquire_many(total)).await.is_ok()
    }

    fn canales(&self) -> MutexGuard<'_, HashMap<Uuid, broadcast::Sender<Evento>>> {
        // Las secciones críticas no hacen nada que pueda dejar el mapa a medias.
        self.canales.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn canal(&self, cliente_id: Uuid) -> Option<broadcast::Sender<Evento>> {
        self.canales().get(&cliente_id).cloned()
    }

    fn a_todos(&self, evento: &Evento) {
        for canal in self.canales().values() {
            let _ = canal.send(evento.clone());
        }
    }

    fn soltar(&self, cliente_id: Uuid) {
        let mut canales = self.canales();
        if canales.get(&cliente_id).is_some_and(|c| c.receiver_count() == 0) {
            canales.remove(&cliente_id);
        }
    }

    fn procesar(self: &Arc<Self>, carga: &str) {
        match serde_json::from_str::<Aviso>(carga) {
            Err(e) => tracing::warn!(error = %e, carga, "aviso de {CANAL} ilegible"),
            Ok(Aviso::Saldo { cliente_id, seq, xact }) => {
                // Sin sockets de ese cliente aquí, no hay nada que leer.
                let Some(canal) = self.canal(cliente_id) else { return };
                let central = Arc::clone(self);
                tokio::spawn(async move {
                    let leido = {
                        let _turno = central.turno().await;
                        consulta::mensaje_por_cambio(&central.pool, &central.config, cliente_id, &xact).await
                    };
                    let evento = match leido {
                        Ok(mensaje) => Evento::Cambio(Arc::new(mensaje)),
                        Err(e) => {
                            tracing::warn!(%cliente_id, seq, error = %e.detalle(), "no se pudo leer el cambio de la bóveda");
                            Evento::Resincronizar
                        }
                    };
                    let _ = canal.send(evento);
                });
            }
            Ok(Aviso::Reserva { corte_id }) => {
                if self.canales().is_empty() {
                    return;
                }
                let central = Arc::clone(self);
                tokio::spawn(async move {
                    let leida = {
                        let _turno = central.turno().await;
                        consulta::prueba_reserva(&central.pool).await
                    };
                    let evento = match leida {
                        Ok(prueba) => Evento::Reserva(Arc::new(prueba)),
                        Err(e) => {
                            tracing::warn!(%corte_id, error = %e.detalle(), "no se pudo leer la prueba de reserva");
                            Evento::Resincronizar
                        }
                    };
                    central.a_todos(&evento);
                });
            }
        }
    }
}

/// La inscripción de un socket. Al soltarla se liberan su cupo y, si era el
/// último socket del cliente en esta tarea, su canal.
pub struct Suscripcion {
    pub cliente_id: Uuid,
    eventos: Option<broadcast::Receiver<Evento>>,
    apagado: watch::Receiver<bool>,
    central: Arc<CentralBoveda>,
    _permiso: OwnedSemaphorePermit,
}

impl Suscripcion {
    pub fn central(&self) -> &Arc<CentralBoveda> {
        &self.central
    }

    pub async fn recibir(&mut self) -> Result<Evento, RecvError> {
        match self.eventos.as_mut() {
            Some(eventos) => eventos.recv().await,
            None => Err(RecvError::Closed),
        }
    }

    /// La señal de apagado de la tarea, para esperarla junto a [`Self::recibir`].
    pub fn senal_de_apagado(&self) -> watch::Receiver<bool> {
        self.apagado.clone()
    }
}

/// Termina cuando la tarea empieza a apagarse.
pub async fn esperar_apagado(senal: &mut watch::Receiver<bool>) {
    if senal.wait_for(|apagando| *apagando).await.is_err() {
        // La central ya no existe: la señal no llegará nunca.
        std::future::pending::<()>().await;
    }
}

impl Drop for Suscripcion {
    fn drop(&mut self) {
        // Primero el receptor, para que la cuenta de la central sea exacta.
        drop(self.eventos.take());
        self.central.soltar(self.cliente_id);
    }
}

/// Escucha `sc_boveda` hasta el apagado ([`CentralBoveda::cerrar`]) y
/// reconecta con espera creciente. Ocupa una conexión del pool mientras
/// escucha; la devuelve al apagarse, para que el pool cierre sin esperar.
pub async fn escuchar(central: Arc<CentralBoveda>) {
    let mut apagado = central.apagado.subscribe();
    tokio::select! {
        () = escuchar_siempre(&central) => {}
        () = esperar_apagado(&mut apagado) => tracing::debug!("LISTEN {CANAL} termina con la tarea"),
    }
}

async fn escuchar_siempre(central: &Arc<CentralBoveda>) {
    let mut espera = Duration::from_secs(1);
    loop {
        match conectar(&central.pool).await {
            Ok(mut oyente) => {
                tracing::info!("escuchando los avisos de la bóveda (LISTEN {CANAL})");
                espera = Duration::from_secs(1);
                // Lo que confirmó mientras no se escuchaba no se va a avisar.
                central.a_todos(&Evento::Resincronizar);
                let mut sondeo = tokio::time::interval_at(tokio::time::Instant::now() + SONDEO, SONDEO);
                loop {
                    tokio::select! {
                        // try_recv es cancel-safe: si gana el sondeo, no se consumió ningún aviso.
                        aviso = oyente.try_recv() => match aviso {
                            Ok(Some(aviso)) => central.procesar(aviso.payload()),
                            // Se vuelve a escuchar desde cero, y la foto de cada socket
                            // llega ya escuchando: si llegara antes del LISTEN nuevo, lo
                            // que confirmara entre ambos se perdería.
                            Ok(None) => {
                                tracing::warn!("se perdió la conexión de LISTEN {CANAL}; se reconecta");
                                break;
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "LISTEN {CANAL} falló; se reintenta");
                                break;
                            }
                        },
                        _ = sondeo.tick() => {
                            if let Err(motivo) = sondear(&mut oyente).await {
                                tracing::warn!(%motivo, "la conexión de LISTEN {CANAL} no responde; se reconecta");
                                break;
                            }
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, segundos = espera.as_secs(), "no se pudo escuchar {CANAL}; se reintenta")
            }
        }
        tokio::time::sleep(espera).await;
        espera = (espera * 2).min(Duration::from_secs(30));
    }
}

async fn conectar(pool: &PgPool) -> Result<PgListener, sqlx::Error> {
    let mut oyente = PgListener::connect_with(pool).await?;
    // La reconexión es de `escuchar_siempre`, que resincroniza al volver a escuchar.
    oyente.eager_reconnect(false);
    oyente.listen(CANAL).await?;
    Ok(oyente)
}

/// `SELECT 1` por la conexión de LISTEN, con plazo.
async fn sondear(oyente: &mut PgListener) -> Result<(), String> {
    match tokio::time::timeout(ESPERA_SONDEO, sqlx::query("SELECT 1").execute(&mut *oyente)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(format!("sin respuesta en {} s", ESPERA_SONDEO.as_secs())),
    }
}
