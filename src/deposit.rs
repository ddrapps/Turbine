use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportedAsset {
    pub chain: String,
    pub symbol: String,
    pub contract_address: String,
    pub decimals: u8,
}

#[derive(Debug, Clone)]
pub struct DepositWatcher {
    pub supported: Vec<SupportedAsset>,
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

    pub fn simulate_deposit(&self, chain: &str, symbol: &str, amount: f64, user_address: &str) -> Option<DetectedDeposit> {
        let asset = self.supported.iter().find(|a| a.chain == chain && a.symbol == symbol)?;
        Some(DetectedDeposit {
            chain: chain.to_string(),
            symbol: symbol.to_string(),
            amount,
            user_address: user_address.to_string(),
            contract_address: asset.contract_address.clone(),
            sol_value_after_fee: amount * 0.99,
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

/// Single shared function that credits a user on the chain.
/// Used by both the deposit watcher and the tracker CLI.
/// Uses the privileged "deposit" sender (no signature required).
pub fn credit_on_chain(user_address: &str, amount: f64) -> bool {
    let tip = {
        let mut stream = match TcpStream::connect("127.0.0.1:7878") {
            Ok(s) => s,
            Err(_) => return false,
        };
        if stream.write_all(b"GET_TIP\n").is_err() {
            return false;
        }
        let mut reader = BufReader::new(stream);
        let mut tip = String::new();
        if reader.read_line(&mut tip).is_err() {
            return false;
        }
        tip.trim().to_string()
    };
    if tip.is_empty() {
        return false;
    }

    let mut stream = match TcpStream::connect("127.0.0.1:7878") {
        Ok(s) => s,
        Err(_) => return false,
    };
    let tx_line = format!("{}|deposit|{}|{}||\n", tip, user_address, amount);
    if stream.write_all(tx_line.as_bytes()).is_err() {
        return false;
    }

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    if reader.read_line(&mut response).is_err() {
        return false;
    }
    response.trim() == "OK"
}

/// Convenience wrapper that uses the fee-adjusted amount from a DetectedDeposit.
pub fn credit_deposit(deposit: &DetectedDeposit) -> bool {
    credit_on_chain(&deposit.user_address, deposit.sol_value_after_fee)
}