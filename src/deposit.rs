use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportedAsset {
    pub chain: String,
    pub symbol: String,
    pub contract_address: String, // real public address
    pub decimals: u8,
}

#[derive(Debug, Clone)]
pub struct DepositWatcher {
    pub supported: Vec<SupportedAsset>,
    // placeholder deposit addresses for testing (replace later)
    pub deposit_addresses: HashMap<String, String>,
}

impl DepositWatcher {
    pub fn new() -> Self {
        let supported = vec![
            SupportedAsset {
                chain: "solana".into(),
                symbol: "SOL".into(),
                contract_address: "native".into(),
                decimals: 9,
            },
            SupportedAsset {
                chain: "solana".into(),
                symbol: "USDC".into(),
                contract_address: "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v".into(),
                decimals: 6,
            },
            SupportedAsset {
                chain: "ethereum".into(),
                symbol: "ETH".into(),
                contract_address: "native".into(),
                decimals: 18,
            },
            SupportedAsset {
                chain: "ethereum".into(),
                symbol: "USDC".into(),
                contract_address: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".into(),
                decimals: 6,
            },
            SupportedAsset {
                chain: "base".into(),
                symbol: "USDC".into(),
                contract_address: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".into(),
                decimals: 6,
            },
        ];

        let mut deposit_addresses = HashMap::new();
        deposit_addresses.insert("solana".into(), "PLACEHOLDER_SOLANA_DEPOSIT_ADDRESS".into());
        deposit_addresses.insert("ethereum".into(), "0xPLACEHOLDER_ETH_DEPOSIT_ADDRESS".into());
        deposit_addresses.insert("base".into(), "0xPLACEHOLDER_BASE_DEPOSIT_ADDRESS".into());

        Self {
            supported,
            deposit_addresses,
        }
    }

    /// Simulate detecting a deposit (replace with real RPC polling later)
    pub fn simulate_deposit(&self, chain: &str, symbol: &str, amount: f64, user_address: &str) -> Option<DetectedDeposit> {
        let asset = self.supported.iter().find(|a| a.chain == chain && a.symbol == symbol)?;
        Some(DetectedDeposit {
            chain: chain.to_string(),
            symbol: symbol.to_string(),
            amount,
            user_address: user_address.to_string(),
            contract_address: asset.contract_address.clone(),
            sol_value_after_fee: amount * 0.99, // 1% fee mock
        })
    }
}

#[derive(Debug, Clone)]
pub struct DetectedDeposit {
    pub chain: String,
    pub symbol: String,
    pub amount: f64,
    pub user_address: String,
    pub contract_address: String,
    pub sol_value_after_fee: f64,
}