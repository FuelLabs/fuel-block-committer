use serde_json::json;
use storage::{DbConfig, Postgres};

#[tokio::test]
#[ignore = "requires an isolated PostgreSQL database; set FCR_TEST_PG_PORT"]
async fn fcr_journal_excludes_a_second_writer_and_preserves_the_positive_latch() {
    let port = std::env::var("FCR_TEST_PG_PORT").unwrap().parse().unwrap();
    let user = std::env::var("FCR_TEST_PG_USER").unwrap_or_else(|_| "postgres".into());
    let config = DbConfig {
        host: "127.0.0.1".into(),
        port,
        username: user.clone(),
        password: "".into(),
        database: "postgres".into(),
        max_connections: 5,
        use_ssl: false,
    };
    let database = Postgres::connect(&config).await.unwrap();
    database.migrate().await.unwrap();
    // Seed only this disposable test writer, using the same explicitly initialized row shape.
    let pool = sqlx::PgPool::connect(&format!("postgresql://{user}@127.0.0.1:{port}/postgres"))
        .await
        .unwrap();
    sqlx::query("INSERT INTO fcr_safety(writer,state) VALUES ('integration-test',$1) ON CONFLICT(writer) DO UPDATE SET state=excluded.state").bind(json!({"incident":null,"generation":"0"})).execute(&pool).await.unwrap();
    let journal = database.fcr_journal("integration-test").await.unwrap();
    assert!(database.fcr_journal("integration-test").await.is_err());
    assert!(journal.load().await.unwrap()["incident"].is_null());
    journal
        .save(json!({"incident":"canonical:test","generation":"0"}))
        .await
        .unwrap();
    drop(journal);
    let journal = database.fcr_journal("integration-test").await.unwrap();
    assert_eq!(journal.load().await.unwrap()["incident"], "canonical:test");
    drop(journal);
    pool.close().await;
}
