use actix_web::{get, web, App, HttpResponse, HttpServer, Responder, Result};
use serde::Serialize;

#[derive(Serialize)]
enum Status {
    Idle,
    Commiting,
}

#[derive(Serialize)]
struct StatusReport {
    pub latest_fuel_block: u64,
    pub latest_committed_block: u64,
    pub status: Status,
    pub ethereum_wallet_gas_balance: u64,
}

#[derive(Clone)]
struct StatusReporter {}

impl StatusReporter {
    fn status_report(&self) -> StatusReport {
        StatusReport {
            latest_fuel_block: 0,
            latest_committed_block: 0,
            status: Status::Idle,
            ethereum_wallet_gas_balance: 0,
        }
    }
}

pub async fn _launch(_port: u64) -> std::io::Result<()> {
    HttpServer::new(|| {
        App::new()
            .app_data(web::Data::new(StatusReporter {}))
            .service(health)
            .service(status)
            .service(metrics)
    })
    .bind(("127.0.0.1", 8080))? //TODO read via config PARAM
    .run()
    .await
}

#[get("/health")]
async fn health() -> impl Responder {
    // TODO: add report for fuel-core & ethereum RPC connection

    HttpResponse::Ok()
}

#[get("/status")]
async fn status(data: web::Data<StatusReporter>) -> Result<impl Responder> {
    let report = data.status_report();

    Ok(web::Json(report))
}

#[get("/metrics")]
async fn metrics() -> impl Responder {
    // TODO read from actor

    HttpResponse::Ok()
}
