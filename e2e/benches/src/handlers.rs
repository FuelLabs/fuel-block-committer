use super::*;
use crate::data::{AppData, ConfigForm};
use crate::template;

/// Serves the control panel page by rendering the embedded template.
pub async fn serve_control_panel(data: web::Data<AppData>) -> HttpResponse {
    // Get the current configuration.
    let cfg = data.simulation_config.lock().await;
    let current_block_size = cfg.block_size;
    let current_compress = cfg.compressibility.to_string().to_lowercase();
    drop(cfg);

    let html = template::render_control_panel(current_block_size, &current_compress);
    HttpResponse::Ok().content_type("text/html").body(html)
}

/// Handles form submission to update the simulation configuration.
/// Returns a 303 See Other redirect back to the control panel.
pub async fn update_config(form: web::Form<ConfigForm>, data: web::Data<AppData>) -> HttpResponse {
    println!("Received update: {:?}", form);
    let compressibility = match form.compressibility.parse::<Compressibility>() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error parsing compressibility: {}", e);
            Compressibility::Medium // fallback default
        }
    };

    {
        let mut cfg = data.simulation_config.lock().await;
        cfg.block_size = form.block_size;
        cfg.compressibility = compressibility;
        println!(
            "Updated config: block_size={}, compressibility={}",
            cfg.block_size, cfg.compressibility
        );
    }
    HttpResponse::SeeOther()
        .append_header(("location", "/"))
        .finish()
}

/// Proxies a GET request for `/proxy/metrics` to the committer metrics URL.
pub async fn proxy_metrics(data: web::Data<AppData>) -> HttpResponse {
    let url = data.metrics_url.clone();
    match reqwest::get(&url).await {
        Ok(resp) => match resp.text().await {
            Ok(body) => HttpResponse::Ok().content_type("text/plain").body(body),
            Err(e) => {
                eprintln!("Error reading metrics response: {}", e);
                HttpResponse::InternalServerError()
                    .body(format!("Error reading metrics response: {}", e))
            }
        },
        Err(e) => {
            eprintln!("Error fetching metrics: {}", e);
            HttpResponse::InternalServerError().body(format!("Error fetching metrics: {}", e))
        }
    }
}
