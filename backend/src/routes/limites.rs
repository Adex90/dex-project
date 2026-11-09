//! Límite de peticiones por IP de cliente, en el propio servicio.
//!
//! Repite los límites del borde (`nginx/snippets/rate-limits.conf` en un host,
//! el WAF en AWS) por si el borde falla, cambia o se salta, y cuida la CPU de
//! cada tarea: cada inicio de sesión cuesta un hash Argon2.
//!
//! | Grupo     | Rutas                                                 | Por omisión          |
//! |-----------|-------------------------------------------------------|----------------------|
//! | `auth`    | `/api/v1/auth/*` (registro, login, 2FA, contraseña)   | 5/min, ráfaga 10     |
//! | `kyc`     | `POST /api/v1/kyc/verificar`                          | 4/min, ráfaga 4      |
//! | `api`     | el resto de `/api/v1`; `auth` y `kyc` también cuentan | 600/min, ráfaga 50   |
//! | `webhook` | `/api/v1/spei/webhook/*`, `/api/v1/kyc/webhook`       | 3000/min, ráfaga 200 |
//!
//! `/salud` y `/vivo` no se limitan: son las sondas del balanceador.
//!
//! - **Clave.** La IP que lee [`super::extraccion::ip_cliente`]: detrás del
//!   proxy propio, la última de `X-Forwarded-For`, que escribe el proxy. Nunca
//!   la primera, que escribe el cliente: con ella cualquiera esquivaría el
//!   límite o agotaría el de otro. Una IPv6 cuenta por su /64, que un cliente
//!   suele tener entera.
//! - **Respuesta.** `429` con `{"error": {"codigo": "REINTENTAR"}}` y
//!   `Retry-After`, como el borde, así que el cliente ve lo mismo en las dos capas.
//! - **Alcance.** Cada tarea cuenta en memoria. Con N tareas detrás del
//!   balanceador, una IP puede llegar a N veces la cuota; el límite global es el
//!   del WAF. Las cuentas de las IP inactivas se borran cada minuto.

use std::{
    net::{IpAddr, Ipv6Addr},
    sync::Arc,
    time::Duration,
};

use axum::{
    body::Body,
    http::{header, HeaderValue, Request, Response, StatusCode},
    response::IntoResponse,
};
use governor::{clock::QuantaInstant, middleware::NoOpMiddleware};
use tower_governor::{governor::GovernorConfigBuilder, key_extractor::KeyExtractor, GovernorError, GovernorLayer};

use super::extraccion::ip_cliente;
use crate::{
    config::{Cuota, LimitesHttp},
    error::AppError,
};

/// Capa de un grupo de rutas. Sus clones comparten las cuentas.
pub type Capa = GovernorLayer<ClaveCliente, NoOpMiddleware<QuantaInstant>, Body>;

/// Cada cuánto se borran las cuentas de las IP que ya no tienen nada pendiente.
const LIMPIEZA: Duration = Duration::from_secs(60);

/// La IP del cliente; IPv6 agrupada por /64.
#[derive(Debug, Clone, Copy)]
pub struct ClaveCliente {
    proxy_confiable: bool,
}

impl KeyExtractor for ClaveCliente {
    type Key = IpAddr;

    fn extract<T>(&self, peticion: &Request<T>) -> Result<IpAddr, GovernorError> {
        ip_cliente(peticion.headers(), peticion.extensions(), self.proxy_confiable)
            .map(agrupar)
            .ok_or(GovernorError::UnableToExtractKey)
    }
}

/// Una IPv4 tal cual (también la escrita como IPv6, `::ffff:a.b.c.d`); una IPv6
/// por su /64.
pub fn agrupar(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let s = v6.segments();
                IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
            }
        },
        v4 => v4,
    }
}

/// Las capas de cada grupo; ninguna con `HTTP_LIMITE_IP=false`.
#[derive(Clone, Default)]
pub struct Limitadores {
    pub auth: Option<Capa>,
    pub kyc: Option<Capa>,
    pub api: Option<Capa>,
    pub webhook: Option<Capa>,
}

impl Limitadores {
    pub fn nuevos(http: &LimitesHttp) -> Self {
        let l = &http.por_ip;
        if !l.activo {
            return Self::default();
        }
        let clave = ClaveCliente { proxy_confiable: http.proxy_confiable };
        Self {
            auth: Some(capa(l.auth, clave)),
            kyc: Some(capa(l.kyc, clave)),
            api: Some(capa(l.api, clave)),
            webhook: Some(capa(l.webhook, clave)),
        }
    }
}

/// El router con la capa del grupo, o tal cual si los límites están apagados.
pub fn aplicar<S: Clone + Send + Sync + 'static>(router: axum::Router<S>, capa: &Option<Capa>) -> axum::Router<S> {
    match capa {
        Some(c) => router.layer(c.clone()),
        None => router,
    }
}

fn capa(cuota: Cuota, clave: ClaveCliente) -> Capa {
    // GCRA: una petición más cada `60 s / por_minuto`, hasta `rafaga` seguidas.
    let periodo = Duration::from_nanos(60_000_000_000 / u64::from(cuota.por_minuto.max(1)));
    let config = GovernorConfigBuilder::default()
        .key_extractor(clave)
        .period(periodo)
        .burst_size(cuota.rafaga.max(1))
        .finish()
        .expect("periodo y ráfaga mayores que 0");
    let config = Arc::new(config);

    // Sin limpieza, cada IP nueva deja su cuenta para siempre. La tarea termina
    // con el router (referencia débil).
    let limitador = Arc::downgrade(config.limiter());
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
            let mut intervalo = tokio::time::interval(LIMPIEZA);
            intervalo.tick().await;
            loop {
                intervalo.tick().await;
                let Some(limitador) = limitador.upgrade() else { break };
                limitador.retain_recent();
                limitador.shrink_to_fit();
            }
        });
    }

    GovernorLayer::new(config).error_handler(respuesta)
}

fn respuesta(error: GovernorError) -> Response<Body> {
    match error {
        GovernorError::TooManyRequests { wait_time, .. } => {
            let mut r = AppError::peticion(
                StatusCode::TOO_MANY_REQUESTS,
                "REINTENTAR",
                "demasiadas peticiones; espera unos segundos y reintenta",
            )
            .into_response();
            r.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from(wait_time.max(1)));
            r
        }
        // El servidor pone la IP de la conexión en cada petición
        // (servidor.rs): sin ella algo está mal configurado. Nunca se deja
        // pasar sin contar.
        GovernorError::UnableToExtractKey => {
            AppError::Interno(anyhow::anyhow!("límite por IP: la petición no trae la IP del cliente")).into_response()
        }
        GovernorError::Other { code, msg, .. } => {
            AppError::peticion(code, "REINTENTAR", msg.unwrap_or_else(|| "reintenta más tarde".into())).into_response()
        }
    }
}

#[cfg(test)]
mod pruebas {
    use super::*;

    #[test]
    fn ipv6_cuenta_por_su_64_e_ipv4_tal_cual() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:ffff:ffff:ffff:ffff".parse().unwrap();
        let otra: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(agrupar(a), agrupar(b));
        assert_ne!(agrupar(a), agrupar(otra));
        assert_eq!(agrupar("2001:db8:1:2::".parse().unwrap()), agrupar(a));

        let v4: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(agrupar(v4), v4);
        assert_eq!(agrupar("::ffff:203.0.113.7".parse().unwrap()), v4);
    }
}
