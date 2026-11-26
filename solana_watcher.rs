use std::fs;
use std::time::Duration;
use tokio::time::sleep;
use solana_sdk::signature::{Keypair, Signer};
use solana_client::rpc_client::RpcClient;
use solana_client::rpc_config::RpcTransactionConfig;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::transaction::{Transaction, VersionedTransaction};
use solana_sdk::instruction::{Instruction, AccountMeta};
use solana_sdk::system_instruction;
use solana_transaction_status::UiTransactionEncoding;
use std::str::FromStr;
use std::sync::Mutex;
use reqwest::Client;
use serde_json::json;
use base64::Engine;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use futures_util::{StreamExt, SinkExt};

pub struct SolanaWatcher {
    rpc_url: String,
    ws_url: String,
    deposit_address: String,
    fee_wallet: Keypair,
    last_seen: Mutex<Option<String>>,
    processed: Mutex<HashSet<String>>,
    http: Client,
    mint_to_symbol: HashMap<String, String>,
    pending_credits: Mutex<HashMap<String, String>>,
}

impl SolanaWatcher {
    pub fn new(rpc_url: &str, ws_url: &str, deposit_address: &str, fee_wallet_key: &str) -> Self {
        let fee_vec: Vec<u8> = serde_json::from_str(fee_wallet_key).expect("invalid FEE_WALLET_KEY json");
        let fee_wallet = Keypair::from_bytes(&fee_vec).expect("invalid fee wallet");

        let processed = if let Ok(data) = fs::read_to_string("/data/processed_sigs.txt") {
            data.lines().filter(|l| !l.is_empty()).map(|s| s.to_string()).collect()
        } else {
            HashSet::new()
        };

        let mint_to_symbol = Self::load_mint_map();

        Self {
            rpc_url: rpc_url.to_string(),
            ws_url: ws_url.to_string(),
            deposit_address: deposit_address.to_string(),
            fee_wallet,
            last_seen: Mutex::new(None),
            processed: Mutex::new(processed),
            http: Client::new(),
            mint_to_symbol,
            pending_credits: Mutex::new(HashMap::new()),
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

    async fn load_user_solana_keys(&self) -> HashMap<String, String> {
        let base = std::env::var("WEBWALLET_URL").unwrap_or_else(|_| "http://localhost:3000".to_string());
        let secret = std::env::var("KEYS_SECRET").unwrap_or_default();
        let url = format!("{}/solana-keys?secret={}", base, secret);
        match self.http.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<HashMap<String, String>>().await {
                    Ok(map) => map,
                    Err(_) => HashMap::new(),
                }
            }
            _ => HashMap::new(),
        }
    }

    pub async fn run(self: Arc<Self>) {
        println!("Solana watcher started. Watching: {}", self.deposit_address);
        println!("Fee address (swap + fees): {}", self.fee_wallet.pubkey());
        let rpc = RpcClient::new(&self.rpc_url);

        // Spawn the WebSocket deposit listener
        let watcher_ws = Arc::clone(&self);
        tokio::spawn(async move {
            loop {
                if let Err(e) = watcher_ws.listen_deposits().await {
                    eprintln!("WebSocket listener error: {}", e);
                }
                eprintln!("WebSocket disconnected, reconnecting in 5s...");
                sleep(Duration::from_secs(5)).await;
            }
        });

        // Keep the user deposit check on a short poll (less frequent traffic)
        loop {
            if let Err(e) = self.check_user_deposits(&rpc).await {
                eprintln!("User deposit check error: {}", e);
            }
            sleep(Duration::from_secs(15)).await;
        }
    }

    async fn listen_deposits(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let (ws_stream, _) = connect_async(&self.ws_url).await?;
        let (mut write, mut read) = ws_stream.split();

        let pubkey = Pubkey::from_str(&self.deposit_address)?;
        let sub_msg = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "logsSubscribe",
            "params": [
                { "mentions": [pubkey.to_string()] },
                { "commitment": "confirmed" }
            ]
        });
        write.send(Message::Text(sub_msg.to_string())).await?;

        let rpc = RpcClient::new(&self.rpc_url);

        while let Some(msg) = read.next().await {
            let msg = msg?;
            if let Message::Text(text) = msg {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(sig) = val["params"]["result"]["value"]["signature"].as_str() {
                        let signature = sig.to_string();
                        {
                            let processed = self.processed.lock().unwrap();
                            if processed.contains(&signature) {
                                continue;
                            }
                        }
                        println!("WebSocket found signature: {}", signature);
                        // Retry a couple of times — very new txs can return null briefly
                        let mut detected = None;
                        for attempt in 0..3 {
                            match self.detect_deposit(&rpc, &signature).await {
                                Ok(Some(d)) => {
                                    detected = Some(d);
                                    break;
                                }
                                Ok(None) => {
                                    if attempt < 2 {
                                        sleep(Duration::from_millis(800)).await;
                                    }
                                }
                                Err(e) => {
                                    eprintln!("detect_deposit error (continuing): {}", e);
                                    break;
                                }
                            }
                        }
                        if let Some((symbol, amount, mint)) = detected {
                            println!("Deposit detected: {} {}", amount, symbol);
                            let user_email = self.pending_credits.lock().unwrap().remove(&signature);
                            let watcher = Arc::new(self.clone_for_spawn());
                            let sig_clone = signature.clone();
                            tokio::spawn(async move {
                                if let Err(e) = watcher.handle_deposit(&symbol, amount, &mint, user_email.as_deref()).await {
                                    eprintln!("handle_deposit failed for {}: {}", sig_clone, e);
                                } else {
                                    let mut processed = watcher.processed.lock().unwrap();
                                    processed.insert(sig_clone.clone());
                                    let _ = fs::write("/data/processed_sigs.txt", processed.iter().cloned().collect::<Vec<_>>().join("\n"));
                                    println!("Processed successfully: {}", sig_clone);
                                }
                            });
                        } else {
                            let mut processed = self.processed.lock().unwrap();
                            processed.insert(signature);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    // Helper so we can spawn without full Arc clone issues in the listener
    fn clone_for_spawn(&self) -> SolanaWatcher {
        SolanaWatcher {
            rpc_url: self.rpc_url.clone(),
            ws_url: self.ws_url.clone(),
            deposit_address: self.deposit_address.clone(),
            fee_wallet: Keypair::from_bytes(&self.fee_wallet.to_bytes()).expect("clone key"),
            last_seen: Mutex::new(None),
            processed: Mutex::new(self.processed.lock().unwrap().clone()),
            http: Client::new(),
            mint_to_symbol: self.mint_to_symbol.clone(),
            pending_credits: Mutex::new(HashMap::new()),
        }
    }

    async fn check_user_deposits(&self, rpc: &RpcClient) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let user_keys = self.load_user_solana_keys().await;
        if user_keys.is_empty() {
            return Ok(());
        }

        for (email, priv_base58) in user_keys {
            let keypair = match bs58::decode(&priv_base58).into_vec() {
                Ok(bytes) if bytes.len() == 64 => Keypair::from_bytes(&bytes).ok(),
                _ => None,
            };
            let Some(keypair) = keypair else { continue };
            let user_pubkey = keypair.pubkey();

            let signatures = match rpc.get_signatures_for_address(&user_pubkey) {
                Ok(s) => s,
                Err(_) => continue,
            };

            for sig in signatures.iter().take(3) {
                let signature = &sig.signature;
                {
                    let processed = self.processed.lock().unwrap();
                    if processed.contains(signature) {
                        continue;
                    }
                }

                if let Ok(Some((symbol, amount, _))) = self.detect_deposit_for_address(rpc, signature, &user_pubkey.to_string()).await {
                    if symbol == "SOL" && amount > 0.000001 {
                        println!("User deposit detected for {}: {} SOL", email, amount);
                        match self.forward_to_fee_add(rpc, &keypair, amount).await {
                            Ok(forward_sig) => {
                                let mut processed = self.processed.lock().unwrap();
                                processed.insert(signature.clone());
                                let _ = fs::write("/data/processed_sigs.txt", processed.iter().cloned().collect::<Vec<_>>().join("\n"));
                                self.pending_credits.lock().unwrap().insert(forward_sig, email.clone());
                                println!("Forwarded {} SOL from {} to FEE_ADD", amount, email);
                            }
                            Err(e) => {
                                eprintln!("Forward failed for {}: {}", email, e);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn forward_to_fee_add(&self, rpc: &RpcClient, user_keypair: &Keypair, amount_sol: f64) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let fee_pubkey = Pubkey::from_str(&self.deposit_address)?;
        let lamports = (amount_sol * 1_000_000_000.0) as u64;
        let send_lamports = lamports.saturating_sub(5_000);
        if send_lamports == 0 {
            return Err("amount too small after fee reserve".into());
        }

        let ix = system_instruction::transfer(&user_keypair.pubkey(), &fee_pubkey, send_lamports);
        let recent_blockhash = rpc.get_latest_blockhash()?;
        let tx = Transaction::new_signed_with_payer(
            &[ix],
            Some(&user_keypair.pubkey()),
            &[user_keypair],
            recent_blockhash,
        );
        let sig = rpc.send_and_confirm_transaction(&tx)?;
        let sig_str = sig.to_string();
        println!("Forward tx: {}", sig_str);
        Ok(sig_str)
    }

    async fn detect_deposit_for_address(
        &self,
        rpc: &RpcClient,
        signature: &str,
        address: &str,
    ) -> Result<Option<(String, f64, String)>, Box<dyn std::error::Error + Send + Sync>> {
        let sig = match signature.parse() {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };
        let config = RpcTransactionConfig {
            encoding: Some(UiTransactionEncoding::JsonParsed),
            max_supported_transaction_version: Some(0),
            ..Default::default()
        };

        // Transaction may not be fully available yet — treat null/error as "not ready"
        let tx = match rpc.get_transaction_with_config(&sig, config) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("detect_deposit_for_address: tx not ready for {}: {}", signature, e);
                return Ok(None);
            }
        };

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
                    if key == address && post[i] > pre[i] {
                        let lamports = post[i] - pre[i];
                        let sol = lamports as f64 / 1_000_000_000.0;
                        if sol > 0.000001 {
                            return Ok(Some(("SOL".to_string(), sol, "So11111111111111111111111111111111111111112".to_string())));
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    async fn handle_deposit(&self, symbol: &str, amount: f64, mint: &str, user_email: Option<&str>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let usdc_amount = if symbol == "USDC" {
            amount
        } else {
            let swap_amount = amount * 0.998;
            println!("Swapping 99.8% ({:.6} {}) — leaving 0.2% for fees", swap_amount, symbol);
            self.jupiter_swap(symbol, swap_amount, mint).await?
        };

        println!("=== SWAP FUNCTION RETURNED ===");
        println!("USDC amount from quote: {:.6}", usdc_amount);

        let credit_value = usdc_amount;
        println!("Applying {:.6} shesaysyes.ca credits from deposit", credit_value);

        if let Some(email) = user_email {
            let backend_url = std::env::var("BACKEND_URL").unwrap_or_else(|_| "http://localhost:8000".to_string());
            let body = json!({
                "user_id": email,
                "usdc_amount": credit_value
            });
            match self.http.post(format!("{}/internal/credit-deposit", backend_url))
                .json(&body)
                .send()
                .await
            {
                Ok(resp) => {
                    if resp.status().is_success() {
                        println!("shesaysyes.ca credits applied for {}: {:.6}", email, credit_value);
                    } else {
                        eprintln!("Credit endpoint returned error for {}", email);
                    }
                }
                Err(e) => eprintln!("Failed to call credit endpoint for {}: {}", email, e),
            }
        } else {
            println!("shesaysyes.ca credits applied: {:.6} (no specific user matched)", credit_value);
        }

        Ok(())
    }

    async fn jupiter_swap(&self, symbol: &str, amount: f64, input_mint: &str) -> Result<f64, Box<dyn std::error::Error + Send + Sync>> {
        let decimals = if symbol == "SOL" { 9 } else { 6 };
        let raw_amount = (amount * 10f64.powi(decimals)) as u64;
        let quote_url = format!(
            "https://api.jup.ag/swap/v1/quote?inputMint={}&outputMint=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v&amount={}&slippageBps=50",
            input_mint, raw_amount
        );

        let quote: serde_json::Value = self.http.get(&quote_url).send().await?.json().await?;
        let out_amount = quote["outAmount"].as_str().unwrap_or("0").parse::<f64>().unwrap_or(0.0) / 1_000_000.0;
        println!("Jupiter quote: {} {} → {:.6} USDC", amount, symbol, out_amount);

        let fee_pubkey = self.fee_wallet.pubkey().to_string();
        let swap_body = json!({
            "quoteResponse": quote,
            "userPublicKey": fee_pubkey,
            "payer": fee_pubkey,
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

            let account_keys = tx.message.static_account_keys();
            let num_signers = tx.message.header().num_required_signatures as usize;

            for i in 0..num_signers.min(account_keys.len()) {
                let key = account_keys[i];
                if key == self.fee_wallet.pubkey() {
                    tx.signatures[i] = fee_sig;
                }
            }

            let rpc = RpcClient::new(&self.rpc_url);
            println!("About to call send_transaction...");
            let sig = rpc.send_transaction(&tx)?;
            println!("Swap sent: {}", sig);

            {
                let mut processed = self.processed.lock().unwrap();
                processed.insert(sig.to_string());
                let _ = fs::write("/data/processed_sigs.txt", processed.iter().cloned().collect::<Vec<_>>().join("\n"));
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

                if let Ok(payment_add) = std::env::var("PAYMENT_ADD") {
                    if let Ok(payment_pubkey) = Pubkey::from_str(&payment_add) {
                        let recipient_ata = Pubkey::find_program_address(
                            &[
                                payment_pubkey.as_ref(),
                                token_program.as_ref(),
                                usdc_mint.as_ref(),
                            ],
                            &ata_program,
                        ).0;

                        let raw_amount = (actual_usdc * 1_000_000.0) as u64;
                        if raw_amount > 0 {
                            let mut data = vec![3u8];
                            data.extend_from_slice(&raw_amount.to_le_bytes());

                            let transfer_ix = Instruction {
                                program_id: token_program,
                                accounts: vec![
                                    AccountMeta::new(ata, false),
                                    AccountMeta::new(recipient_ata, false),
                                    AccountMeta::new_readonly(self.fee_wallet.pubkey(), true),
                                ],
                                data,
                            };

                            let recent_blockhash = rpc.get_latest_blockhash()?;
                            let transfer_tx = Transaction::new_signed_with_payer(
                                &[transfer_ix],
                                Some(&self.fee_wallet.pubkey()),
                                &[&self.fee_wallet],
                                recent_blockhash,
                            );

                            match rpc.send_and_confirm_transaction(&transfer_tx) {
                                Ok(sig) => println!("USDC sent to PAYMENT_ADD: {}", sig),
                                Err(e) => eprintln!("Failed to send USDC to PAYMENT_ADD: {}", e),
                            }
                        }
                    }
                }
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
    ) -> Result<Option<(String, f64, String)>, Box<dyn std::error::Error + Send + Sync>> {
        let sig = match signature.parse() {
            Ok(s) => s,
            Err(_) => return Ok(None),
        };
        let config = RpcTransactionConfig {
            encoding: Some(UiTransactionEncoding::JsonParsed),
            max_supported_transaction_version: Some(0),
            ..Default::default()
        };

        // Transaction may not be fully available yet — treat null/error as "not ready"
        let tx = match rpc.get_transaction_with_config(&sig, config) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("detect_deposit: tx not ready for {}: {}", signature, e);
                return Ok(None);
            }
        };

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

#[tokio::main]
async fn main() {
    let rpc_url = std::env::var("HELIUS_RPC_URL").expect("HELIUS_RPC_URL not set");
    let ws_url = std::env::var("HELIUS_WEBSOCKET").expect("HELIUS_WEBSOCKET not set");
    let fee_wallet_key = std::env::var("FEE_WALLET_KEY").expect("FEE_WALLET_KEY not set");
    let deposit_address = std::env::var("FEE_ADD").expect("FEE_ADD not set");

    let watcher = Arc::new(SolanaWatcher::new(
        &rpc_url,
        &ws_url,
        &deposit_address,
        &fee_wallet_key,
    ));

    let port = std::env::var("PORT").unwrap_or_else(|_| "7879".to_string());
    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).expect("failed to bind watcher port");
    println!("Watcher listening on {}", addr);

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(mut stream) = stream {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                let _ = reader.read_line(&mut line);
                let reply = if line.trim() == "PING" { "PONG\n" } else { "OK\n" };
                let _ = stream.write_all(reply.as_bytes());
            }
        }
    });

    watcher.run().await;
}
