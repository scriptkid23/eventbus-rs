use bus_outbox::SqliteBuffer;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[test]
fn insert_and_pop_in_order() {
    let buf = SqliteBuffer::in_memory().unwrap();
    buf.insert("id-1", "events.test", b"payload1", "{}", now_ms())
        .unwrap();
    buf.insert("id-2", "events.test", b"payload2", "{}", now_ms() + 1)
        .unwrap();

    let rows = buf.fetch_pending(10).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, "id-1");
    assert_eq!(rows[1].id, "id-2");
}

#[test]
fn delete_removes_row() {
    let buf = SqliteBuffer::in_memory().unwrap();
    buf.insert("id-1", "events.test", b"payload", "{}", now_ms())
        .unwrap();
    buf.delete("id-1").unwrap();

    let rows = buf.fetch_pending(10).unwrap();
    assert!(rows.is_empty());
}

#[test]
fn pending_count() {
    let buf = SqliteBuffer::in_memory().unwrap();
    assert_eq!(buf.pending_count().unwrap(), 0);
    buf.insert("id-1", "events.test", b"data", "{}", now_ms())
        .unwrap();
    assert_eq!(buf.pending_count().unwrap(), 1);
}
