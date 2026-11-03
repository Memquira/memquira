//! Bounded graceful shutdown, exercised through `serve_until_shutdown`, the function
//! `run()` serves with: in-flight requests finish, new connections are refused once shutdown
//! starts, and the drain is bounded even when a request never finishes.

use std::time::{Duration, Instant};

use axum::Router;
use axum::routing::get;
use memcore_api::serve_until_shutdown;
use memcore_core::ShutdownToken;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

fn test_router() -> Router {
    Router::new()
        .route(
            "/slow",
            get(|| async {
                tokio::time::sleep(Duration::from_millis(800)).await;
                "done"
            }),
        )
        .route(
            "/hang",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(3_600)).await;
                "never"
            }),
        )
}

async fn start(
    drain: Duration,
) -> (
    std::net::SocketAddr,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<Instant>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let token = ShutdownToken::new();
    let (tx, rx) = oneshot::channel::<()>();
    let signal_token = token.child_token();
    let signal = async move {
        let _ = rx.await;
        signal_token.cancel();
    };
    let server = tokio::spawn(async move {
        serve_until_shutdown(listener, test_router(), signal, token, drain)
            .await
            .expect("server should stop cleanly");
        Instant::now()
    });
    (addr, tx, server)
}

async fn request(addr: std::net::SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    stream
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n").as_bytes(),
        )
        .await
        .expect("write");
    let mut response = String::new();
    stream.read_to_string(&mut response).await.expect("read");
    response
}

#[tokio::test]
async fn in_flight_request_completes_and_new_connections_are_refused() {
    let (addr, signal, server) = start(Duration::from_secs(10)).await;
    let in_flight = tokio::spawn(request(addr, "/slow"));
    tokio::time::sleep(Duration::from_millis(200)).await;

    signal.send(()).expect("signal");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        TcpStream::connect(addr).await.is_err(),
        "new connections must be refused once shutdown starts"
    );

    let response = in_flight.await.expect("join");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("done"), "{response}");
    server.await.expect("server task");
}

#[tokio::test]
async fn drain_is_bounded_when_a_request_never_finishes() {
    let (addr, signal, server) = start(Duration::from_secs(1)).await;
    let _hung = tokio::spawn(request(addr, "/hang"));
    tokio::time::sleep(Duration::from_millis(200)).await;

    let started = Instant::now();
    signal.send(()).expect("signal");
    let stopped = server.await.expect("server task");
    let drain = stopped.duration_since(started);
    assert!(
        drain >= Duration::from_millis(900),
        "drain waited {drain:?}"
    );
    assert!(
        drain < Duration::from_secs(3),
        "drain must be bounded, took {drain:?}"
    );
}
