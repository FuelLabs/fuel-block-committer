use std::sync::Arc;

use actix_web::{
    error::InternalError, get, http::StatusCode, web, App, HttpResponse, HttpServer, Responder,
};
use prometheus::{Encoder, Registry, TextEncoder};

use crate::{
    errors::{Error, Result},
    services::StatusReporter,
};

pub async fn launch(
    metrics_registry: Arc<Registry>,
    status_reporter: Arc<StatusReporter>,
) -> Result<()> {
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(Arc::clone(&metrics_registry)))
            .app_data(web::Data::new(Arc::clone(&status_reporter)))
            .service(status)
            .service(metrics)
        // .service(health)
    })
    .bind(("127.0.0.1", 7070))
    .unwrap() //TODO read via config PARAM
    .run()
    .await
    .map_err(|e| Error::Other(e.to_string()))
}

#[get("/health")]
async fn health() -> impl Responder {
    // TODO: add report for fuel-core & ethereum RPC connection

    HttpResponse::Ok()
}

#[get("/status")]
async fn status(data: web::Data<Arc<StatusReporter>>) -> impl Responder {
    let report = data.current_status().await?;

    Result::Ok(web::Json(report))
}

#[get("/metrics")]
async fn metrics(registry: web::Data<Arc<Registry>>) -> impl Responder {
    let encoder = TextEncoder::new();
    let mut buf: Vec<u8> = vec![];
    let mut encode = |metrics: &_| {
        encoder
            .encode(&metrics, &mut buf)
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
