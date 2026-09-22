# Rust + Axum template

An x402 v2 reverse proxy for Kite. All methods under `/v1/*` require payment;
`/healthz` is free. A paid `/v1/forecast?x=1` request reaches
`UPSTREAM_URL/forecast?x=1`. For Open-Meteo, set
`UPSTREAM_URL=https://api.open-meteo.com/v1` so `/v1/forecast` reaches its
`/v1/forecast` endpoint. The wrapper verifies payment, calls the upstream,
and settles only when the upstream returns a status below 400.

```bash
cp .env.example .env         # set PAY_TO and UPSTREAM_URL
set -a && source .env && set +a
cargo run
curl -i 'http://localhost:8080/v1/forecast?latitude=52.52&longitude=13.41&current=temperature_2m'
# HTTP 402 with a base64 PAYMENT-REQUIRED header
```

For a self-contained local example, start the mock upstream in one terminal:

```bash
cargo run --example mock_upstream
curl -i http://127.0.0.1:3001/price   # 200 {"price":123}
curl -i http://127.0.0.1:3001/fail    # 500
```

Start the wrapper in a second terminal from this directory:

```bash
PAY_TO=0x1111111111111111111111111111111111111111 \
UPSTREAM_URL=http://127.0.0.1:3001 cargo run
curl -i http://127.0.0.1:8080/v1/price # 402 without payment
```

The example wallet is only for the unpaid local check. A paid request needs a
real wallet and valid Kite payment. The mock HTTP test below exercises
`verify → upstream → settle` and confirms that an upstream 500 is not settled.

Once published to crates.io, the binary can also be installed with
`cargo install kite-x402-axum-template` and run as `kite-x402-axum`.

The variables match the Go and TypeScript templates: `PAY_TO`, `KITE_NETWORK`
(`mainnet` or `testnet`), `UPSTREAM_URL`, `PRICE_USD`, `PORT`,
`SERVICE_DESCRIPTION`, `FACILITATOR_URL`, `UPSTREAM_AUTH_HEADER`, and
`UPSTREAM_AUTH_VALUE`. `FACILITATOR_URL` defaults to the Pieverse `/v2` base.
The default price is `$0.001`. Prices accept at most six decimal places.

`src/main.rs` holds configuration, x402 HTTP handling, and the proxy;
`src/kite.rs` holds Kite network constants and exact decimal conversion.
`examples/mock_upstream.rs` is the local API used in the example above.
`src/tests/mod.rs` keeps the price and mock HTTP flow tests separate from the
runtime code.
Payment verification and settlement are delegated to the facilitator. The
wrapper never handles a buyer's private key or an upstream credential in a
402 response.

```bash
cargo test paid_route_verifies_proxies_and_only_settles_success
cargo test kite_prices_have_exact_token_units
```

## Live payment verification

Deploy the wrapper to a public HTTPS origin with `KITE_NETWORK=testnet`, then
use a Kite-compatible x402 client funded with testnet pieUSD. A successful
request returns the upstream response with a base64 `PAYMENT-RESPONSE` header
containing the settlement transaction hash.

The implementation was verified against the Kite testnet facilitator. The
request returned HTTP 200 with `{"price":123}`, and the settlement succeeded on
`eip155:2368`:

`0x6b86702fdf7e1113a6abfcf1616cc05cd874fbbf4cf55894c5feac87cd81d1c0`

View the transaction on the [Kite testnet explorer](https://testnet.kitescan.ai/tx/0x6b86702fdf7e1113a6abfcf1616cc05cd874fbbf4cf55894c5feac87cd81d1c0).

## License

Apache-2.0. See [LICENSE](LICENSE).
