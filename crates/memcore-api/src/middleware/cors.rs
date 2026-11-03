use axum::http::{HeaderName, HeaderValue, Method};
use memcore_config::Settings;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer, ExposeHeaders};

/// Builds a CORS layer from settings when `cors_enabled` is true.
pub fn build_cors_layer(settings: &Settings) -> Option<CorsLayer> {
    if !settings.cors_enabled {
        return None;
    }

    let mut layer = CorsLayer::new()
        .allow_methods(parse_methods(&settings.cors_allowed_methods))
        .allow_headers(parse_headers(&settings.cors_allowed_headers));

    if !settings.cors_exposed_headers.is_empty() {
        layer = layer.expose_headers(parse_expose_headers(&settings.cors_exposed_headers));
    }

    layer = layer.allow_origin(parse_origins(&settings.cors_allowed_origins));
    layer = layer.allow_credentials(settings.cors_allow_credentials);

    Some(layer)
}

fn parse_origins(origins: &[String]) -> AllowOrigin {
    if origins.iter().any(|origin| origin.trim() == "*") {
        return AllowOrigin::any();
    }

    let values = origins
        .iter()
        .filter_map(|origin| HeaderValue::from_str(origin.trim()).ok())
        .collect::<Vec<_>>();
    AllowOrigin::list(values)
}

fn parse_methods(methods: &[String]) -> AllowMethods {
    let values = methods
        .iter()
        .filter_map(|method| Method::from_bytes(method.trim().as_bytes()).ok())
        .collect::<Vec<_>>();
    AllowMethods::list(values)
}

fn parse_headers(headers: &[String]) -> AllowHeaders {
    let values = headers
        .iter()
        .filter_map(|header| HeaderName::from_bytes(header.trim().as_bytes()).ok())
        .collect::<Vec<_>>();
    AllowHeaders::list(values)
}

fn parse_expose_headers(headers: &[String]) -> ExposeHeaders {
    let values = headers
        .iter()
        .filter_map(|header| HeaderName::from_bytes(header.trim().as_bytes()).ok())
        .collect::<Vec<_>>();
    ExposeHeaders::list(values)
}
