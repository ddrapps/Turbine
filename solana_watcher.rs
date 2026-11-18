use serde::{Deserialize, Serialize};
use std::fs;
use std::time::Duration;
use tokio::time::sleep;
use reqwest::Client;
use solana_sdk::signature::{Keypair, Signer};
use solana_client::rpc_client::RpcClient;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupportedAsset {
    pub chain: String,
    pub symbol: String,
    pub contract_address: String,
    pub decimals: u8,
}

pub struct SolanaWatcher {
    rpc_url: String,
    deposit_address: String,
    fee_wallet: Keypair,
    client: Client,
    supported: Vec<SupportedAsset>,
}

impl SolanaWatcher {
    pub fn new(rpc_url: &str, deposit_address: &str, fee_wallet_path: &str) -> Self {
        let fee_bytes = fs::read(fee_wallet_path).expect("fee_wallet.json not found");
        let fee_wallet = Keypair::from_bytes(&fee_bytes).expect("invalid fee wallet");

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
        ];

        Self {
            rpc_url: rpc_url.to_string(),
            deposit_address: deposit_address.to_string(),
            fee_wallet,
            client: Client::new(),
            supported,
        }
    }

    pub async fn run(&self) {
        println!("Solana watcher started. Watching: {}", self.deposit_address);
        let _rpc = RpcClient::new(&self.rpc_url);

        loop {
            println!("Checking for deposits...");
            // Real RPC polling goes here later
            sleep(Duration::from_secs(15)).await;
        }
    }
}

#[tokio::main]
async fn main() {
    let watcher = SolanaWatcher::new(
        "https://api.mainnet-beta.solana.com",
        "YOUR_SOLANA_DEPOSIT_ADDRESS",
        "fee_wallet.json",
    );
    watcher.run().await;
}