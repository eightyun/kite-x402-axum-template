//! Kite x402 service template (Rust + Axum).
//!
//! Wraps an existing HTTP API behind x402 payments settled on the Kite chain.
//! Requests to /v1/* return HTTP 402 until the caller attaches a valid
//! PAYMENT-SIGNATURE; the facilitator verifies the payment, the request is
//! proxied to UPSTREAM_URL, and payment is settled only if the upstream
//! answered with a non-error status.
mod kite;

#[cfg(test)]
mod tests;

use std::{collections::HashSet, env, sync::Arc, time::Duration};

use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use kite::{chain_by_name, requirements, KiteChain, PaymentRequirements, FACILITATOR_URL};
use serde::Serialize;
use serde_json::{json, Value};
use url::Url;

type AppState = Arc<Config>;

struct Config {
    chain: &'static KiteChain,
    price: String,
    description: String,
    requirement: PaymentRequirements,
    upstream: Url,
    facilitator: Url,
    upstream_auth_header: HeaderName,
    upstream_auth_value: Option<HeaderValue>,
    client: reqwest::Client,
}

fn env_or(key: &str, fallback: &str) -> String {
    let value = env::var(key).unwrap_or_default();
    let value = value.trim();
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value.to_owned()
    }
}

impl Config {
    fn from_env() -> Result<Self, String> {
        // Both Kite networks use the same environment variables as the Go and
        // TypeScript templates. Reject invalid payment and upstream settings
        // before accepting requests.
        let pay_to = env_or("PAY_TO", "");
        if pay_to.len() != 42
            || !pay_to.starts_with("0x")
            || !pay_to[2..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("PAY_TO must be a 0x-prefixed 20-byte Kite wallet address".into());
        }
        let chain = chain_by_name(&env_or("KITE_NETWORK", "mainnet"))?;
        let upstream = parse_http_url(&env_or("UPSTREAM_URL", ""), "UPSTREAM_URL")?;
        let facilitator = parse_http_url(
            &env_or("FACILITATOR_URL", FACILITATOR_URL),
            "FACILITATOR_URL",
        )?;
        let price = env_or("PRICE_USD", "0.001");
        let price = if price.starts_with('$') {
            price
        } else {
            format!("${price}")
        };
        // Kite stablecoins are not built-in pricing assets. Build the exact
        // token amount and EIP-712 domain from the selected network.
        let requirement = requirements(chain, pay_to, &price)?;
        let upstream_auth_header = env_or("UPSTREAM_AUTH_HEADER", "Authorization")
            .parse::<HeaderName>()
            .map_err(|_| "invalid UPSTREAM_AUTH_HEADER")?;
        let upstream_auth_value = match env_or("UPSTREAM_AUTH_VALUE", "") {
            value if value.is_empty() => None,
            value => Some(
                value
                    .parse::<HeaderValue>()
                    .map_err(|_| "invalid UPSTREAM_AUTH_VALUE")?,
            ),
        };
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            chain,
            price,
            description: env_or(
                "SERVICE_DESCRIPTION",
                "Paid API wrapped for the Kite network",
            ),
            requirement,
            upstream,
            facilitator,
            upstream_auth_header,
            upstream_auth_value,
            client,
        })
    }
}

fn parse_http_url(value: &str, key: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| format!("{key} must be an absolute http(s) URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(format!(
            "{key} must be an absolute http(s) URL without query or fragment"
        ));
    }
    Ok(url)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Arc::new(Config::from_env()?);
    let port: u16 = env_or("PORT", "8080").parse()?;
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await?;
    eprintln!(
        "kite x402 service on :{port} -> {} (network {}, {} per call to {})",
        config.upstream, config.chain.network, config.price, config.requirement.pay_to
    );
    axum::serve(listener, app(config)).await?;
    Ok(())
}

fn app(config: AppState) -> Router {
    // 1. Which routes cost money. Every method under /v1/ is paid;
    //    /healthz stays free so load balancers can probe the service.
    Router::new()
        .route("/healthz", get(healthz))
        .fallback(paid_route)
        .with_state(config)
}

async fn healthz(State(config): State<AppState>) -> impl IntoResponse {
    Json(
        json!({"ok": true, "network": config.chain.network, "asset": config.chain.symbol, "price": config.price}),
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Resource<'a> {
    url: String,
    description: &'a str,
    mime_type: &'static str,
}

fn challenge(config: &Config, request_url: String, reason: &str) -> Response {
    // An unpaid request receives the same x402 v2 payment terms in the JSON
    // body and the base64-encoded PAYMENT-REQUIRED header.
    let required = json!({
        "x402Version": 2,
        "error": reason,
        "resource": Resource { url: request_url, description: &config.description, mime_type: "application/json" },
        "accepts": [&config.requirement],
    });
    let encoded = STANDARD.encode(required.to_string());
    let mut response = (StatusCode::PAYMENT_REQUIRED, Json(required)).into_response();
    response.headers_mut().insert(
        "payment-required",
        HeaderValue::from_str(&encoded).expect("base64 header"),
    );
    response
}

fn request_url(request: &Request) -> String {
    // Public HTTPS hosts usually terminate TLS before forwarding to Axum.
    // Preserve that public scheme in the resource URL advertised to buyers.
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    let protocol = if request
        .headers()
        .get("x-forwarded-proto")
        .and_then(|h| h.to_str().ok())
        == Some("https")
    {
        "https"
    } else {
        "http"
    };
    format!("{protocol}://{host}{}", request.uri())
}

async fn paid_route(State(config): State<AppState>, request: Request) -> Response {
    if !request.uri().path().starts_with("/v1/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let url = request_url(&request);
    let Some(signature) = request
        .headers()
        .get("payment-signature")
        .and_then(|h| h.to_str().ok())
    else {
        return challenge(&config, url, "PAYMENT-SIGNATURE header is required");
    };
    let Ok(decoded) = STANDARD.decode(signature) else {
        return challenge(&config, url, "invalid PAYMENT-SIGNATURE encoding");
    };
    let Ok(payload) = serde_json::from_slice::<Value>(&decoded) else {
        return challenge(&config, url, "invalid PAYMENT-SIGNATURE JSON");
    };
    // The buyer must accept this route's exact network, asset, amount, payee,
    // and EIP-712 domain. Send server-defined paymentRequirements to the
    // facilitator rather than trusting the buyer's accepted terms.
    let expected = serde_json::to_value(&config.requirement).expect("serializable requirements");
    if payload.get("x402Version") != Some(&json!(2))
        || payload.get("accepted") != Some(&expected)
        || !payload.get("payload").is_some_and(Value::is_object)
    {
        return challenge(&config, url, "payment does not match this route");
    }
    let payment =
        json!({"x402Version": 2, "paymentPayload": payload, "paymentRequirements": expected});
    // 2. Verify the signed authorization with the facilitator before the
    //    upstream is called. Verification alone does not transfer funds.
    match facilitator_call(&config, "verify", &payment).await {
        Ok(result) if result.get("isValid") == Some(&Value::Bool(true)) => {}
        Ok(_) => return challenge(&config, url, "payment verification failed"),
        Err(_) => {
            return (StatusCode::BAD_GATEWAY, "payment facilitator unavailable").into_response()
        }
    }

    // 3. Forward only verified requests. A failed upstream call is not charged.
    let upstream_response = match proxy(&config, request).await {
        Ok(response) => response,
        Err(_) => return (StatusCode::BAD_GATEWAY, "upstream unreachable").into_response(),
    };
    if upstream_response.status().as_u16() >= 400 {
        return upstream_into_response(upstream_response, None);
    }
    // 4. Settle only after the upstream returns a status below 400. Do not
    //    return the successful body unless settlement also succeeds.
    let settlement = match facilitator_call(&config, "settle", &payment).await {
        Ok(result) if result.get("success") == Some(&Value::Bool(true)) => result,
        _ => return (StatusCode::BAD_GATEWAY, "payment settlement failed").into_response(),
    };
    // The buyer receives the facilitator receipt in PAYMENT-RESPONSE.
    let payment_response = STANDARD.encode(settlement.to_string());
    upstream_into_response(upstream_response, Some(payment_response))
}

async fn facilitator_call(
    config: &Config,
    endpoint: &str,
    payment: &Value,
) -> Result<Value, reqwest::Error> {
    let url = format!(
        "{}/{}",
        config.facilitator.as_str().trim_end_matches('/'),
        endpoint
    );
    config
        .client
        .post(url)
        .json(payment)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
}

fn hop_headers(headers: &HeaderMap) -> HashSet<HeaderName> {
    // Hop-by-hop headers are connection-specific. Incoming payment headers
    // must not reach the upstream; upstream payment headers must not reach buyers.
    let mut excluded: HashSet<HeaderName> = [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "host",
        "content-length",
        "payment-signature",
        "payment-required",
        "payment-response",
    ]
    .into_iter()
    .map(HeaderName::from_static)
    .collect();
    // Connection may nominate additional hop-by-hop headers dynamically.
    for value in headers.get_all(header::CONNECTION).iter() {
        if let Ok(value) = value.to_str() {
            for token in value.split(',') {
                if let Ok(name) = token.trim().parse() {
                    excluded.insert(name);
                }
            }
        }
    }
    excluded
}

async fn proxy(config: &Config, request: Request) -> Result<reqwest::Response, reqwest::Error> {
    // Strip /v1 so /v1/forecast reaches UPSTREAM_URL/forecast. Keep the query
    // string and stream request bodies for methods that can carry one.
    let (parts, body) = request.into_parts();
    let mut target = config.upstream.clone();
    let suffix = parts.uri.path().strip_prefix("/v1").expect("paid route");
    let base = config.upstream.path().trim_end_matches('/');
    target.set_path(&format!("{base}{suffix}"));
    target.set_query(parts.uri.query());
    let excluded = hop_headers(&parts.headers);
    let has_body = parts.method != Method::GET && parts.method != Method::HEAD;
    let mut upstream = config.client.request(parts.method, target);
    for (name, value) in &parts.headers {
        if !excluded.contains(name) && name != config.upstream_auth_header {
            upstream = upstream.header(name, value);
        }
    }
    if let Some(value) = &config.upstream_auth_value {
        // Inject the provider credential only into the upstream request. It
        // never appears in a 402 challenge or the buyer's response.
        upstream = upstream.header(&config.upstream_auth_header, value);
    }
    if has_body {
        upstream
            .body(reqwest::Body::wrap_stream(body.into_data_stream()))
            .send()
            .await
    } else {
        upstream.send().await
    }
}

fn upstream_into_response(
    upstream: reqwest::Response,
    payment_response: Option<String>,
) -> Response {
    // Stream the upstream body back after filtering connection-specific and
    // upstream-supplied payment headers; only our receipt may be attached.
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let excluded = hop_headers(&headers);
    let mut response = Response::new(Body::from_stream(upstream.bytes_stream()));
    *response.status_mut() = status;
    for (name, value) in &headers {
        if !excluded.contains(name) {
            response.headers_mut().append(name, value.clone());
        }
    }
    if let Some(value) = payment_response {
        response.headers_mut().insert(
            "payment-response",
            HeaderValue::from_str(&value).expect("base64 header"),
        );
    }
    response
}
