use std::{
    io::BufRead,
    sync::{Arc, OnceLock, Weak},
};

use futures::{Future, StreamExt, TryStreamExt};
use testcontainers::{core::WaitFor, runners::AsyncRunner, Image, RunnableImage};
use tokio::{runtime::Handle, sync::OnceCell};

use super::{DbConfig, Postgres};
use crate::adapters::storage::{Error, Result};

struct PostgresImage;

impl Image for PostgresImage {
    type Args = ();

    fn name(&self) -> String {
        "postgres".to_owned()
    }

    fn tag(&self) -> String {
        "latest".to_owned()
    }

    fn ready_conditions(&self) -> Vec<WaitFor> {
        vec![WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        )]
    }

    fn expose_ports(&self) -> Vec<u16> {
        vec![5432]
    }
}

pub struct PostgresProcess {
    username: String,
    password: String,
    initial_db: String,
    container: testcontainers::ContainerAsync<PostgresImage>,
}

fn block_in_place<F, R>(f: F) -> Result<R>
where
    F: Future<Output = R> + Send + 'static,
    R: Send + 'static,
{
    let handle = Handle::try_current().map_err(|e| Error::Other(e.to_string()))?;
    match handle.runtime_flavor() {
        tokio::runtime::RuntimeFlavor::CurrentThread => std::thread::spawn(move || {
            tokio::runtime::Runtime::new()
                .map(|rt| rt.block_on(f))
                .map_err(|e| {
                    Error::Other(format!(
                        "Couldn't run a tokio tuntime to do in-place blocking: {e}"
                    ))
                })
        })
        .join()
        .map_err(|_| Error::Other("Couldn't join the thread".to_string()))?,
        tokio::runtime::RuntimeFlavor::MultiThread => Ok(tokio::task::block_in_place(move || {
            Handle::current().block_on(f)
        })),
        _ => panic!("unsupported runtime flavor"),
    }
}

impl PostgresProcess {
    pub async fn shared() -> Result<Arc<Self>> {
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

    pub async fn start() -> Result<Self> {
        let username = "username".to_string();
        let password = "password".to_string();
        let initial_db = "test".to_string();

        let container = RunnableImage::from(PostgresImage)
            .with_env_var(("POSTGRES_USER", &username))
            .with_env_var(("POSTGRES_PASSWORD", &password))
            .with_env_var(("POSTGRES_DB", &initial_db))
            .start()
            .await;

        Ok(Self {
            container,
            username,
            password,
            initial_db,
        })
    }

    pub async fn create_random_db(&self) -> Result<Postgres> {
        let mut config = DbConfig {
            host: "localhost".to_string(),
            port: self.container.get_host_port_ipv4(5432).await,
            username: self.username.clone(),
            password: self.password.clone(),
            db: self.initial_db.clone(),
            max_connections: 5,
        };
        let db = Postgres::connect(&config).await?;

        let db_name = format!("test_db_{}", rand::random::<u32>());
        let query = format!("CREATE DATABASE {db_name}");
        db.execute(&query).await?;

        config.db = db_name.clone();

        let db = Postgres::connect(&config).await?;

        db.migrate().await?;

        Ok(db)
    }
}
