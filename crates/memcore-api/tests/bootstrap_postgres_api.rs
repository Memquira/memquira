use memcore_api::AppState;
use memcore_config::{EventBackend, FactBackend, Settings};

#[tokio::test]
async fn missing_postgres_url_fails_when_fact_backend_postgres() {
    let settings = Settings {
        fact_backend: FactBackend::Postgres,
        event_backend: EventBackend::Postgres,
        postgres_url: None,
        ..Settings::default()
    };

    let error = match AppState::initialize(settings).await {
        Ok(_) => panic!("postgres backend without url should fail"),
        Err(error) => error,
    };

    assert_eq!(error.code(), "validation_error");
    assert!(
        error
            .to_string()
            .contains("MEMCORE_POSTGRES_URL is required when MEMCORE_FACT_BACKEND=postgres")
    );
}

#[tokio::test]
async fn missing_postgres_url_fails_when_event_backend_postgres() {
    let settings = Settings {
        fact_backend: FactBackend::Mock,
        event_backend: EventBackend::Postgres,
        postgres_url: None,
        ..Settings::default()
    };

    let error = match AppState::initialize(settings).await {
        Ok(_) => panic!("postgres event backend without url should fail"),
        Err(error) => error,
    };

    assert_eq!(error.code(), "validation_error");
    assert!(
        error
            .to_string()
            .contains("MEMCORE_POSTGRES_URL is required when MEMCORE_EVENT_BACKEND=postgres")
    );
}

#[tokio::test]
async fn postgres_backends_attempt_connection_when_url_present() {
    let settings = Settings {
        fact_backend: FactBackend::Postgres,
        event_backend: EventBackend::Postgres,
        postgres_url: Some("postgres://invalid-host:5432/memcore_test".to_string()),
        ..Settings::default()
    };

    let error = match AppState::initialize(settings).await {
        Ok(_) => panic!("unreachable postgres host should not initialize"),
        Err(error) => error,
    };

    assert_eq!(error.code(), "storage_error");
    assert!(
        error
            .to_string()
            .contains("failed to connect postgres database")
    );
}

#[tokio::test]
#[ignore = "requires running Postgres; set MEMCORE_TEST_POSTGRES_URL to run"]
async fn postgres_fact_and_event_backends_start_with_real_database() {
    let postgres_url = std::env::var("MEMCORE_TEST_POSTGRES_URL")
        .expect("MEMCORE_TEST_POSTGRES_URL must be set for this test");

    let settings = Settings {
        fact_backend: FactBackend::Postgres,
        event_backend: EventBackend::Postgres,
        postgres_url: Some(postgres_url),
        ..Settings::default()
    };

    AppState::initialize(settings)
        .await
        .expect("postgres fact and event backends should initialize with test database");
}
