use std::{
    borrow::Cow,
    sync::{Arc, Weak},
};

use testcontainers::{
    core::{ContainerPort, WaitFor},
    runners::AsyncRunner,
    Image,
};

use super::postgres::{DbConfig, Postgres};

struct PostgresImage {
    username: String,
    password: String,
    initial_db: String,
}

impl Image for PostgresImage {
    fn name(&self) -> &str {
        "postgres"
    }

    fn tag(&self) -> &str {
        "latest"
    }

    fn ready_conditions(&self) -> Vec<WaitFor> {
        vec![WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        )]
    }

    fn expose_ports(&self) -> &[ContainerPort] {
        &const { [ContainerPort::Tcp(5432)] }
    }

    fn env_vars(
        &self,
    ) -> impl IntoIterator<Item = (impl Into<Cow<'_, str>>, impl Into<Cow<'_, str>>)> {
        [
            ("POSTGRES_USER", self.username.as_str()),
            ("POSTGRES_PASSWORD", self.password.as_str()),
            ("POSTGRES_DB", self.initial_db.as_str()),
        ]
    }
}

pub struct PostgresProcess {
    username: String,
    password: String,
    initial_db: String,
    container: testcontainers::ContainerAsync<PostgresImage>,
}

impl PostgresProcess {
    pub async fn shared() -> ports::storage::Result<Arc<Self>> {
        // If at some point no tests are running, the shared instance will be dropped. If
        // requested again, it will be recreated.
        // This is a workaround for the lack of a global setup/teardown for tests.

        static LOCK: tokio::sync::Mutex<Weak<PostgresProcess>> =
            tokio::sync::Mutex::const_new(Weak::new());
        let mut shared_process = LOCK.lock().await;

        let process = if let Some(running_process) = shared_process.upgrade() {
            running_process
        } else {
            let process = Arc::new(Self::start().await?);
            *shared_process = Arc::downgrade(&process);
            process
        };

        Ok(process)
    }

    pub async fn start() -> ports::storage::Result<Self> {
        let username = "username".to_string();
        let password = "password".to_string();
        let initial_db = "test".to_string();

        let container = PostgresImage {
            username: username.clone(),
            password: password.clone(),
            initial_db: initial_db.clone(),
        }
        .start()
        .await
        .map_err(|e| ports::storage::Error::Database(format!("{e}")))?;

        Ok(Self {
            username,
            password,
            initial_db,
            container,
        })
    }

    pub async fn create_random_db(&self) -> ports::storage::Result<Postgres> {
        let port = self
            .container
            .get_host_port_ipv4(5432)
            .await
            .map_err(|e| ports::storage::Error::Database(format!("{e}")))?;

        let mut config = DbConfig {
            host: "localhost".to_string(),
            port,
            username: self.username.clone(),
            password: self.password.clone(),
            database: self.initial_db.clone(),
            max_connections: 5,
        };
        let db = Postgres::connect(&config).await?;

        let db_name = format!("test_db_{}", rand::random::<u32>());
        let query = format!("CREATE DATABASE {db_name}");
        db.execute(&query).await?;

        config.database = db_name;

        let db = Postgres::connect(&config).await?;

        db.migrate().await?;

        Ok(db)
    }
}
