//! The shipped `memcore-api` executable
//! (`load_settings` → `run`) refuses every context-cache configuration whose invalidation is not
//! verified for its process topology, and starts with the safe ones (cases CG1–CG15).
//!
//! Each case runs the real binary with a cleared environment (plus the OS variables a process
//! needs), mock providers (`MEMCORE_ALLOW_MOCK=true`), a loopback port and a temporary working
//! directory (no `.env` is picked up). Refused cases must exit non-zero with the policy error,
//! never accept a connection on the API port and, for Redis, never connect to the configured
//! Redis address (a test-owned listener that must see zero connections).

use std::collections::BTreeSet;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_memcore-api");
const DEV_API_KEY: &str = "memcore_dev_key";
const ORG: &str = "org_cache_gate";
const USER: &str = "user_cache_gate";
const SECRET: &str = "cg-s3cret-pass";

/// The ports handed out to this binary's tests; its lock also serialises reserving a port with
/// spawning a child. A child spawned while a reservation listener is open inherits it and keeps it
/// listening until its `exec`, so a "released" port could still accept a connection that the
/// refused-start checks would blame on memcore-api. Under this lock no child is spawned while a
/// reservation listener exists (`Command::spawn` returns after the child's `exec`), and no two
/// tests get the same port.
static PORTS: Mutex<BTreeSet<u16>> = Mutex::new(BTreeSet::new());

fn ports() -> MutexGuard<'static, BTreeSet<u16>> {
    PORTS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A loopback listener on a port no other test in this binary was given (callers hold `ports()`).
fn reserve(used: &mut BTreeSet<u16>) -> TcpListener {
    loop {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        if used.insert(listener.local_addr().expect("addr").port()) {
            return listener;
        }
    }
}

/// The OS variables a Windows / Unix process needs; nothing memcore-specific.
fn os_env() -> Vec<(String, String)> {
    [
        "SystemRoot",
        "SYSTEMROOT",
        "windir",
        "TEMP",
        "TMP",
        "PATH",
        "USERPROFILE",
        "HOME",
    ]
    .iter()
    .filter_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| (key.to_string(), value))
    })
    .collect()
}

struct Api {
    child: Child,
    port: u16,
    _dir: tempfile::TempDir,
}

fn spawn(vars: &[(&str, String)]) -> Api {
    let dir = tempfile::tempdir().expect("tempdir");
    // Held until this child is spawned, so no child (this one or another test's) inherits a
    // reservation listener.
    let mut used = ports();
    let port = reserve(&mut used).local_addr().expect("addr").port();
    let mut command = Command::new(BIN);
    command
        .env_clear()
        .envs(os_env())
        .env("MEMCORE_HOST", "127.0.0.1")
        .env("MEMCORE_PORT", port.to_string())
        .env("MEMCORE_ALLOW_MOCK", "true")
        // A disposable SQLite file and the mock vector store (the binary's own defaults are
        // `./data/memcore.db` and Qdrant); nothing cache-related.
        .env(
            "MEMCORE_DATABASE_URL",
            format!(
                "sqlite://{}/memcore.db?mode=rwc",
                dir.path().display().to_string().replace('\\', "/")
            ),
        )
        .env("MEMCORE_VECTOR_BACKEND", "mock")
        .current_dir(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in vars {
        command.env(key, value);
    }
    Api {
        child: command.spawn().expect("spawn memcore-api"),
        port,
        _dir: dir,
    }
}

/// Whether something accepts connections on `port`. A probe connected to itself (the OS gave it
/// `port` as its own source port, which Windows does because `bind(0)` and outgoing connections
/// share one port range) is TCP simultaneous open and proves there is no listener.
fn listening(port: u16) -> bool {
    TcpStream::connect_timeout(
        &format!("127.0.0.1:{port}").parse().expect("addr"),
        Duration::from_millis(200),
    )
    .is_ok_and(|stream| {
        !matches!((stream.local_addr(), stream.peer_addr()), (Ok(local), Ok(peer)) if local == peer)
    })
}

fn output_of(child: &mut Child) -> String {
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut out);
    }
    out
}

/// A refused start: exits non-zero within the deadline, never listens; returns its output.
fn expect_refused(vars: &[(&str, String)], expected: &str) -> String {
    let mut api = spawn(vars);
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        assert!(
            !listening(api.port),
            "a refused configuration must never accept connections"
        );
        if let Some(status) = api.child.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = api.child.kill();
            panic!("refused configuration did not exit: {vars:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let output = output_of(&mut api.child);
    assert!(!status.success(), "must exit non-zero: {output}");
    assert!(
        output.contains(expected),
        "expected {expected:?} in: {output}"
    );
    assert!(
        output.contains("set MEMCORE_CONTEXT_CACHE_BACKEND=disabled")
            || output.contains("MEMCORE_CONTEXT_CACHE_BACKEND=disabled")
            || output.contains("Invalid MEMCORE_CONTEXT_CACHE_BACKEND value")
            || output.contains("must be one of: true, false"),
        "the error must name the safe alternative: {output}"
    );
    assert!(!listening(api.port));
    output
}

fn http(port: u16, method: &str, path: &str, body: Option<&str>) -> (u16, serde_json::Value) {
    let mut stream = TcpStream::connect(format!("127.0.0.1:{port}")).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("timeout");
    let body = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {DEV_API_KEY}\r\n\
         X-Organization-ID: {ORG}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("write");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read");
    let status: u16 = raw
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let json = match (raw.find("\r\n\r\n"), raw.rfind('}')) {
        (Some(head), Some(end)) => raw[head..=end]
            .find('{')
            .and_then(|start| serde_json::from_str(&raw[head + start..=end]).ok())
            .unwrap_or(serde_json::Value::Null),
        _ => serde_json::Value::Null,
    };
    (status, json)
}

/// An allowed start: `/health` answers 200 within the deadline.
fn expect_started(vars: &[(&str, String)]) -> Api {
    let mut api = spawn(vars);
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        if let Some(status) = api.child.try_wait().expect("try_wait") {
            let output = output_of(&mut api.child);
            panic!("allowed configuration exited ({status}): {output}");
        }
        if listening(api.port) && http(api.port, "GET", "/health", None).0 == 200 {
            return api;
        }
        assert!(Instant::now() < deadline, "API did not become healthy");
        std::thread::sleep(Duration::from_millis(100));
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn add_memory(port: u16, text: &str) {
    let body = serde_json::json!({
        "user_id": USER,
        "messages": [{ "role": "user", "content": text }],
        "metadata": {}
    })
    .to_string();
    let (status, json) = http(port, "POST", "/api/v1/memories", Some(&body));
    assert_eq!(status, 200, "{json}");
}

fn context(port: u16, query: &str) -> serde_json::Value {
    let body = serde_json::json!({ "user_id": USER, "query": query }).to_string();
    let (status, json) = http(port, "POST", "/api/v1/context", Some(&body));
    assert_eq!(status, 200, "{json}");
    json
}

fn v(value: &str) -> String {
    value.to_string()
}

/// The disabled path: the response carries no cache metadata (omitted when the cache is off) or
/// reports it disabled, and is never a hit.
fn assert_uncached(json: &serde_json::Value) {
    assert_ne!(json["cache"]["enabled"], true, "{json}");
    assert_ne!(json["cache"]["hit"], true, "{json}");
}

const FIRST: &str = "I am learning Rust and building a memory engine.";
const SECOND: &str = "My favourite editor for Rust is Helix.";

#[test]
fn cg1_default_configuration_starts_and_serves_uncached_context() {
    let api = expect_started(&[]);
    add_memory(api.port, FIRST);
    let json = context(api.port, FIRST);
    assert_eq!(json["status"], "success");
    assert_uncached(&json);
}

#[test]
fn cg2_cg11_explicit_disabled_starts_and_recomputes_every_request() {
    let api = expect_started(&[
        ("MEMCORE_CONTEXT_CACHE_BACKEND", v("disabled")),
        ("MEMCORE_ENV", v("production")),
        ("MEMCORE_CONTEXT_CACHE_TTL_SECONDS", v("3600")),
    ]);
    add_memory(api.port, FIRST);
    let first = context(api.port, "Rust");
    assert_uncached(&first);
    assert!(first["context"].as_str().unwrap_or("").contains("Rust"));
    // CG11: a new memory is visible to the identical request (nothing was cached).
    add_memory(api.port, SECOND);
    let second = context(api.port, "Rust");
    assert_uncached(&second);
    assert!(
        second["context"].as_str().unwrap_or("").contains("Helix"),
        "{second}"
    );
}

/// CG3 / CG4 (the same test runs in a `--features redis-cache` build): Redis is refused before
/// any connection; the configured Redis address is a test-owned listener that must stay idle.
#[test]
fn cg3_cg4_redis_is_refused_without_any_connection_attempt() {
    let redis = reserve(&mut ports());
    redis.set_nonblocking(true).expect("nonblocking");
    let redis_port = redis.local_addr().expect("addr").port();
    for extra in [
        vec![],
        vec![
            ("MEMCORE_ENV", v("development")),
            ("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", v("true")),
        ],
        vec![("MEMCORE_ENV", v("production"))],
    ] {
        let mut vars = vec![
            ("MEMCORE_CONTEXT_CACHE_BACKEND", v("redis")),
            (
                "MEMCORE_REDIS_URL",
                format!("redis://:{SECRET}@127.0.0.1:{redis_port}/0"),
            ),
        ];
        vars.extend(extra);
        let output = expect_refused(
            &vars,
            "MEMCORE_CONTEXT_CACHE_BACKEND=redis is not supported",
        );
        assert!(!output.contains(SECRET), "CG14: {output}");
        assert!(
            !output.contains(&format!(":{redis_port}")),
            "CG14: {output}"
        );
    }
    match redis.accept() {
        Err(error) if error.kind() == ErrorKind::WouldBlock => {}
        Ok(_) => panic!("memcore-api connected to the configured Redis"),
        Err(error) => panic!("unexpected accept error: {error}"),
    }
    #[cfg(feature = "redis-cache")]
    eprintln!("CG4: executed with the redis-cache feature compiled in");
}

#[test]
fn cg5_memory_without_the_local_opt_in_is_refused() {
    expect_refused(
        &[
            ("MEMCORE_CONTEXT_CACHE_BACKEND", v("memory")),
            ("MEMCORE_ENV", v("development")),
        ],
        "needs MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS=true",
    );
}

#[test]
fn cg6_memory_with_the_local_opt_in_starts_and_caches_in_process() {
    let api = expect_started(&[
        ("MEMCORE_CONTEXT_CACHE_BACKEND", v("memory")),
        ("MEMCORE_ENV", v("development")),
        ("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", v("true")),
    ]);
    add_memory(api.port, FIRST);
    let first = context(api.port, FIRST);
    assert_eq!(first["cache"]["enabled"], true);
    assert_eq!(first["cache"]["hit"], false);
    let second = context(api.port, FIRST);
    assert_eq!(second["cache"]["hit"], true, "{second}");
}

#[test]
fn cg7_memory_in_production_or_ambiguous_configuration_is_refused_despite_the_opt_in() {
    let opt_in = ("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", v("true"));
    let memory = ("MEMCORE_CONTEXT_CACHE_BACKEND", v("memory"));
    expect_refused(
        &[
            memory.clone(),
            opt_in.clone(),
            ("MEMCORE_ENV", v("production")),
        ],
        "not with MEMCORE_ENV=production",
    );
    expect_refused(
        &[
            memory.clone(),
            opt_in.clone(),
            ("MEMCORE_ENV", v("development")),
            ("MEMCORE_STORAGE_MODE", v("production")),
        ],
        "MEMCORE_STORAGE_MODE=production",
    );
    let output = expect_refused(
        &[
            memory.clone(),
            opt_in.clone(),
            ("MEMCORE_ENV", v("development")),
            ("MEMCORE_FACT_BACKEND", v("postgres")),
            (
                "MEMCORE_POSTGRES_URL",
                format!("postgres://memcore:{SECRET}@127.0.0.1:1/memcore"),
            ),
        ],
        "MEMCORE_FACT_BACKEND=postgres",
    );
    assert!(!output.contains(SECRET), "CG14: {output}");
    // Ambiguous: MEMCORE_ENV unset (defaults to development, not accepted as evidence).
    expect_refused(
        &[memory, opt_in],
        "needs MEMCORE_ENV set explicitly to development",
    );
}

#[test]
fn cg8_unknown_backend_is_refused() {
    expect_refused(
        &[("MEMCORE_CONTEXT_CACHE_BACKEND", v("memcached"))],
        "Invalid MEMCORE_CONTEXT_CACHE_BACKEND value",
    );
}

#[test]
fn cg9_conflicting_or_malformed_values_are_refused() {
    expect_refused(
        &[
            ("MEMCORE_CONTEXT_CACHE_BACKEND", v("disabled")),
            ("MEMCORE_CONTEXT_CACHE_ENABLED", v("true")),
        ],
        "MEMCORE_CONTEXT_CACHE_ENABLED contradicts MEMCORE_CONTEXT_CACHE_BACKEND",
    );
    let output = expect_refused(
        &[
            ("MEMCORE_CONTEXT_CACHE_BACKEND", v("memory")),
            ("MEMCORE_ENV", v("development")),
            ("MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS", v(USER)),
        ],
        "MEMCORE_CONTEXT_CACHE_LOCAL_SINGLE_PROCESS must be one of",
    );
    assert!(!output.contains(USER), "CG14: {output}");
}
