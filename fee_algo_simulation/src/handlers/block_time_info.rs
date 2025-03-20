use crate::utils::ETH_BLOCK_TIME;

use super::*;

pub async fn get_block_time_info(state: actix_web::web::Data<AppState>) -> HttpResponse {
    let current_height = match state.fee_api.current_height().await {
        Ok(h) => h,
        Err(e) => {
            error!("Error fetching current height: {:?}", e);
            return HttpResponse::InternalServerError()
                .body("Could not fetch current block height");
        }
    };

    let last_block_time = match state.fee_api.inner().get_block_time(current_height).await {
        Ok(Some(t)) => t,
        _ => {
            return HttpResponse::InternalServerError()
                .body("Last block time not found".to_string());
        }
    };

    HttpResponse::Ok().json(json!({
        "last_block_height": current_height,
        "last_block_time": last_block_time.to_rfc3339(),
        "block_interval": ETH_BLOCK_TIME
    }))
}
