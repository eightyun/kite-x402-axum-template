use serde::Serialize;

// FACILITATOR_URL verifies and settles payments on both Kite networks. This
// wrapper appends /verify and /settle, so the /v2 prefix must stay.
pub const FACILITATOR_URL: &str = "https://facilitator.pieverse.io/v2";

// KiteChain describes one Kite network the wrapper can charge on. Only the
// stablecoin settled by the facilitator is listed: the payer signs an EIP-3009
// transferWithAuthorization, so the asset must implement EIP-3009 and its
// EIP-712 name/version must match the token contract exactly.
pub struct KiteChain {
    pub network: &'static str, // CAIP-2 identifier
    pub asset: &'static str,   // stablecoin contract
    pub symbol: &'static str,
    pub decimals: u32,
    pub eip712_name: &'static str,
    pub eip712_version: &'static str,
}

// Kite mainnet: Bridged USDC (USDC.e), 6 decimals.
pub const MAINNET: KiteChain = KiteChain {
    network: "eip155:2366",
    asset: "0x7aB6f3ed87C42eF0aDb67Ed95090f8bF5240149e",
    symbol: "USDC.e",
    decimals: 6,
    eip712_name: "Bridged USDC (Kite AI)",
    eip712_version: "2",
};

// Kite testnet: pieUSD, 18 decimals. Passport sandbox agents pay with this.
pub const TESTNET: KiteChain = KiteChain {
    network: "eip155:2368",
    asset: "0x38129cf4CE5E183eFF248F42A7D345Bb1B47621A",
    symbol: "pieUSD",
    decimals: 18,
    eip712_name: "pieUSD",
    eip712_version: "1",
};

// Resolve the KITE_NETWORK environment value to its payment configuration.
pub fn chain_by_name(name: &str) -> Result<&'static KiteChain, String> {
    match name {
        "mainnet" | "" => Ok(&MAINNET),
        "testnet" => Ok(&TESTNET),
        _ => Err(format!(
            "unknown KITE_NETWORK {name:?} (want mainnet or testnet)"
        )),
    }
}

// The x402 v2 payment terms advertised in PAYMENT-REQUIRED and sent to the
// facilitator for verification and settlement.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequirements {
    pub scheme: &'static str,
    pub network: &'static str,
    pub amount: String,
    pub asset: &'static str,
    pub pay_to: String,
    pub max_timeout_seconds: u32,
    pub extra: PaymentExtra,
}

#[derive(Clone, Serialize)]
pub struct PaymentExtra {
    pub name: &'static str,
    pub version: &'static str,
}

pub fn requirements(
    chain: &'static KiteChain,
    pay_to: String,
    price: &str,
) -> Result<PaymentRequirements, String> {
    // Pin the Kite token address and EIP-712 domain instead of relying on a
    // default stablecoin table, which does not include these assets.
    Ok(PaymentRequirements {
        scheme: "exact",
        network: chain.network,
        amount: price_to_units(price, chain.decimals)?,
        asset: chain.asset,
        pay_to,
        max_timeout_seconds: 60,
        extra: PaymentExtra {
            name: chain.eip712_name,
            version: chain.eip712_version,
        },
    })
}

// Convert "$0.001"-style prices to integer token units. Decimal string math
// avoids floating-point rounding for both 6-decimal USDC.e and 18-decimal
// pieUSD; the public USD price is limited to six fractional digits.
pub fn price_to_units(price: &str, decimals: u32) -> Result<String, String> {
    let value = price.strip_prefix('$').unwrap_or(price);
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty()
        || value.ends_with('.')
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 6
        || fraction.len() > decimals as usize
    {
        return Err("PRICE_USD must be a positive decimal with at most 6 fractional digits".into());
    }
    let scale = 10_u128
        .checked_pow(decimals)
        .ok_or("PRICE_USD is too large")?;
    let whole = whole
        .parse::<u128>()
        .map_err(|_| "PRICE_USD is too large")?;
    let fractional = if fraction.is_empty() {
        0
    } else {
        fraction.parse::<u128>().map_err(|_| "invalid PRICE_USD")?
    };
    let fractional_scale = 10_u128
        .checked_pow(decimals - fraction.len() as u32)
        .ok_or("invalid PRICE_USD")?;
    let units = whole
        .checked_mul(scale)
        .and_then(|v| {
            fractional
                .checked_mul(fractional_scale)
                .and_then(|f| v.checked_add(f))
        })
        .ok_or("PRICE_USD is too large")?;
    if units == 0 {
        return Err("PRICE_USD must be greater than zero".into());
    }
    Ok(units.to_string())
}
