use ed25519_dalek::{SigningKey, Signer};
use hex;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

fn load_signing_key() -> Option<SigningKey> {
    let bytes = fs::read("wallet.key").ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut key_bytes = [0u8; 32];
    key_bytes.copy_from_slice(&bytes);
    Some(SigningKey::from_bytes(&key_bytes))
}

fn get_address() -> String {
    if let Some(key) = load_signing_key() {
        hex::encode(key.verifying_key().as_bytes())
    } else {
        "no-wallet-key".to_string()
    }
}

fn proxy_to_node(request: &str) -> String {
    match TcpStream::connect("127.0.0.1:7878") {
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

fn handle_send(to: &str, amount: &str) -> String {
    let signing_key = match load_signing_key() {
        Some(k) => k,
        None => return "ERROR: wallet.key not found or invalid\n".to_string(),
    };
    let from = hex::encode(signing_key.verifying_key().as_bytes());

    // Get tip
    let tip = proxy_to_node("GET_TIP\n").trim().to_string();
    if tip.starts_with("ERROR") || tip.is_empty() {
        return "ERROR: Could not get tip from node\n".to_string();
    }

    // Sign
    let msg = format!("{}{}{}", from, to, amount);
    let signature = signing_key.sign(msg.as_bytes());
    let sig_hex = hex::encode(signature.to_bytes());

    // Build and send tx
    let tx_line = format!("{}|{}|{}|{}|{}|{}\n", tip, from, to, amount, sig_hex, from);
    proxy_to_node(&tx_line)
}

fn handle_client(mut stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    let _ = reader.read_line(&mut request_line);

    let (status, content_type, body) = if request_line.contains("GET /balance") {
        ("200 OK", "application/json", proxy_to_node("GET_BALANCES\n"))
    } else if request_line.contains("GET /chain") {
        ("200 OK", "application/json", proxy_to_node("GET_CHAIN\n"))
    } else if request_line.contains("GET /address") {
        ("200 OK", "text/plain", get_address())
    } else if request_line.contains("POST /send") {
        // Simple body parse: expect "to|amount"
        let mut body = String::new();
        let _ = reader.read_line(&mut body);
        let parts: Vec<&str> = body.trim().split('|').collect();
        let result = if parts.len() == 2 {
            handle_send(parts[0], parts[1])
        } else {
            "ERROR: bad request\n".to_string()
        };
        ("200 OK", "text/plain", result)
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
    let listener = TcpListener::bind("127.0.0.1:3000").unwrap();
    println!("Web wallet running at http://127.0.0.1:3000");
    println!("Address: {}", get_address());
    for stream in listener.incoming() {
        if let Ok(stream) = stream {
            thread::spawn(|| handle_client(stream));
        }
    }
}