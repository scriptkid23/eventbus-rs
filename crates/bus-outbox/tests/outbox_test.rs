use bus_core::MessageId;
use bus_macros::Event;
use bus_outbox::{PostgresOutboxStore, migrate::run_migrations, store::OutboxStore};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
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
    let url = format!("postgres://postgres:password@{}:{}/testdb", host, port);
    (c, url)
}

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "orders.created", aggregate = "order")]
struct OrderCreated {
    id:    MessageId,
    total: i64,
}

#[tokio::test]
async fn insert_and_fetch_pending() {
    let (_c, url) = start_postgres().await;
    let pool = PgPool::connect(&url).await.unwrap();
    run_migrations(&pool).await.unwrap();

    let store = PostgresOutboxStore::new(pool.clone());
    let event = OrderCreated {
        id:    MessageId::new(),
        total: 100,
    };

    let mut tx = pool.begin().await.unwrap();
    store.insert(&mut tx, &event).await.unwrap();
    tx.commit().await.unwrap();

    let rows = store.fetch_pending(10).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].subject, "orders.created");
}

#[tokio::test]
async fn mark_published_removes_from_pending() {
    let (_c, url) = start_postgres().await;
    let pool = PgPool::connect(&url).await.unwrap();
    run_migrations(&pool).await.unwrap();

    let store = PostgresOutboxStore::new(pool.clone());
    let event = OrderCreated {
        id:    MessageId::new(),
        total: 200,
    };

    let mut tx = pool.begin().await.unwrap();
    store.insert(&mut tx, &event).await.unwrap();
    tx.commit().await.unwrap();

    let rows = store.fetch_pending(10).await.unwrap();
    let id = MessageId::from_uuid(rows[0].id);
    store.mark_published(&id).await.unwrap();

    let rows_after = store.fetch_pending(10).await.unwrap();
    assert!(
        rows_after.is_empty(),
        "published row must not appear in pending"
    );
}
