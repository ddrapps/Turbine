use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};

mod deposit;
use deposit::credit_on_chain;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Deposit {
    id: u64,
    user_chain_address: String,
    incoming_coin: String,
    incoming_amount: f64,
    usdc_received: f64,
    fee_percent: f64,
    fee_usdc: f64,
    credited_coin: f64,
    status: String,
    timestamp: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Cashout {
    id: u64,
    user_chain_address: String,
    coin_amount: f64,
    fee_percent: f64,
    fee_usdc: f64,
    usdc_sent: f64,
    destination_usdc_address: String,
    status: String,
    timestamp: String,
}

fn load_operator_address() -> String {
    let content = fs::read_to_string("/data/str.env").unwrap_or_default();
    for line in content.lines() {
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "STR327_ADDRESS" {
                return value.trim().to_string();
            }
        }
    }
    "operator".to_string()
}

fn load_deposits() -> Vec<Deposit> {
    match fs::read_to_string("/data/deposits.json") {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn save_deposits(deposits: &[Deposit]) {
    let json = serde_json::to_string_pretty(deposits).unwrap();
    fs::write("/data/deposits.json", json).unwrap();
}

fn load_cashouts() -> Vec<Cashout> {
    match fs::read_to_string("/data/cashouts.json") {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn save_cashouts(cashouts: &[Cashout]) {
    let json = serde_json::to_string_pretty(cashouts).unwrap();
    fs::write("/data/cashouts.json", json).unwrap();
}

fn next_id<T>(items: &[T], get_id: fn(&T) -> u64) -> u64 {
    items.iter().map(get_id).max().unwrap_or(0) + 1
}

fn node_address() -> String {
    std::env::var("NODE_ADDRESS").unwrap_or_else(|_| "".to_string())
}

fn debit_on_chain(user_address: &str, amount: f64) -> bool {
    let addr = node_address();
    let tip = {
        let mut stream = match TcpStream::connect(&addr) {
            Ok(s) => s,
            Err(_) => return false,
        };
        if stream.write_all(b"GET_TIP\n").is_err() { return false; }
        let mut reader = BufReader::new(stream);
        let mut tip = String::new();
        if reader.read_line(&mut tip).is_err() { return false; }
        tip.trim().to_string()
    };
    if tip.is_empty() { return false; }

    let mut stream = match TcpStream::connect(&addr) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let tx_line = format!("{}|cashout|{}|{}||\n", tip, user_address, amount);
    if stream.write_all(tx_line.as_bytes()).is_err() { return false; }

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    if reader.read_line(&mut response).is_err() { return false; }
    response.trim() == "OK"
}

fn handle_credit_request(line: &str) -> String {
    let parts: Vec<&str> = line.split('|').collect();
    if parts.len() < 4 { return "REJECTED".to_string(); }
    let user = parts[0].to_string();
    let coin = parts[1].to_string();
    let amount: f64 = parts[2].parse().unwrap_or(0.0);
    let usdc: f64 = parts[3].parse().unwrap_or(0.0);
    let fee_percent = 1.0;
    let fee_usdc = usdc * (fee_percent / 100.0);
    let credited = usdc - fee_usdc;
    let operator = load_operator_address();

    let mut deposits = load_deposits();
    let id = next_id(&deposits, |d| d.id);
    let now = chrono::Utc::now().to_rfc3339();
    deposits.push(Deposit {
        id, user_chain_address: user.clone(), incoming_coin: coin,
        incoming_amount: amount, usdc_received: usdc, fee_percent,
        fee_usdc, credited_coin: credited, status: "pending".into(), timestamp: now,
    });
    save_deposits(&deposits);

    let user_ok = credit_on_chain(&user, credited);
    let fee_ok = if fee_usdc > 0.0 { credit_on_chain(&operator, fee_usdc) } else { true };

    if user_ok {
        if let Some(d) = deposits.iter_mut().find(|d| d.id == id) { d.status = "credited".into(); }
        save_deposits(&deposits);
        if fee_ok { format!("OK {:.6}", credited) } else { format!("OK {:.6} (fee failed)", credited) }
    } else {
        "REJECTED".to_string()
    }
}

fn run_server() {
    let port = std::env::var("PORT").unwrap_or_else(|_| "7879".to_string());
    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).expect("Failed to bind tracker");
    println!("Tracker listening on {}", addr);

    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).is_ok() {
            let msg = line.trim();
            println!("Received: {}", msg);
            let response = handle_credit_request(msg);
            let _ = stream.write_all(format!("{}\n", response).as_bytes());
        }
    }
}

fn main() {
    let operator = load_operator_address();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        run_server();
        return;
    }

    match args[1].as_str() {
        "deposit" => {
            if args.len() < 3 { println!("deposit add or list"); return; }
            match args[2].as_str() {
                "add" => {
                    if args.len() < 7 { println!("deposit add <user_addr> <coin> <amount> <usdc_received>"); return; }
                    let user = args[3].clone();
                    let coin = args[4].clone();
                    let amount: f64 = args[5].parse().unwrap_or(0.0);
                    let usdc: f64 = args[6].parse().unwrap_or(0.0);
                    let fee_percent = 1.0;
                    let fee_usdc = usdc * (fee_percent / 100.0);
                    let credited = usdc - fee_usdc;

                    let mut deposits = load_deposits();
                    let id = next_id(&deposits, |d| d.id);
                    let now = chrono::Utc::now().to_rfc3339();
                    deposits.push(Deposit {
                        id, user_chain_address: user.clone(), incoming_coin: coin,
                        incoming_amount: amount, usdc_received: usdc, fee_percent,
                        fee_usdc, credited_coin: credited, status: "pending".into(), timestamp: now,
                    });
                    save_deposits(&deposits);
                    println!("Deposit #{} logged. Will credit: {:.6} (fee {:.6} to operator)", id, credited, fee_usdc);

                    let user_ok = credit_on_chain(&user, credited);
                    let fee_ok = if fee_usdc > 0.0 { credit_on_chain(&operator, fee_usdc) } else { true };

                    if user_ok {
                        if let Some(d) = deposits.iter_mut().find(|d| d.id == id) { d.status = "credited".into(); }
                        save_deposits(&deposits);
                        println!("Credited {:.6} to {} on chain", credited, user);
                        if fee_ok {
                            println!("Fee {:.6} USDC credited to operator {}", fee_usdc, operator);
                        } else {
                            println!("Warning: failed to credit fee to operator");
                        }
                    } else {
                        println!("Logged but failed to credit on chain");
                    }
                }
                "list" => {
                    let deposits = load_deposits();
                    if deposits.is_empty() { println!("No deposits."); }
                    else {
                        for d in deposits {
                            println!("#{} | {} | {} {} → {:.6} USDC | fee {:.6} | credited {:.6} | {}",
                                d.id, d.user_chain_address, d.incoming_amount, d.incoming_coin,
                                d.usdc_received, d.fee_usdc, d.credited_coin, d.status);
                        }
                    }
                }
                _ => println!("Unknown deposit command"),
            }
        }
        "cashout" => {
            if args.len() < 3 { println!("cashout add or list"); return; }
            match args[2].as_str() {
                "add" => {
                    if args.len() < 7 { println!("cashout add <user_addr> <coin_amount> <usdc_sent> <dest_usdc_addr>"); return; }
                    let user = args[3].clone();
                    let coin_amount: f64 = args[4].parse().unwrap_or(0.0);
                    let usdc_sent: f64 = args[5].parse().unwrap_or(0.0);
                    let dest = args[6].clone();
                    let fee_percent = 1.0;
                    let fee_usdc = usdc_sent * (fee_percent / 100.0);
                    let net_usdc = usdc_sent - fee_usdc;

                    let mut cashouts = load_cashouts();
                    let id = next_id(&cashouts, |c| c.id);
                    let now = chrono::Utc::now().to_rfc3339();
                    cashouts.push(Cashout {
                        id, user_chain_address: user.clone(), coin_amount, fee_percent,
                        fee_usdc, usdc_sent: net_usdc, destination_usdc_address: dest,
                        status: "pending".into(), timestamp: now,
                    });
                    save_cashouts(&cashouts);
                    println!("Cashout #{} logged. Will debit: {:.6} (fee {:.6} to operator)", id, coin_amount, fee_usdc);

                    if debit_on_chain(&user, coin_amount) {
                        if let Some(c) = cashouts.iter_mut().find(|c| c.id == id) { c.status = "paid".into(); }
                        save_cashouts(&cashouts);
                        println!("Debited {:.6} from {} on chain. USDC to send: {:.6}", coin_amount, user, net_usdc);

                        if fee_usdc > 0.0 {
                            if credit_on_chain(&operator, fee_usdc) {
                                println!("Fee {:.6} USDC credited to operator {}", fee_usdc, operator);
                            } else {
                                println!("Warning: failed to credit fee to operator");
                            }
                        }
                    } else {
                        println!("Logged but failed to debit on chain");
                    }
                }
                "list" => {
                    let cashouts = load_cashouts();
                    if cashouts.is_empty() { println!("No cashouts."); }
                    else {
                        for c in cashouts {
                            println!("#{} | {} | {:.6} coin → {:.6} USDC | fee {:.6} | to {} | {}",
                                c.id, c.user_chain_address, c.coin_amount, c.usdc_sent,
                                c.fee_usdc, c.destination_usdc_address, c.status);
                        }
                    }
                }
                _ => println!("Unknown cashout command"),
            }
        }
        _ => println!("Unknown command. Use deposit or cashout."),
    }
}
