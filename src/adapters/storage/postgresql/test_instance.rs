use std::{
    io::BufRead,
    sync::{Arc, OnceLock, Weak},
};

use bollard::{
    container::{CreateContainerOptions, LogsOptions, StopContainerOptions},
    service::{HostConfig, PortBinding, PortMap},
    Docker,
};
use futures::{Future, StreamExt, TryStreamExt};
use testcontainers::Image;
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
    container: PostgresContainer,
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

struct PostgresContainer {
    id: String,
    port: u16,
    username: String,
    password: String,
    initial_db: String,
}

impl PostgresContainer {
    fn map_port(container: u16, host: u16) -> PortMap {
        [(
            format!("{container}/tcp"),
            Some(vec![PortBinding {
                host_ip: Some("localhost".to_string()),
                host_port: Some(host.to_string()),
            }]),
        )]
        .into()
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn host(&self) -> String {
        "localhost".to_string()
    }

    pub async fn create(connection: &Docker) -> Result<Self> {
        let free_host_port = portpicker::pick_unused_port().ok_or_else(|| {
            Error::Other("No free port available to use for postgres.".to_string())
        })?;

        let host_config = HostConfig {
            port_bindings: Some(Self::map_port(5432, free_host_port)),
            ..Default::default()
        };

        let username = "username".to_string();
        let password = "password".to_string();
        let db = "test".to_string();

        let container = connection
            .create_container::<&str, _>(
                None,
                bollard::container::Config {
                    image: Some("postgres:latest"),
                    env: Some(vec![
                        &format!("POSTGRES_USER={username}"),
                        &format!("POSTGRES_PASSWORD={password}"),
                        &format!("POSTGRES_DB={db}"),
                    ]),
                    host_config: Some(host_config),
                    ..Default::default()
                },
            )
            .await?;

        Ok(Self {
            id: container.id,
            port: free_host_port,
            username,
            password,
            initial_db: db,
        })
    }

    pub async fn start(&self, docker: &Docker) -> Result<()> {
        docker.start_container::<&str>(&self.id, None).await?;
        Ok(())
    }

    pub async fn wait_ready(&self, docker: &Docker) -> Result<()> {
        let mut log_stream = docker.logs::<String>(
            &self.id,
            Some(LogsOptions {
                follow: true,
                stderr: true,
                ..Default::default()
            }),
        );

        let fut = async {
            // The expectation is that chunks are line-aligned.
            while let Some(chunk) = log_stream.try_next().await? {
                if chunk
                    .to_string()
                    .contains("database system is ready to accept connections")
                {
                    break;
                }
            }
            Result::Ok(())
        };

        tokio::time::timeout(std::time::Duration::from_secs(10), fut)
            .await
            .map_err(|_| {
                Error::Other("Timed out waiting for the database to be ready".to_string())
            })??;

        Ok(())
    }

    pub async fn stop_and_remove(connection: Docker, id: String) -> Result<()> {
        let wait_before_forcing = 1;
        // We use a separate step to stop the container, although the
        // `remove_container` function can also stop and remove it. This is
        // because `remove_container` might wait indefinitely for the container
        // to stop, as it doesn't allow setting a timeout.

        // We ignore the result of stopping the container, as it might already be stopped or
        // not started at all.
        let _ = connection
            .stop_container(
                &id,
                Some(StopContainerOptions {
                    t: wait_before_forcing,
                }),
            )
            .await;

        connection
            .remove_container(
                &id,
                Some(bollard::container::RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await?;

        Result::Ok(())
    }
}

impl Drop for PostgresContainer {
    fn drop(&mut self) {
        let id = self.id.clone();

        if let Ok(docker) = Docker::connect_with_local_defaults() {
            block_in_place(Self::stop_and_remove(docker, id))
                .unwrap()
                .unwrap();
        } else {
            panic!("Docker connection failed");
        }
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
        let connection = Docker::connect_with_local_defaults()?;

        let container = PostgresContainer::create(&connection).await?;
        container.start(&connection).await?;
        container.wait_ready(&connection).await?;

        Ok(Self { container })
    }

    pub async fn create_random_db(&self) -> Result<Postgres> {
        let mut config = DbConfig {
            host: self.container.host(),
            port: self.container.port(),
            username: self.container.username.clone(),
            password: self.container.password.clone(),
            db: self.container.initial_db.clone(),
            connections: 1,
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
