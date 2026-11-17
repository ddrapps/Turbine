use ed25519_dalek::{SigningKey, Signer};
use rand::rngs::ThreadRng;
use rand::Rng;
use hex;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::collections::HashMap;
use serde_json::Value;

fn load_or_create_key() -> SigningKey {
    if let Ok(bytes) = fs::read("wallet.key") {
        if bytes.len() == 32 {
            return SigningKey::from_bytes(bytes.as_slice().try_into().unwrap());
        }
    }
    let mut rng = ThreadRng::default();
    let mut secret = [0u8; 32];
    rng.fill_bytes(&mut secret);
    fs::write("wallet.key", &secret).unwrap();
    println!("Created new wallet.key");
    SigningKey::from_bytes(&secret)
}

fn load_config() -> String {
    if let Ok(content) = fs::read_to_string("wallet_config.txt") {
        for line in content.lines() {
            if let Some((k, v)) = line.split_once('=') {
                if k.trim() == "node" {
                    return v.trim().to_string();
                }
            }
        }
    }
    "127.0.0.1:7878".to_string()
}

fn load_address_book() -> HashMap<String, String> {
    let mut book = HashMap::new();
    if let Ok(content) = fs::read_to_string("address_book.txt") {
        for line in content.lines() {
            if let Some((name, addr)) = line.split_once('=') {
                book.insert(name.trim().to_string(), addr.trim().to_string());
            }
        }
    }
    book
}

fn resolve_address(input: &str, book: &HashMap<String, String>) -> String {
    book.get(input).cloned().unwrap_or_else(|| input.to_string())
}

fn connect(node: &str) -> Result<TcpStream, String> {
    TcpStream::connect(node).map_err(|e| format!("Could not connect to node at {}: {}", node, e))
}

fn main() {
    let signing_key = load_or_create_key();
    let verifying_key = signing_key.verifying_key();
    let pub_hex = hex::encode(verifying_key.as_bytes());
    let node = load_config();
    let address_book = load_address_book();

    println!("Your address: {}", pub_hex);

    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        println!("Usage:");
        println!("  cargo run --bin wallet -- balance");
        println!("  cargo run --bin wallet -- history");
        println!("  cargo run --bin wallet -- send <to> <amount>");
        println!("  cargo run --bin wallet -- addcontact <name> <address>");
        return;
    }

    match args[1].as_str() {
        "balance" => {
            match connect(&node) {
                Ok(mut stream) => {
                    if stream.write_all(b"GET_BALANCES\n").is_err() {
                        println!("Failed to send request");
                        return;
                    }
                    let mut reader = BufReader::new(stream);
                    let mut response = String::new();
                    if reader.read_line(&mut response).is_err() {
                        println!("Failed to read response");
                        return;
                    }
                    if let Ok(balances) = serde_json::from_str::<Value>(response.trim()) {
                        if let Some(bal) = balances.get(&pub_hex).and_then(|v| v.as_f64()) {
                            println!("Your balance: {}", bal);
                        } else {
                            println!("Your balance: 0.0");
                        }
                    } else {
                        println!("Could not parse balances");
                    }
                }
                Err(e) => println!("{}", e),
            }
        }
        "history" => {
            match connect(&node) {
                Ok(mut stream) => {
                    if stream.write_all(b"GET_CHAIN\n").is_err() {
                        println!("Failed to send request");
                        return;
                    }
                    let mut reader = BufReader::new(stream);
                    let mut response = String::new();
                    if reader.read_line(&mut response).is_err() {
                        println!("Failed to read response");
                        return;
                    }
                    if let Ok(chain) = serde_json::from_str::<Vec<Value>>(response.trim()) {
                        println!("Your transaction history:");
                        let mut found = false;
                        for block in chain {
                            if let Some(tx) = block.get("transaction") {
                                let from = tx.get("from").and_then(|v| v.as_str()).unwrap_or("");
                                let to = tx.get("to").and_then(|v| v.as_str()).unwrap_or("");
                                let volume = tx.get("volume").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                if from == pub_hex || to == pub_hex {
                                    found = true;
                                    let direction = if from == pub_hex { "Sent" } else { "Received" };
                                    println!("  {} {} -> {} ({})", direction, from, to, volume);
                                }
                            }
                        }
                        if !found {
                            println!("  (none)");
                        }
                    } else {
                        println!("Could not parse chain");
                    }
                }
                Err(e) => println!("{}", e),
            }
        }
        "send" => {
            if args.len() < 4 {
                println!("Usage: cargo run --bin wallet -- send <to> <amount>");
                return;
            }
            let to_input = &args[2];
            let to = resolve_address(to_input, &address_book);
            let amount = &args[3];
            let from = &pub_hex;

            match connect(&node) {
                Ok(mut stream) => {
                    if stream.write_all(b"GET_TIP\n").is_err() {
                        println!("Failed to get tip");
                        return;
                    }
                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                    let mut tip = String::new();
                    if reader.read_line(&mut tip).is_err() {
                        println!("Failed to read tip");
                        return;
                    }
                    let tip = tip.trim();

                    let msg = format!("{}{}{}", from, to, amount);
                    let signature = signing_key.sign(msg.as_bytes());
                    let sig_hex = hex::encode(signature.to_bytes());

                    let tx_line = format!("{}|{}|{}|{}|{}|{}\n", tip, from, to, amount, sig_hex, pub_hex);
                    match connect(&node) {
                        Ok(mut stream2) => {
                            if stream2.write_all(tx_line.as_bytes()).is_err() {
                                println!("Failed to send transaction");
                                return;
                            }
                            let mut reader2 = BufReader::new(stream2);
                            let mut response = String::new();
                            if reader2.read_line(&mut response).is_ok() {
                                println!("Response: {}", response.trim());
                            } else {
                                println!("No response from node");
                            }
                        }
                        Err(e) => println!("{}", e),
                    }
                }
                Err(e) => println!("{}", e),
            }
        }
        "addcontact" => {
            if args.len() < 4 {
                println!("Usage: cargo run --bin wallet -- addcontact <name> <address>");
                return;
            }
            let name = &args[2];
            let address = &args[3];
            let line = format!("{}={}\n", name, address);
            if fs::write("address_book.txt", 
                fs::read_to_string("address_book.txt").unwrap_or_default() + &line).is_ok() {
                println!("Added contact: {} = {}", name, address);
            } else {
                println!("Failed to save contact");
            }
        }
        _ => println!("Unknown command"),
    }
}