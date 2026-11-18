use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::time::Duration;
use tokio::time::sleep;
use reqwest::Client;
use solana_sdk::signature::{Keypair, Signer};
use solana_client::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

// Re-use the shared credit function from deposit.rs
mod deposit;
use deposit::{credit_on_chain, DetectedDeposit};

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
            // Add more SPL tokens here as needed (NVDAX, etc.)
        ];

        Self {
            rpc_url: rpc_url.to_string(),
            deposit_address: deposit_address.to_string(),
            fee_wallet,
            client: Client::new(),
            supported,
        }
    }

    /// Polls for new deposits and processes them.
    pub async fn run(&self) {
        println!("Solana watcher started. Watching: {}", self.deposit_address);
        let rpc = RpcClient::new(&self.rpc_url);

        loop {
            if let Err(e) = self.check_for_deposits(&rpc).await {
                eprintln!("Watcher error: {}", e);
            }
            sleep(Duration::from_secs(15)).await;
        }
    }

    async fn check_for_deposits(&self, rpc: &RpcClient) -> Result<(), Box<dyn std::error::Error>> {
        // Placeholder: In production, use getSignaturesForAddress + getTransaction
        // to detect incoming transfers to self.deposit_address.
        // For now this is the structure ready for the real RPC calls.

        // Example simulation path (remove later):
        // if let Some(deposit) = self.simulate_incoming("SOL", 0.5, "user_chain_address_here") {
        //     self.process_deposit(deposit).await?;
        // }

        Ok(())
    }

    /// Swap non-USDC token to USDC via Jupiter, then credit.
    async fn process_deposit(&self, deposit: DetectedDeposit) -> Result<(), Box<dyn std::error::Error>> {
        let usdc_amount = if deposit.symbol == "USDC" {
            deposit.amount
        } else {
            self.swap_to_usdc(&deposit).await?
        };

        let after_fee = usdc_amount * 0.99;
        let credited = DetectedDeposit {
            usdc_value_after_fee: after_fee,
            ..deposit
        };

        if credit_on_chain(&credited.user_address, credited.usdc_value_after_fee) {
            println!("Credited {:.6} to {}", credited.usdc_value_after_fee, credited.user_address);
        } else {
            eprintln!("Failed to credit on chain");
        }

        Ok(())
    }

    /// Call Jupiter API for quote + swap transaction, sign with fee wallet, and send.
    async fn swap_to_usdc(&self, deposit: &DetectedDeposit) -> Result<f64, Box<dyn std::error::Error>> {
        // Jupiter quote endpoint (free)
        let input_mint = if deposit.contract_address == "native" {
            "So11111111111111111111111111111111111111112" // wrapped SOL
        } else {
            &deposit.contract_address
        };

        let quote_url = format!(
            "https://quote-api.jup.ag/v6/quote?inputMint={}&outputMint=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v&amount={}&slippageBps=50",
            input_mint,
            (deposit.amount * 10f64.powi(deposit.contract_address.len() as i32)).round() as u64 // rough; use proper decimals later
        );

        let quote: serde_json::Value = self.client.get(&quote_url).send().await?.json().await?;

        // Next: build swap transaction, sign with self.fee_wallet, send via RPC.
        // This is the structure; full signing code comes in the next iteration once we test the quote.

        let out_amount = quote["outAmount"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0) / 1_000_000.0;
        println!("Jupiter quote: {} {} → ~{} USDC", deposit.amount, deposit.symbol, out_amount);

        Ok(out_amount)
    }
}

// Helper for testing without real RPC
impl SolanaWatcher {
    fn simulate_incoming(&self, symbol: &str, amount: f64, user: &str) -> Option<DetectedDeposit> {
        let asset = self.supported.iter().find(|a| a.symbol == symbol)?;
        Some(DetectedDeposit {
            chain: "solana".into(),
            symbol: symbol.to_string(),
            amount,
            user_address: user.to_string(),
            contract_address: asset.contract_address.clone(),
            usdc_value_after_fee: 0.0,
        })
    }
}
