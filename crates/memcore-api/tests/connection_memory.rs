//! An open connection must cost kilobytes, not megabytes. Serving the `Router`
//! directly made axum rebuild every route's service stack per accepted connection (about 2.2 MB
//! each for memcore's router), so thousands of open load-test connections drove RSS above 2 GB.
//! A counting global allocator (this test binary only) measures the live heap of the REAL memcore
//! app served through the production `serve_until_shutdown`. Health and ordinary requests must keep
//! working while the connections are held, and the memory must be released when they close.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Duration;

use memcore_api::{AppState, create_app, serve_until_shutdown};
use memcore_config::Settings;
use memcore_core::ShutdownToken;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

struct Counting;

static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE_BYTES.fetch_add(
            new_size as isize - layout.size() as isize,
            Ordering::Relaxed,
        );
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const CONNECTIONS: usize = 200;
/// Fixed: about 4 KB per idle connection; before the fix: about 2,200 KB.
const MAX_BYTES_PER_CONNECTION: isize = 64 * 1024;

async fn get_status(addr: std::net::SocketAddr, path: &str) -> u16 {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let request = format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.expect("read");
    let text = String::from_utf8_lossy(&response);
    text.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0)
}

async fn settle() -> isize {
    // Let the server accept / drop everything, then read the live heap.
    tokio::time::sleep(Duration::from_millis(400)).await;
    LIVE_BYTES.load(Ordering::Relaxed)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_connections_cost_kilobytes_not_megabytes() {
    let settings = Settings {
        allow_mock: true,
        ..Settings::default()
    };
    let app = create_app(AppState::new(settings));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let token = ShutdownToken::new();
    let server_token = token.clone();
    let server = tokio::spawn(async move {
        serve_until_shutdown(
            listener,
            app,
            async move { server_token.cancelled().await },
            ShutdownToken::new(),
            Duration::from_secs(5),
        )
        .await
        .expect("serve");
    });
    // Warm up: one request so lazily-built state is not counted as per-connection cost.
    assert_eq!(get_status(addr, "/health/live").await, 200);
    let before = settle().await;

    let mut held = Vec::with_capacity(CONNECTIONS);
    for _ in 0..CONNECTIONS {
        held.push(TcpStream::connect(addr).await.expect("idle connection"));
    }
    let with_connections = settle().await;
    let per_connection = (with_connections - before) / CONNECTIONS as isize;
    assert!(
        per_connection < MAX_BYTES_PER_CONNECTION,
        "{per_connection} bytes per idle connection (limit {MAX_BYTES_PER_CONNECTION})"
    );

    // While the connections are held: liveness, readiness and an ordinary route still answer.
    assert_eq!(get_status(addr, "/health/live").await, 200);
    assert_eq!(get_status(addr, "/health/ready").await, 200);
    assert_eq!(get_status(addr, "/api/v1/version").await, 200);

    drop(held);
    let after_close = settle().await;
    assert!(
        after_close - before < MAX_BYTES_PER_CONNECTION,
        "memory not released after the connections closed: {} bytes still held",
        after_close - before
    );
    assert_eq!(get_status(addr, "/health/live").await, 200);

    token.cancel();
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("bounded shutdown")
        .expect("server task");
}
