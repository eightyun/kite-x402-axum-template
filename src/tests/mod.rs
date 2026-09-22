use super::*;
use crate::kite::{price_to_units, TESTNET};
use std::sync::Mutex;

#[test]
fn kite_prices_have_exact_token_units() {
    assert_eq!(price_to_units("0.001", 6).unwrap(), "1000");
    assert_eq!(price_to_units("$0.001", 18).unwrap(), "1000000000000000");
    assert!(price_to_units("0.0000001", 6).is_err());
    assert!(price_to_units("0", 6).is_err());
    assert!(price_to_units("1.", 6).is_err());
    let testnet = requirements(
        &TESTNET,
        "0x1111111111111111111111111111111111111111".into(),
        "$0.001",
    )
    .unwrap();
    assert_eq!(testnet.network, "eip155:2368");
    assert_eq!(testnet.amount, "1000000000000000");
    assert_eq!(testnet.extra.name, "pieUSD");
    assert_eq!(testnet.extra.version, "1");
}

async fn start(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{address}")
}

#[tokio::test]
async fn paid_route_verifies_proxies_and_only_settles_success() {
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let verify_events = events.clone();
    let settle_events = events.clone();
    let facilitator = Router::new()
        .route("/v2/verify", axum::routing::post(move |Json(body): Json<Value>| {
            let events = verify_events.clone();
            async move {
                assert_eq!(body["x402Version"], 2);
                assert_eq!(body["paymentRequirements"]["network"], "eip155:2366");
                events.lock().unwrap().push("verify".into());
                Json(json!({"isValid": true}))
            }
        }))
        .route("/v2/settle", axum::routing::post(move || {
            let events = settle_events.clone();
            async move {
                events.lock().unwrap().push("settle".into());
                Json(json!({"success": true, "network": "eip155:2366", "transaction": "0x123", "payer": "0x456"}))
            }
        }));
    let facilitator_url = start(facilitator).await;
    let upstream_events = events.clone();
    let upstream = Router::new().fallback(move |request: Request| {
        let events = upstream_events.clone();
        async move {
            assert!(request.uri().path().starts_with("/api/"));
            assert_eq!(
                request.headers().get("authorization").unwrap(),
                "Bearer secret"
            );
            if request.uri().path().ends_with("/ok") {
                assert_eq!(request.uri().query(), Some("q=1"));
            }
            assert!(!request.headers().contains_key("payment-signature"));
            events.lock().unwrap().push("upstream".into());
            if request.uri().path().ends_with("/fail") {
                (StatusCode::INTERNAL_SERVER_ERROR, "failed")
            } else {
                (StatusCode::OK, "ok")
            }
        }
    });
    let upstream_url = start(upstream).await;
    let requirement = requirements(
        &kite::MAINNET,
        "0x1111111111111111111111111111111111111111".into(),
        "$0.001",
    )
    .unwrap();
    let config = Arc::new(Config {
        chain: &kite::MAINNET,
        price: "$0.001".into(),
        description: "test".into(),
        requirement: requirement.clone(),
        upstream: Url::parse(&format!("{upstream_url}/api")).unwrap(),
        facilitator: Url::parse(&format!("{facilitator_url}/v2")).unwrap(),
        upstream_auth_header: header::AUTHORIZATION,
        upstream_auth_value: Some(HeaderValue::from_static("Bearer secret")),
        client: reqwest::Client::new(),
    });
    let service_url = start(app(config)).await;
    let client = reqwest::Client::new();
    let unpaid = client
        .get(format!("{service_url}/v1/ok?q=1"))
        .send()
        .await
        .unwrap();
    assert_eq!(unpaid.status(), StatusCode::PAYMENT_REQUIRED);
    let required: Value = serde_json::from_slice(
        &STANDARD
            .decode(unpaid.headers().get("payment-required").unwrap())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(required["accepts"][0]["amount"], "1000");
    assert!(events.lock().unwrap().is_empty());

    let signature = STANDARD.encode(
        json!({"x402Version": 2, "accepted": requirement, "payload": {"signature": "test"}})
            .to_string(),
    );
    let paid = client
        .get(format!("{service_url}/v1/ok?q=1"))
        .header("payment-signature", &signature)
        .header("authorization", "Bearer buyer")
        .send()
        .await
        .unwrap();
    assert_eq!(paid.status(), StatusCode::OK);
    assert!(paid.headers().contains_key("payment-response"));
    assert_eq!(paid.text().await.unwrap(), "ok");
    assert_eq!(*events.lock().unwrap(), ["verify", "upstream", "settle"]);

    events.lock().unwrap().clear();
    let failed = client
        .get(format!("{service_url}/v1/fail"))
        .header("payment-signature", &signature)
        .send()
        .await
        .unwrap();
    assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!failed.headers().contains_key("payment-response"));
    assert_eq!(*events.lock().unwrap(), ["verify", "upstream"]);
}
