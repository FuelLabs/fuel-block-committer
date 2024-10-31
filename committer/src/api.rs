use std::sync::Arc;

use ::metrics::{
    prometheus::{self, Encoder, Registry, TextEncoder},
    HealthChecker,
};
use actix_web::{
    error::InternalError, get, http::StatusCode, web, App, HttpResponse, HttpServer, Responder,
};
use ports::storage::Storage;
use services::{status_reporter::StatusReporter, HealthReporter};

use crate::{
    config::Config,
    errors::{Error, Result},
    Database,
};

pub async fn launch_api_server(
    config: &Config,
    metrics_registry: Registry,
    storage: impl Storage + 'static,
    fuel_health_check: HealthChecker,
    eth_health_check: HealthChecker,
) -> Result<()> {
    let metrics_registry = Arc::new(metrics_registry);
    let status_reporter = Arc::new(StatusReporter::new(storage));
    let health_reporter = Arc::new(HealthReporter::new(fuel_health_check, eth_health_check));
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(Arc::clone(&metrics_registry)))
            .app_data(web::Data::new(Arc::clone(&status_reporter)))
            .app_data(web::Data::new(Arc::clone(&health_reporter)))
            .service(status)
            .service(metrics)
            .service(health)
    })
    .bind((config.app.host, config.app.port))
    .map_err(|e| Error::Other(e.to_string()))?
    .run()
    .await
    .map_err(|e| Error::Other(e.to_string()))
}

#[get("/health")]
async fn health(data: web::Data<Arc<HealthReporter>>) -> impl Responder {
    let report = data.report();

    let mut response = if report.healthy() {
        HttpResponse::Ok()
    } else {
        HttpResponse::InternalServerError()
    };

    response.json(report)
}

#[get("/status")]
async fn status(data: web::Data<Arc<StatusReporter<Database>>>) -> impl Responder {
    let report = data.current_status().await?;

    Result::Ok(web::Json(report))
}

#[get("/metrics")]
async fn metrics(registry: web::Data<Arc<Registry>>) -> impl Responder {
    let encoder = TextEncoder::new();
    let mut buf: Vec<u8> = vec![];
    let mut encode = |metrics: &_| {
        encoder
            .encode(metrics, &mut buf)
            .map_err(map_to_internal_err)
    };

    encode(&registry.gather())?;
    encode(&prometheus::gather())?;

    let text = String::from_utf8(buf).map_err(map_to_internal_err)?;

    std::result::Result::<_, InternalError<_>>::Ok(text)
}

fn map_to_internal_err(error: impl std::error::Error) -> InternalError<String> {
    InternalError::new(error.to_string(), StatusCode::INTERNAL_SERVER_ERROR)
}
