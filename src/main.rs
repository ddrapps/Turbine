use serde::{Serialize, Deserialize};
use sha2::{Sha256, Digest};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use chrono::Utc;
use ed25519_dalek::{VerifyingKey, Signature, Verifier};
use hex;

#[derive(Serialize, Deserialize, Clone)]
struct Transaction {
    from: String,
    to: String,
    volume: f64,
    signature: String,
    public_key: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct Block {
    index: u64,
    transaction: Transaction,
    previous_hash: String,
}

fn calculate_hash(block: &Block) -> String {
    let input = format!("{}{}{}{}{}",
        block.index,
        block.transaction.from,
        block.transaction.to,
        block.transaction.volume,
        block.previous_hash);
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{:02x}", b)).collect()
}

fn verify_signature(tx: &Transaction) -> bool {
    // Special privileged from-addresses are allowed without a signature
    if tx.from == "genesis" || tx.from == "str327" || tx.from == "premine" || tx.from == "deposit" {
        return true;
    }
    if tx.signature.is_empty() || tx.public_key.is_empty() {
        return false;
    }
    let public_key_bytes = hex::decode(&tx.public_key).unwrap_or_default();
    let sig_bytes = hex::decode(&tx.signature).unwrap_or_default();
    if public_key_bytes.len() != 32 || sig_bytes.len() != 64 {
        return false;
    }
    let verifying_key = match VerifyingKey::from_bytes(public_key_bytes.as_slice().try_into().unwrap_or(&[0u8; 32])) {
        Ok(k) => k,
        Err(_) => return false,
    };
    let signature = Signature::from_bytes(
    sig_bytes.as_slice().try_into().unwrap_or(&[0u8; 64])
);
    let msg = format!("{}{}{}", tx.from, tx.to, tx.volume);
    verifying_key.verify(msg.as_bytes(), &signature).is_ok()
}

fn calculate_balances(chain: &[Block]) -> HashMap<String, f64> {
    let mut balances = HashMap::new();
    for block in chain {
        let tx = &block.transaction;
        // Privileged from-addresses do not decrease a balance
        if tx.from != "genesis" && tx.from != "str327" && tx.from != "premine" && tx.from != "deposit" {
            *balances.entry(tx.from.clone()).or_insert(0.0) -= tx.volume;
        }
        *balances.entry(tx.to.clone()).or_insert(0.0) += tx.volume;
    }
    balances
}

fn save_chain(chain: &[Block]) {
    let json = serde_json::to_string_pretty(chain).unwrap();
    fs::write("chain.json", json).unwrap();
    println!("Chain saved");
}

fn load_chain() -> Vec<Block> {
    let content = fs::read_to_string("chain.json").unwrap_or_default();
    serde_json::from_str(&content).unwrap_or_default()
}

fn load_str_env() -> (f64, String, f64) {
    let content = fs::read_to_string("str.env").unwrap_or_default();
    let mut starting_balance = 100.0;
    let mut str327_address = "kevin".to_string();
    let mut daily_str327 = 0.01;

    for line in content.lines() {
        if let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "STARTING_BALANCE" => {
                    if let Ok(v) = value.trim().parse::<f64>() {
                        starting_balance = v;
                    }
                }
                "STR327_ADDRESS" => str327_address = value.trim().to_string(),
                "DAILY_STR327" => {
                    if let Ok(v) = value.trim().parse::<f64>() {
                        daily_str327 = v;
                    }
                }
                _ => {}
            }
        }
    }
    (starting_balance, str327_address, daily_str327)
}

fn handle_http(stream: &mut std::net::TcpStream, chain: &[Block]) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    let _ = reader.read_line(&mut request_line);

    let response = if request_line.contains("GET /balance") {
        let balances = calculate_balances(chain);
        let body = serde_json::to_string_pretty(&balances).unwrap();
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\n\r\n{}", body)
    } else if request_line.contains("GET /chain") {
        let body = serde_json::to_string_pretty(chain).unwrap();
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\n\r\n{}", body)
    } else {
        "HTTP/1.1 404 Not Found\r\n\r\n".to_string()
    };

    let _ = stream.write_all(response.as_bytes());
}

fn main() {
    let (starting_balance, str327_address, daily_str327) = load_str_env();

    let args: Vec<String> = env::args().collect();
    let port = args.get(1).unwrap_or(&"7878".to_string()).clone();
    let addr = format!("127.0.0.1:{}", port);

    let mut chain = load_chain();
    if chain.is_empty() {
        chain.push(Block {
            index: 1,
            transaction: Transaction {
                from: "genesis".into(),
                to: str327_address.clone(),
                volume: starting_balance,
                signature: String::new(),
                public_key: String::new(),
            },
            previous_hash: "0".into(),
        });
        save_chain(&chain);
        println!("Created genesis with {} to {}", starting_balance, str327_address);
    }

    let today = Utc::now().format("%Y-%m-%d").to_string();
    let str327_file = "str327.txt";
    let last_str327 = fs::read_to_string(str327_file).unwrap_or_default();
    if last_str327.trim() != today {
        let tip = chain.last().unwrap();
        let prev = calculate_hash(tip);
        chain.push(Block {
            index: tip.index + 1,
            transaction: Transaction {
                from: "str327".into(),
                to: str327_address.clone(),
                volume: daily_str327,
                signature: String::new(),
                public_key: String::new(),
            },
            previous_hash: prev,
        });
        save_chain(&chain);
        fs::write(str327_file, &today).unwrap();
        println!("Daily str327 of {} given to {} for {}", daily_str327, str327_address, today);
    }

    println!("Node ready. Blocks: {}", chain.len());
    let tip = chain.last().unwrap();
    println!("Current tip: {}", calculate_hash(tip));

    let balances = calculate_balances(&chain);
    println!("Balances: {:?}", balances);

    let listener = TcpListener::bind(&addr).unwrap();
    println!("Listening on {}", addr);

    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).is_ok() {
            let msg = line.trim();
            println!("Received: {}", msg);

            // HTTP requests
            if msg.starts_with("GET /") {
                handle_http(&mut stream, &chain);
                continue;
            }

            // Existing TCP commands
            if msg == "GET_TIP" {
                let tip = chain.last().unwrap();
                let tip_hash = calculate_hash(tip);
                let _ = stream.write_all(format!("{}\n", tip_hash).as_bytes());
            } else if msg == "GET_CHAIN" {
                let json = serde_json::to_string(&chain).unwrap();
                let _ = stream.write_all(json.as_bytes());
                let _ = stream.write_all(b"\n");
            } else if msg == "GET_BALANCES" {
                let balances = calculate_balances(&chain);
                let json = serde_json::to_string(&balances).unwrap();
                let _ = stream.write_all(json.as_bytes());
                let _ = stream.write_all(b"\n");
            } else if let Some((prev, rest)) = msg.split_once('|') {
                let parts: Vec<&str> = rest.split('|').collect();
                if parts.len() == 5 {
                    if let Ok(volume) = parts[2].parse::<f64>() {
                        let tip = chain.last().unwrap();
                        let real_prev = calculate_hash(tip);
                        if prev == real_prev {
                            let tx = Transaction {
                                from: parts[0].into(),
                                to: parts[1].into(),
                                volume,
                                signature: parts[3].into(),
                                public_key: parts[4].into(),
                            };
                            if !verify_signature(&tx) {
                                println!("Rejected - bad signature");
                                let _ = stream.write_all(b"REJECTED\n");
                                continue;
                            }
                            let balances = calculate_balances(&chain);
                            let sender_balance = balances.get(&tx.from).copied().unwrap_or(0.0);
                            // Privileged from-addresses skip the balance check
                            if tx.from != "genesis" && tx.from != "str327" && tx.from != "premine" && tx.from != "deposit" && sender_balance < volume {
                                println!("Rejected - insufficient balance");
                                let _ = stream.write_all(b"REJECTED\n");
                                continue;
                            }
                            chain.push(Block {
                                index: tip.index + 1,
                                transaction: tx,
                                previous_hash: real_prev,
                            });
                            println!("Accepted block #{}", chain.len());
                            save_chain(&chain);
                            let _ = stream.write_all(b"OK\n");
                        } else {
                            println!("Rejected - bad previous hash");
                            let _ = stream.write_all(b"REJECTED\n");
                        }
                    }
                }
            }
        }
    }
}
