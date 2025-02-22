use std::sync::Arc;

use actix_web::{guard, web, App, HttpResponse, HttpServer};
use async_graphql::Schema;
use async_graphql_actix_web::{GraphQLRequest, GraphQLResponse};
use tokio::sync::Mutex;
use url::Url;

use super::{
    graphql::{build_schema, QueryRoot},
    simulation::{produce_blocks, AppState, SimulationConfig},
};

/// The FuelNode encapsulates the Actix‑Web server and holds simulation configuration.
pub struct FuelNode {
    shutdown_handle: Option<actix_web::dev::ServerHandle>,
    port: u16,
    config: Arc<Mutex<SimulationConfig>>,
}

impl FuelNode {
    /// Create a new FuelNode.
    pub fn new(port: u16, config: Arc<Mutex<SimulationConfig>>) -> Self {
        Self {
            shutdown_handle: None,
            port,
            config,
        }
    }

    /// Returns the GraphQL endpoint URL.
    pub fn url(&self) -> Url {
        Url::parse(&format!("http://localhost:{}/v1/graphql", self.port)).unwrap()
    }

    /// Runs the Actix‑Web server and concurrently spawns the block production loop.
    pub async fn run(&mut self) -> std::io::Result<()> {
        let initial_block_size = { self.config.lock().await.block_size };
        let state = Arc::new(AppState::new(initial_block_size));
        let state_clone = state.clone();
        let config_clone = self.config.clone();

        // Spawn block production.
        tokio::spawn(async move {
            produce_blocks(state_clone, config_clone).await;
        });

        // Build the GraphQL schema with the application state.
        let schema = build_schema().data(state).finish();

        let port = self.port;
        let server = HttpServer::new(move || {
            App::new()
                .app_data(web::Data::new(schema.clone()))
                .service(
                    web::resource("/v1/graphql")
                        .guard(guard::Post())
                        .to(graphql_handler),
                )
                .service(
                    web::resource("/v1/graphql")
                        .guard(guard::Get())
                        .to(graphql_playground),
                )
        })
        .bind(("0.0.0.0", port))?
        .run();

        self.shutdown_handle = Some(server.handle());
        println!(
            "GraphQL server running on http://localhost:{}/v1/graphql",
            port
        );
        tokio::spawn(server);

        Ok(())
    }

    /// Gracefully stops the server.
    pub async fn stop(&mut self) {
        if let Some(handle) = self.shutdown_handle.take() {
            handle.stop(true).await;
        }
    }
}

/// Handler for GraphQL requests.
async fn graphql_handler(
    schema: web::Data<
        Schema<QueryRoot, async_graphql::EmptyMutation, async_graphql::EmptySubscription>,
    >,
    req: GraphQLRequest,
) -> GraphQLResponse {
    schema.execute(req.into_inner()).await.into()
}

/// Handler for serving the GraphQL Playground.
async fn graphql_playground() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(async_graphql::http::playground_source(
            async_graphql::http::GraphQLPlaygroundConfig::new("/v1/graphql"),
        ))
}
