use serde::{Serialize, Deserialize};
use sha2::{Sha256, Digest};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
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
        block.index, block.transaction.from, block.transaction.to,
        block.transaction.volume, block.previous_hash);
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{:02x}", b)).collect()
}

fn verify_signature(tx: &Transaction) -> bool {
    if tx.from == "genesis" || tx.from == "premine" || tx.from == "deposit" || tx.from == "cashout" {
        return true;
    }
    if tx.signature.is_empty() || tx.public_key.is_empty() { return false; }
    let public_key_bytes = hex::decode(&tx.public_key).unwrap_or_default();
    let sig_bytes = hex::decode(&tx.signature).unwrap_or_default();
    if public_key_bytes.len() != 32 || sig_bytes.len() != 64 { return false; }
    let verifying_key = match VerifyingKey::from_bytes(public_key_bytes.as_slice().try_into().unwrap_or(&[0u8; 32])) {
        Ok(k) => k,
        Err(_) => return false,
    };
    let signature = Signature::from_bytes(sig_bytes.as_slice().try_into().unwrap_or(&[0u8; 64]));
    let msg = format!("{}{}{}", tx.from, tx.to, tx.volume);
    verifying_key.verify(msg.as_bytes(), &signature).is_ok()
}

fn calculate_balances(chain: &[Block]) -> HashMap<String, f64> {
    let mut balances = HashMap::new();
    for block in chain {
        let tx = &block.transaction;
        if tx.from == "cashout" {
            *balances.entry(tx.to.clone()).or_insert(0.0) -= tx.volume;
        } else {
            if tx.from != "genesis" && tx.from != "premine" && tx.from != "deposit" {
                *balances.entry(tx.from.clone()).or_insert(0.0) -= tx.volume;
            }
            *balances.entry(tx.to.clone()).or_insert(0.0) += tx.volume;
        }
    }
    balances
}

fn save_chain(chain: &[Block]) {
    let json = serde_json::to_string_pretty(chain).unwrap();
    fs::write("/data/chain.json", json).unwrap();
    println!("Chain saved");
}

fn load_chain() -> Vec<Block> {
    let content = fs::read_to_string("/data/chain.json").unwrap_or_default();
    serde_json::from_str(&content).unwrap_or_default()
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
    let args: Vec<String> = env::args().collect();
    let port = env::var("PORT")
        .ok()
        .or_else(|| args.get(1).cloned())
        .unwrap_or_else(|| "7878".to_string());
    let addr = format!("0.0.0.0:{}", port);

    let mut chain = load_chain();
    if chain.is_empty() {
        chain.push(Block {
            index: 1,
            transaction: Transaction {
                from: "genesis".into(), to: "CRUX".into(), volume: 0.0,
                signature: String::new(), public_key: String::new(),
            },
            previous_hash: "0".into(),
        });
        save_chain(&chain);
        println!("Created empty genesis for CRUX");
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

            if msg.starts_with("GET /") {
                handle_http(&mut stream, &chain);
                continue;
            }

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
                                from: parts[0].into(), to: parts[1].into(), volume,
                                signature: parts[3].into(), public_key: parts[4].into(),
                            };
                            if !verify_signature(&tx) {
                                println!("Rejected - bad signature");
                                let _ = stream.write_all(b"REJECTED\n");
                                continue;
                            }
                            let balances = calculate_balances(&chain);
                            let sender_balance = balances.get(&tx.from).copied().unwrap_or(0.0);
                            if tx.from != "genesis" && tx.from != "premine" && tx.from != "deposit" && tx.from != "cashout" && sender_balance < volume {
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
