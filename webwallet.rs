use ed25519_dalek::{SigningKey, Signer};
use hex;
use rand::rngs::ThreadRng;
use rand::{Rng, RngExt};
use serde_json;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use solana_sdk::signature::{Keypair, Signer as SolanaSigner};

fn load_users() -> HashMap<String, String> {
    match fs::read_to_string("/data/users.json") {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

fn save_users(users: &HashMap<String, String>) {
    if let Ok(json) = serde_json::to_string_pretty(users) {
        let _ = fs::write("/data/users.json", json);
    }
}

fn load_solana_keys() -> HashMap<String, String> {
    match fs::read_to_string("/data/solana_keys.json") {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => HashMap::new(),
    }
}

fn save_solana_keys(keys: &HashMap<String, String>) {
    if let Ok(json) = serde_json::to_string_pretty(keys) {
        let _ = fs::write("/data/solana_keys.json", json);
    }
}

fn derive_address(private_key_hex: &str) -> Option<String> {
    let bytes = hex::decode(private_key_hex.trim()).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&bytes);
    let key = SigningKey::from_bytes(&key_bytes);
    Some(hex::encode(key.verifying_key().as_bytes()))
}

fn generate_new_key() -> (String, String) {
    let mut rng = ThreadRng::default();
    let mut secret = [0u8; 32];
    rng.fill_bytes(&mut secret);
    let key = SigningKey::from_bytes(&secret);
    let priv_hex = hex::encode(secret);
    let addr = hex::encode(key.verifying_key().as_bytes());
    (priv_hex, addr)
}

fn generate_pin() -> String {
    let mut rng = ThreadRng::default();
    format!("{:06}", rng.random_range(0..1_000_000))
}

fn send_pin_email(to: &str, pin: &str) -> Result<(), Box<dyn std::error::Error>> {
    let smtp_user = std::env::var("SMTP_USER")?;
    let smtp_pass = std::env::var("SMTP_PASS")?;
    let smtp_from = std::env::var("SMTP_FROM").unwrap_or_else(|_| smtp_user.clone());

    let email = Message::builder()
        .from(smtp_from.parse()?)
        .to(to.parse()?)
        .subject("Your shesaysyes.ca login PIN")
        .header(ContentType::TEXT_PLAIN)
        .body(format!("Your login PIN is: {}\n\nIt expires in 10 minutes.", pin))?;

    let creds = Credentials::new(smtp_user, smtp_pass);
    let mailer = SmtpTransport::relay("smtp.gmail.com")?
        .credentials(creds)
        .build();

    mailer.send(&email)?;
    Ok(())
}

fn proxy_to_watcher(request: &str) -> String {
    let watcher_addr = match std::env::var("WATCHER_ADDRESS") {
        Ok(addr) => addr,
        Err(_) => return "ERROR: WATCHER_ADDRESS environment variable not set\n".to_string(),
    };
    match TcpStream::connect(&watcher_addr) {
        Ok(mut stream) => {
            let _ = stream.write_all(request.as_bytes());
            let mut reader = BufReader::new(stream);
            let mut response = String::new();
            let _ = reader.read_line(&mut response);
            response
        }
        Err(_) => "ERROR: Watcher not running\n".to_string(),
    }
}
fn proxy_to_node(request: &str) -> String {
    let node_addr = match std::env::var("NODE_ADDRESS") {
        Ok(addr) => addr,
        Err(_) => return "ERROR: NODE_ADDRESS environment variable not set\n".to_string(),
    };
    match TcpStream::connect(&node_addr) {
        Ok(mut stream) => {
            let _ = stream.write_all(request.as_bytes());
            let mut reader = BufReader::new(stream);
            let mut response = String::new();
            let _ = reader.read_line(&mut response);
            response
        }
        Err(_) => "ERROR: Node not running\n".to_string(),
    }
}

fn handle_send(from: &str, to: &str, amount: &str, private_key_hex: &str) -> String {
    let signing_key = match derive_address(private_key_hex).and_then(|_| {
        let bytes = hex::decode(private_key_hex.trim()).ok()?;
        if bytes.len() != 32 { return None; }
        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(&bytes);
        Some(SigningKey::from_bytes(&key_bytes))
    }) {
        Some(k) => k,
        None => return "ERROR: invalid private key\n".to_string(),
    };

    let tip = proxy_to_node("GET_TIP\n").trim().to_string();
    if tip.starts_with("ERROR") || tip.is_empty() {
        return "ERROR: Could not get tip from node\n".to_string();
    }

    let msg = format!("{}{}{}", from, to, amount);
    let signature = signing_key.sign(msg.as_bytes());
    let sig_hex = hex::encode(signature.to_bytes());

    let tx_line = format!("{}|{}|{}|{}|{}|{}\n", tip, from, to, amount, sig_hex, from);
    proxy_to_node(&tx_line)
}

fn handle_cashout(address: &str, amount_str: &str) -> String {
    let amount: f64 = match amount_str.parse() {
        Ok(a) if a > 0.0 => a,
        _ => return "ERROR: invalid amount\n".to_string(),
    };

    let tip = proxy_to_node("GET_TIP\n").trim().to_string();
    if tip.starts_with("ERROR") || tip.is_empty() {
        return "ERROR: Could not get tip from node\n".to_string();
    }

    let secret = std::env::var("DEPOSIT_SECRET").unwrap_or_default();
    if secret.is_empty() {
        return "ERROR: DEPOSIT_SECRET not set\n".to_string();
    }
    let tx_line = format!("{}|cashout|{}|{}|{}|\n", tip, address, amount, secret);
    let result = proxy_to_node(&tx_line);

    if result.trim() == "OK" {
        format!(
            "Cash out successful. {} CRUX burned.<br><br>\
             <button disabled>Convert to cash (partner TBD)</button>\
             <p style=\"color:#666;font-size:0.9em;\">Fiat partner not yet configured.</p>",
            amount
        )
    } else {
        format!("Cash out failed: {}", result.trim())
    }
}

fn content_type_for(path: &str) -> &'static str {
    if path.ends_with(".mp4") { "video/mp4" }
    else if path.ends_with(".webm") { "video/webm" }
    else if path.ends_with(".png") { "image/png" }
    else if path.ends_with(".jpg") || path.ends_with(".jpeg") { "image/jpeg" }
    else if path.ends_with(".gif") { "image/gif" }
    else if path.ends_with(".svg") { "image/svg+xml" }
    else if path.ends_with(".css") { "text/css" }
    else if path.ends_with(".js") { "application/javascript" }
    else { "application/octet-stream" }
}

fn handle_client(mut stream: TcpStream, users: Arc<Mutex<HashMap<String, String>>>, pins: Arc<Mutex<HashMap<String, (String, u64)>>>, solana_keys: Arc<Mutex<HashMap<String, String>>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    let _ = reader.read_line(&mut request_line);

    // Read headers and capture X-Keys-Secret
    let mut keys_secret_header = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        let _ = reader.read_line(&mut line);
        if line.trim().is_empty() {
            break;
        }
        let lower = line.to_lowercase();
        if lower.starts_with("x-keys-secret:") {
            keys_secret_header = line.splitn(2, ':').nth(1).unwrap_or("").trim().to_string();
        }
    }

    let (status, content_type, body) = if request_line.contains("POST /check-email") {
        let mut body = String::new();
        let _ = reader.read_line(&mut body);
        let email = body.trim().to_lowercase();
        let users_map = users.lock().unwrap();
        let result = if users_map.contains_key(&email) { "exists" } else { "new" };
        ("200 OK", "text/plain", result.to_string())
    } else if request_line.contains("POST /send-pin") {
        let mut body = String::new();
        let _ = reader.read_line(&mut body);
        let email = body.trim().to_lowercase();
        let pin = generate_pin();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        pins.lock().unwrap().insert(email.clone(), (pin.clone(), now));
        match send_pin_email(&email, &pin) {
            Ok(_) => ("200 OK", "text/plain", "PIN sent".to_string()),
            Err(e) => {
                eprintln!("Failed to send PIN email to {}: {}", email, e);
                ("500 Internal Server Error", "text/plain", "Failed to send PIN".to_string())
            }
        }
    } else if request_line.contains("POST /verify-pin") {
        let mut body = String::new();
        let _ = reader.read_line(&mut body);
        let parts: Vec<&str> = body.trim().split('|').collect();
        if parts.len() == 2 {
            let email = parts[0].to_lowercase();
            let entered = parts[1];
            let mut pins_map = pins.lock().unwrap();
            if let Some((stored_pin, sent_at)) = pins_map.get(&email) {
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                if now - sent_at > 600 {
                    pins_map.remove(&email);
                    ("401 Unauthorized", "text/plain", "PIN expired".to_string())
                } else if stored_pin == entered {
                    pins_map.remove(&email);
                    ("200 OK", "text/plain", "OK".to_string())
                } else {
                    ("401 Unauthorized", "text/plain", "Wrong PIN".to_string())
                }
            } else {
                ("401 Unauthorized", "text/plain", "No PIN for this email".to_string())
            }
        } else {
            ("400 Bad Request", "text/plain", "email|pin required".to_string())
        }
    } else if request_line.contains("POST /create-wallet") {
        let mut body = String::new();
        let _ = reader.read_line(&mut body);
        let email = body.trim().to_lowercase();
        let mut users_map = users.lock().unwrap();
        if users_map.contains_key(&email) {
            ("400 Bad Request", "text/plain", "Email already exists".to_string())
        } else {
            let (priv_hex, addr) = generate_new_key();
            let solana_keypair = Keypair::new();
            let solana_pubkey = solana_keypair.pubkey().to_string();
            let solana_priv = bs58::encode(solana_keypair.to_bytes()).into_string();
            users_map.insert(email.clone(), format!("{}|{}", addr, solana_pubkey));
            save_users(&users_map);
            let mut solana_map = solana_keys.lock().unwrap();
            solana_map.insert(email, solana_priv);
            save_solana_keys(&solana_map);
            ("200 OK", "text/plain", format!("{}|{}|{}", priv_hex, addr, solana_pubkey))
        }
    } else if request_line.contains("POST /get-wallet") {
        let mut body = String::new();
        let _ = reader.read_line(&mut body);
        let email = body.trim().to_lowercase();
        let users_map = users.lock().unwrap();
        if let Some(stored) = users_map.get(&email) {
            let stored_parts: Vec<&str> = stored.split('|').collect();
            if stored_parts.len() >= 1 {
                let addr = stored_parts[0];
                let solana_pubkey = if stored_parts.len() > 1 { stored_parts[1] } else { "" };
                ("200 OK", "text/plain", format!("{}|{}", addr, solana_pubkey))
            } else {
                ("404 Not Found", "text/plain", "Wallet not found".to_string())
            }
        } else {
            ("404 Not Found", "text/plain", "Wallet not found".to_string())
        }
    } else if request_line.contains("POST /login") {
        let mut body = String::new();
        let _ = reader.read_line(&mut body);
        let parts: Vec<&str> = body.trim().split('|').collect();
        if parts.len() == 2 {
            let email = parts[0].to_lowercase();
            let pk = parts[1];
            match derive_address(pk) {
                Some(addr) => {
                    let users_map = users.lock().unwrap();
                    if let Some(stored) = users_map.get(&email) {
                        let stored_parts: Vec<&str> = stored.split('|').collect();
                        if stored_parts.len() >= 1 && stored_parts[0] == addr {
                            let solana_pubkey = if stored_parts.len() > 1 { stored_parts[1] } else { "" };
                            ("200 OK", "text/plain", format!("{}|{}", addr, solana_pubkey))
                        } else {
                            ("401 Unauthorized", "text/plain", "Wrong private key for this email".to_string())
                        }
                    } else {
                        ("401 Unauthorized", "text/plain", "Wrong private key for this email".to_string())
                    }
                }
                None => ("401 Unauthorized", "text/plain", "Invalid private key".to_string()),
            }
        } else {
            ("400 Bad Request", "text/plain", "email|privatekey required".to_string())
        }
    } else if request_line.contains("GET /watcher-ping") {
        ("200 OK", "text/plain", proxy_to_watcher("PING\n"))
    } else if request_line.contains("GET /balance") {
        ("200 OK", "application/json", proxy_to_node("GET_BALANCES\n"))
    } else if request_line.contains("GET /chain") {
        ("200 OK", "application/json", proxy_to_node("GET_CHAIN\n"))
    } else if request_line.contains("POST /send") {
        let mut body = String::new();
        let _ = reader.read_line(&mut body);
        let parts: Vec<&str> = body.trim().split('|').collect();
        let result = if parts.len() == 4 {
            handle_send(parts[0], parts[1], parts[2], parts[3])
        } else {
            "ERROR: address|to|amount|privatekey required\n".to_string()
        };
        ("200 OK", "text/plain", result)
    } else if request_line.contains("POST /cashout") {
        let mut body = String::new();
        let _ = reader.read_line(&mut body);
        let parts: Vec<&str> = body.trim().split('|').collect();
        let result = if parts.len() == 2 {
            handle_cashout(parts[0], parts[1])
        } else {
            "ERROR: address|amount required\n".to_string()
        };
        ("200 OK", "text/html", result)
    } else if request_line.contains("GET /solana-keys") {
        let expected = std::env::var("KEYS_SECRET").unwrap_or_default();
        let ok = !expected.is_empty() && keys_secret_header == expected;
        if !ok {
            ("401 Unauthorized", "text/plain", "Unauthorized".to_string())
        } else {
            let map = solana_keys.lock().unwrap();
            let body = serde_json::to_string(&*map).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", body)
        }
    } else if request_line.starts_with("GET /assets/") {
        let path_part = request_line.split_whitespace().nth(1).unwrap_or("");
        let rel = path_part.trim_start_matches("/assets/");
        let safe = rel.split('?').next().unwrap_or("");
        if safe.contains("..") || safe.is_empty() {
            ("400 Bad Request", "text/plain", "Bad path".to_string())
        } else {
            let file_path = format!("assets/{}", safe);
            match fs::read(&file_path) {
                Ok(bytes) => {
                    let ct = content_type_for(&file_path);
                    let http = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nAccept-Ranges: bytes\r\n\r\n",
                        ct, bytes.len()
                    );
                    let _ = stream.write_all(http.as_bytes());
                    let _ = stream.write_all(&bytes);
                    return;
                }
                Err(_) => ("404 Not Found", "text/plain", "Asset not found".to_string()),
            }
        }
    } else if request_line.contains("GET /") || request_line.contains("GET /index") {
        ("200 OK", "text/html", fs::read_to_string("wallet.html").unwrap_or_else(|_| "Missing wallet.html".to_string()))
    } else {
        ("404 Not Found", "text/plain", "Not found".to_string())
    };

    let http = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\n\r\n{}",
        status, content_type, body.len(), body
    );
    let _ = stream.write_all(http.as_bytes());
}

fn main() {
    let users = Arc::new(Mutex::new(load_users()));
    let pins = Arc::new(Mutex::new(HashMap::<String, (String, u64)>::new()));
    let solana_keys = Arc::new(Mutex::new(load_solana_keys()));
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).unwrap();
    println!("Web wallet running at http://{}", addr);
    for stream in listener.incoming() {
        if let Ok(stream) = stream {
            let users = Arc::clone(&users);
            let pins = Arc::clone(&pins);
            let solana_keys = Arc::clone(&solana_keys);
            thread::spawn(move || handle_client(stream, users, pins, solana_keys));
        }
    }
}
