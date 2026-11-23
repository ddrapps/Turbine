use std::fs;
use std::time::Duration;
use tokio::time::sleep;
use solana_sdk::signature::{Keypair, Signer};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::RpcTransactionConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::transaction::{Transaction, VersionedTransaction};
use solana_sdk::instruction::{Instruction, AccountMeta};
use solana_transaction_status::UiTransactionEncoding;
use std::str::FromStr;
use std::sync::Mutex;
use reqwest::Client;
use serde_json::json;
use base64::Engine;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;

pub struct SolanaWatcher {
    rpc_url: String,
    deposit_address: String,
    fee_wallet: Keypair,
    trans_fee_wallet: Keypair,
    last_seen: Mutex<Option<String>>,
    processed: Mutex<HashSet<String>>,
    http: Client,
    mint_to_symbol: HashMap<String, String>,
}

impl SolanaWatcher {
    pub fn new(rpc_url: &str, deposit_address: &str, fee_wallet_json: &str, trans_fee_wallet_json: &str) -> Self {
        let fee_vec: Vec<u8> = serde_json::from_str(fee_wallet_json).expect("invalid fee_wallet json");
        let fee_wallet = Keypair::from_bytes(&fee_vec).expect("invalid fee wallet");

        let trans_fee_vec: Vec<u8> = serde_json::from_str(trans_fee_wallet_json).expect("invalid trans_fee json");
        let trans_fee_wallet = Keypair::from_bytes(&trans_fee_vec).expect("invalid trans fee wallet");

        let processed = if let Ok(data) = fs::read_to_string("processed_sigs.txt") {
            data.lines().filter(|l| !l.is_empty()).map(|s| s.to_string()).collect()
        } else {
            HashSet::new()
        };

        let mint_to_symbol = Self::load_mint_map();

        Self {
            rpc_url: rpc_url.to_string(),
            deposit_address: deposit_address.to_string(),
            fee_wallet,
            trans_fee_wallet,
            last_seen: Mutex::new(None),
            processed: Mutex::new(processed),
            http: Client::new(),
            mint_to_symbol,
        }
    }

    fn load_mint_map() -> HashMap<String, String> {
        let mut map = HashMap::new();
        if let Ok(data) = fs::read_to_string("mint.json") {
            if let Ok(list) = serde_json::from_str::<Vec<serde_json::Value>>(&data) {
                for item in list {
                    if let (Some(mint), Some(symbol)) = (
                        item.get("mint").and_then(|v| v.as_str()),
                        item.get("symbol").and_then(|v| v.as_str()),
                    ) {
                        map.insert(mint.to_string(), symbol.to_string());
                    }
                }
            }
        }
        println!("Loaded {} mints from mint.json", map.len());
        map
    }

    pub async fn run(&self) {
        println!("Solana watcher started. Watching: {}", self.deposit_address);
        println!("Fee address (swap):     {}", self.fee_wallet.pubkey());
        println!("Trans fee address (SOL): {}", self.trans_fee_wallet.pubkey());
        let rpc = RpcClient::new(&self.rpc_url);

        if let Ok(pubkey) = Pubkey::from_str(&self.deposit_address) {
            if let Ok(signatures) = rpc.get_signatures_for_address(&pubkey) {
                let mut processed = self.processed.lock().unwrap();
                let mut newly_marked = 0;
                for sig in signatures.iter().take(5) {
                    if processed.insert(sig.signature.clone()) {
                        newly_marked += 1;
                    }
                }
                if newly_marked > 0 {
                    let _ = fs::write(
                        "processed_sigs.txt",
                        processed.iter().cloned().collect::<Vec<_>>().join("\n"),
                    );
                    println!("Startup: marked {} recent signatures as seen", newly_marked);
                } else {
                    println!("Startup: recent signatures already marked as seen");
                }
            }
        }

        loop {
            if let Err(e) = self.check_for_deposits(&rpc).await {
                eprintln!("Watcher error: {}", e);
            }
            sleep(Duration::from_secs(20)).await;
        }
    }

    async fn check_for_deposits(&self, rpc: &RpcClient) -> Result<(), Box<dyn std::error::Error>> {
        let pubkey = Pubkey::from_str(&self.deposit_address)?;
        let signatures = rpc.get_signatures_for_address(&pubkey)?;

        let mut last_seen = self.last_seen.lock().unwrap();

        for sig in signatures.iter().take(5) {
            let signature = &sig.signature;

            {
                let processed = self.processed.lock().unwrap();
                if processed.contains(signature) {
                    continue;
                }
            }

            if let Some(ref last) = *last_seen {
                if signature == last {
                    break;
                }
            }

            println!("Found signature: {}", signature);

            let mut success = false;
            if let Some((symbol, amount, mint)) = self.detect_deposit(rpc, signature).await? {
                println!("Deposit detected: {} {}", amount, symbol);
                if let Err(e) = self.handle_deposit(&symbol, amount, &mint).await {
                    eprintln!("handle_deposit failed: {}", e);
                } else {
                    success = true;
                }
            }

            let mut processed = self.processed.lock().unwrap();
            if success {
                processed.insert(signature.clone());
                let _ = fs::write("processed_sigs.txt", processed.iter().cloned().collect::<Vec<_>>().join("\n"));
                println!("Processed successfully: {}", signature);
            } else {
                processed.insert(signature.clone());
            }
        }

        if let Some(newest) = signatures.first() {
            *last_seen = Some(newest.signature.clone());
        }

        Ok(())
    }

    async fn handle_deposit(&self, symbol: &str, amount: f64, mint: &str) -> Result<(), Box<dyn std::error::Error>> {
        let usdc_amount = if symbol == "USDC" {
            amount
        } else {
            self.jupiter_swap(symbol, amount, mint).await?
        };

        println!("=== SWAP FUNCTION RETURNED ===");
        println!("USDC amount from quote: {:.6}", usdc_amount);

        let fee_amount = usdc_amount * 0.02;
        let after_fee = usdc_amount * 0.98;
        println!("Would credit {:.6} CRUX to user (2% fee kept)", after_fee);

        if fee_amount > 0.000001 {
            println!("About to send 2% fee...");
            if let Err(e) = self.send_usdc_fee(fee_amount).await {
                eprintln!("Failed to send 2% fee: {}", e);
            } else {
                println!("Sent {:.6} USDC fee to AQ5TGaA65xYxHMxB7NFW9mDjtn8tFDx4VeF8Qa5hdHhz", fee_amount);
            }
        }

        let user_address = "a22f25d76ff6671502763f5fb6c0e6b234767e11323cc718912ceaae6c9cc6a7";
        if credit_on_chain(user_address, after_fee) {
            println!("Credited {:.6} CRUX to {} on chain", after_fee, user_address);
        } else {
            println!("Failed to credit on chain");
        }

        Ok(())
    }

    async fn send_usdc_fee(&self, amount: f64) -> Result<(), Box<dyn std::error::Error>> {
        let dest = "AQ5TGaA65xYxHMxB7NFW9mDjtn8tFDx4VeF8Qa5hdHhz";
        let usdc_mint = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
        let token_program = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
        let associated_token_program = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

        let amount_raw = (amount * 1_000_000.0) as u64;

        let rpc = RpcClient::new(&self.rpc_url);
        let dest_pubkey = Pubkey::from_str(dest)?;
        let mint_pubkey = Pubkey::from_str(usdc_mint)?;
        let token_program_id = Pubkey::from_str(token_program)?;
        let ata_program_id = Pubkey::from_str(associated_token_program)?;

        let source_ata = Pubkey::find_program_address(
            &[
                self.fee_wallet.pubkey().as_ref(),
                token_program_id.as_ref(),
                mint_pubkey.as_ref(),
            ],
            &ata_program_id,
        ).0;

        let dest_ata = Pubkey::find_program_address(
            &[
                dest_pubkey.as_ref(),
                token_program_id.as_ref(),
                mint_pubkey.as_ref(),
            ],
            &ata_program_id,
        ).0;

        let mut data = vec![3u8];
        data.extend_from_slice(&amount_raw.to_le_bytes());

        let transfer_ix = Instruction {
            program_id: token_program_id,
            accounts: vec![
                AccountMeta::new(source_ata, false),
                AccountMeta::new(dest_ata, false),
                AccountMeta::new_readonly(self.fee_wallet.pubkey(), true),
            ],
            data,
        };

        let recent = rpc.get_latest_blockhash()?;
        let mut tx = Transaction::new_with_payer(&[transfer_ix], Some(&self.trans_fee_wallet.pubkey()));
        tx.sign(&[&self.fee_wallet, &self.trans_fee_wallet], recent);

        let sig = rpc.send_transaction(&tx)?;
        println!("Fee transfer sent: {}", sig);
        Ok(())
    }

    async fn jupiter_swap(&self, symbol: &str, amount: f64, input_mint: &str) -> Result<f64, Box<dyn std::error::Error>> {
        let decimals = if symbol == "SOL" { 9 } else { 6 };
        let raw_amount = (amount * 10f64.powi(decimals)) as u64;
        let quote_url = format!(
            "https://api.jup.ag/swap/v1/quote?inputMint={}&outputMint=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v&amount={}&slippageBps=50",
            input_mint, raw_amount
        );

        let quote: serde_json::Value = self.http.get(&quote_url).send().await?.json().await?;
        let out_amount = quote["outAmount"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0) / 1_000_000.0;
        println!("Jupiter quote: {} {} → {:.6} USDC", amount, symbol, out_amount);

        let swap_body = json!({
            "quoteResponse": quote,
            "userPublicKey": self.fee_wallet.pubkey().to_string(),
            "payer": self.trans_fee_wallet.pubkey().to_string(),
            "wrapAndUnwrapSol": true,
        });

        let swap_resp: serde_json::Value = self.http
            .post("https://api.jup.ag/swap/v1/swap")
            .json(&swap_body)
            .send()
            .await?
            .json()
            .await?;

        if let Some(tx_b64) = swap_resp["swapTransaction"].as_str() {
            let tx_bytes = base64::engine::general_purpose::STANDARD.decode(tx_b64)?;
            let mut tx: VersionedTransaction = bincode::deserialize(&tx_bytes)?;

            let message_data = tx.message.serialize();
            let fee_sig = self.fee_wallet.sign_message(&message_data);
            let trans_fee_sig = self.trans_fee_wallet.sign_message(&message_data);

            let account_keys = tx.message.static_account_keys();
            let num_signers = tx.message.header().num_required_signatures as usize;

            for i in 0..num_signers.min(account_keys.len()) {
                let key = account_keys[i];
                if key == self.fee_wallet.pubkey() {
                    tx.signatures[i] = fee_sig;
                } else if key == self.trans_fee_wallet.pubkey() {
                    tx.signatures[i] = trans_fee_sig;
                }
            }

            let rpc = RpcClient::new(&self.rpc_url);
            println!("About to call send_transaction...");
            let sig = rpc.send_transaction(&tx)?;
            println!("Swap sent: {}", sig);

            // Immediately mark the swap signature so it is not treated as a new deposit
            {
                let mut processed = self.processed.lock().unwrap();
                processed.insert(sig.to_string());
                let _ = fs::write("processed_sigs.txt", processed.iter().cloned().collect::<Vec<_>>().join("\n"));
            }

            println!("Waiting for confirmation...");
            sleep(Duration::from_secs(5)).await;

            let confirmed = rpc.confirm_transaction(&sig).unwrap_or(false);
            if confirmed {
                println!("Swap confirmed: {}", sig);
            } else {
                println!("Swap confirmation timed out (may still have succeeded)");
            }

            let usdc_mint = Pubkey::from_str("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v")?;
            let token_program = Pubkey::from_str("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA")?;
            let ata_program = Pubkey::from_str("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL")?;
            let ata = Pubkey::find_program_address(
                &[
                    self.fee_wallet.pubkey().as_ref(),
                    token_program.as_ref(),
                    usdc_mint.as_ref(),
                ],
                &ata_program,
            ).0;

            if let Ok(balance) = rpc.get_token_account_balance(&ata) {
                let actual_usdc = balance.ui_amount.unwrap_or(0.0);
                println!("USDC balance after swap: {:.6} USDC", actual_usdc);
            }
            println!("Leaving jupiter_swap now");
        } else {
            println!("No swapTransaction in Jupiter response");
        }

        println!("jupiter_swap returning Ok");
        Ok(out_amount)
    }

    async fn detect_deposit(
        &self,
        rpc: &RpcClient,
        signature: &str,
    ) -> Result<Option<(String, f64, String)>, Box<dyn std::error::Error>> {
        let sig = signature.parse()?;
        let config = RpcTransactionConfig {
            encoding: Some(UiTransactionEncoding::JsonParsed),
            max_supported_transaction_version: Some(0),
            ..Default::default()
        };
        let tx = rpc.get_transaction_with_config(&sig, config)?;

        if let Some(meta) = tx.transaction.meta {
            let pre = &meta.pre_balances;
            let post = &meta.post_balances;

            let account_keys: Vec<String> = match &tx.transaction.transaction {
                solana_transaction_status::EncodedTransaction::Json(ui_tx) => {
                    match &ui_tx.message {
                        solana_transaction_status::UiMessage::Parsed(parsed) => {
                            parsed.account_keys.iter().map(|k| k.pubkey.clone()).collect()
                        }
                        solana_transaction_status::UiMessage::Raw(raw) => raw.account_keys.clone(),
                    }
                }
                _ => vec![],
            };

            if !account_keys.is_empty() && pre.len() == post.len() && pre.len() == account_keys.len() {
                for (i, key) in account_keys.iter().enumerate() {
                    if key == &self.deposit_address && post[i] > pre[i] {
                        let lamports = post[i] - pre[i];
                        let sol = lamports as f64 / 1_000_000_000.0;
                        if sol > 0.000001 {
                            println!("SOL increase detected: {} SOL", sol);
                            return Ok(Some(("SOL".to_string(), sol, "So11111111111111111111111111111111111111112".to_string())));
                        }
                    }
                }
            }

            let pre_tokens = match &meta.pre_token_balances {
                solana_transaction_status::option_serializer::OptionSerializer::Some(v) => v,
                _ => return Ok(None),
            };
            let post_tokens = match &meta.post_token_balances {
                solana_transaction_status::option_serializer::OptionSerializer::Some(v) => v,
                _ => return Ok(None),
            };

            let mut pre_map: HashMap<(String, String), f64> = HashMap::new();
            for tb in pre_tokens {
                let owner = match &tb.owner {
                    solana_transaction_status::option_serializer::OptionSerializer::Some(o) => o,
                    _ => continue,
                };
                if owner == &self.deposit_address {
                    let amt = tb.ui_token_amount.ui_amount.unwrap_or(0.0);
                    pre_map.insert((tb.mint.clone(), owner.clone()), amt);
                }
            }
            for tb in post_tokens {
                let owner = match &tb.owner {
                    solana_transaction_status::option_serializer::OptionSerializer::Some(o) => o,
                    _ => continue,
                };
                if owner == &self.deposit_address {
                    let post_amt = tb.ui_token_amount.ui_amount.unwrap_or(0.0);
                    let pre_amt = pre_map.get(&(tb.mint.clone(), owner.clone())).copied().unwrap_or(0.0);
                    let delta = post_amt - pre_amt;
                    if delta > 0.000001 {
                        let symbol = self.mint_to_symbol.get(&tb.mint).cloned().unwrap_or_else(|| "UNKNOWN".to_string());
                        println!("Token increase detected: {} {} (mint: {})", delta, symbol, tb.mint);
                        if symbol != "UNKNOWN" {
                            return Ok(Some((symbol, delta, tb.mint.clone())));
                        }
                    }
                }
            }
        }

        Ok(None)
    }
}

fn credit_on_chain(user_address: &str, amount: f64) -> bool {
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

#[tokio::main]
async fn main() {
    let rpc_url = std::env::var("HELIUS_RPC_URL").expect("HELIUS_RPC_URL not set");
    let fee_wallet_json = std::env::var("FEE_WALLET_KEY").expect("FEE_WALLET_KEY not set");
    let trans_fee_wallet_json = std::env::var("TRANS_FEE").expect("TRANS_FEE not set");

    let watcher = SolanaWatcher::new(
        &rpc_url,
        "DGzDWaxZBWar1qgATna4o1Cjvn4wKWLHveZ3mDcbNg5a",
        &fee_wallet_json,
        &trans_fee_wallet_json,
    );
    watcher.run().await;
}
