use bus_nats::{ConnectOptions, NatsClient, StreamConfig};
use std::time::Duration;
use testcontainers::{
    GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};

#[tokio::test]
async fn connect_with_options_authenticates_with_user_password() {
    let c = GenericImage::new("nats", "2.10-alpine")
        .with_exposed_port(4222.tcp())
        .with_wait_for(WaitFor::message_on_stderr("Server is ready"))
        .with_cmd(["-js", "--user", "svc", "--pass", "secret"])
        .start()
        .await
        .unwrap();
    let host = c.get_host().await.unwrap();
    let port = c.get_host_port_ipv4(4222).await.unwrap();
    let url = format!("nats://{host}:{port}");
    let cfg = StreamConfig {
        num_replicas: 1,
        ..Default::default()
    };

    assert!(
        NatsClient::connect(&url, &cfg).await.is_err(),
        "plain connect must fail without auth"
    );

    let mut last_err = None;
    for _ in 0..30 {
        let opts = ConnectOptions::with_user_and_password("svc".into(), "secret".into());
        match NatsClient::connect_with_options(&url, opts, &cfg).await {
            Ok(_) => return,
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    panic!("authed connect failed: {last_err:?}");
}
