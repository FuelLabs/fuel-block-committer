use std::sync::Arc;

use ::metrics::{
    HealthChecker,
    prometheus::{self, Encoder, Registry, TextEncoder},
};
use actix_web::{
    App, HttpResponse, HttpServer, Responder, error::InternalError, get, http::StatusCode, web,
};
use serde::Deserialize;
use services::{
    cost_reporter::service::CostReporter, health_reporter::service::HealthReporter,
    status_reporter::service::StatusReporter,
};

use crate::{
    Database,
    config::{Config, Internal},
    errors::{Error, Result},
};

pub async fn launch_api_server(
    config: &Config,
    internal_config: &Internal,
    metrics_registry: Registry,
    storage: impl services::status_reporter::port::Storage + Clone + 'static,
    fuel_health_check: HealthChecker,
    eth_health_check: HealthChecker,
) -> Result<()> {
    let metrics_registry = Arc::new(metrics_registry);
    let status_reporter = Arc::new(StatusReporter::new(storage.clone()));
    let health_reporter = Arc::new(HealthReporter::new(fuel_health_check, eth_health_check));
    let cost_reporter = Arc::new(CostReporter::new(
        storage,
        internal_config.cost_request_limit,
    ));
    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(Arc::clone(&metrics_registry)))
            .app_data(web::Data::new(Arc::clone(&status_reporter)))
            .app_data(web::Data::new(Arc::clone(&health_reporter)))
            .app_data(web::Data::new(Arc::clone(&cost_reporter)))
            .service(status)
            .service(metrics)
            .service(health)
            .service(costs)
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum HeightVariant {
    Latest,
    Specific,
}

#[derive(Deserialize)]
struct CostQueryParams {
    variant: HeightVariant,
    value: Option<u32>,
    limit: Option<usize>,
}

#[get("/v1/costs")]
async fn costs(
    data: web::Data<Arc<CostReporter<Database>>>,
    query: web::Query<CostQueryParams>,
) -> impl Responder {
    let limit = query.limit.unwrap_or(100);

    let response = match query.variant {
        HeightVariant::Latest => data.get_latest_costs(limit).await,
        HeightVariant::Specific => match query.value {
            Some(height) => data.get_costs(height, limit).await,
            None => Err(services::Error::Other(
                "height value is required".to_string(),
            )),
        },
    };

    match response {
        Ok(costs) => HttpResponse::Ok().json(costs),
        Err(services::Error::Other(e)) => {
            HttpResponse::from_error(InternalError::new(e, StatusCode::BAD_REQUEST))
        }
        Err(e) => HttpResponse::from_error(map_to_internal_err(e)),
    }
}

fn map_to_internal_err(error: impl std::error::Error) -> InternalError<String> {
    InternalError::new(error.to_string(), StatusCode::INTERNAL_SERVER_ERROR)
}
