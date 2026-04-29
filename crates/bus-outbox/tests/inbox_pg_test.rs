use bus_core::{IdempotencyStore, MessageId};
use bus_outbox::{PostgresIdempotencyStore, migrate::run_migrations};
use sqlx::PgPool;
use std::time::Duration;
use testcontainers::{
    GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};

async fn start_postgres() -> (impl Drop, String) {
    let c = GenericImage::new("postgres", "16-alpine")
        .with_exposed_port(5432.tcp())
        .with_wait_for(WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        ))
        .with_env_var("POSTGRES_PASSWORD", "password")
        .with_env_var("POSTGRES_DB", "testdb")
        .start()
        .await
        .unwrap();
    let host = c.get_host().await.unwrap();
    let port = c.get_host_port_ipv4(5432).await.unwrap();
    (
        c,
        format!("postgres://postgres:password@{}:{}/testdb", host, port),
    )
}

#[tokio::test]
async fn first_insert_returns_true() {
    let (_c, url) = start_postgres().await;
    let pool = PgPool::connect(&url).await.unwrap();
    run_migrations(&pool).await.unwrap();

    let store = PostgresIdempotencyStore::new(pool, "test-consumer".into());
    let id = MessageId::new();
    assert!(
        store
            .try_insert(&id, Duration::from_secs(3600))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn second_insert_returns_false() {
    let (_c, url) = start_postgres().await;
    let pool = PgPool::connect(&url).await.unwrap();
    run_migrations(&pool).await.unwrap();

    let store = PostgresIdempotencyStore::new(pool, "test-consumer".into());
    let id = MessageId::new();
    store
        .try_insert(&id, Duration::from_secs(3600))
        .await
        .unwrap();
    assert!(
        !store
            .try_insert(&id, Duration::from_secs(3600))
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn different_consumers_independent() {
    let (_c, url) = start_postgres().await;
    let pool = PgPool::connect(&url).await.unwrap();
    run_migrations(&pool).await.unwrap();

    let store_a = PostgresIdempotencyStore::new(pool.clone(), "consumer-a".into());
    let store_b = PostgresIdempotencyStore::new(pool, "consumer-b".into());
    let id = MessageId::new();

    assert!(
        store_a
            .try_insert(&id, Duration::from_secs(3600))
            .await
            .unwrap()
    );
    // Same message_id but different consumer — must return true
    assert!(
        store_b
            .try_insert(&id, Duration::from_secs(3600))
            .await
            .unwrap()
    );
}
