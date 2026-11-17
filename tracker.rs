use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Deposit {
    id: u64,
    user_chain_address: String,
    incoming_coin: String,
    incoming_amount: f64,
    sol_received: f64,
    fee_percent: f64,
    fee_sol: f64,
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
    fee_sol: f64,
    sol_sent: f64,
    destination_sol_address: String,
    status: String,
    timestamp: String,
}

fn load_deposits() -> Vec<Deposit> {
    match fs::read_to_string("deposits.json") {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn save_deposits(deposits: &[Deposit]) {
    let json = serde_json::to_string_pretty(deposits).unwrap();
    fs::write("deposits.json", json).unwrap();
}

fn load_cashouts() -> Vec<Cashout> {
    match fs::read_to_string("cashouts.json") {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn save_cashouts(cashouts: &[Cashout]) {
    let json = serde_json::to_string_pretty(cashouts).unwrap();
    fs::write("cashouts.json", json).unwrap();
}

fn next_id<T>(items: &[T], get_id: fn(&T) -> u64) -> u64 {
    items.iter().map(get_id).max().unwrap_or(0) + 1
}

fn credit_on_chain(user_address: &str, amount: f64) -> bool {
    let mut stream = match TcpStream::connect("127.0.0.1:7878") {
        Ok(s) => s,
        Err(_) => {
            println!("Could not connect to node");
            return false;
        }
    };

    if stream.write_all(b"GET_TIP\n").is_err() {
        println!("Failed to send GET_TIP");
        return false;
    }

    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut tip = String::new();
    if reader.read_line(&mut tip).is_err() {
        println!("Failed to read tip");
        return false;
    }
    let tip = tip.trim().to_string();

    let tx_line = format!("{}|deposit|{}|{}||\n", tip, user_address, amount);
    if stream.write_all(tx_line.as_bytes()).is_err() {
        println!("Failed to send credit tx");
        return false;
    }

    let mut response = String::new();
    if reader.read_line(&mut response).is_err() {
        println!("Failed to read response");
        return false;
    }

    response.trim() == "OK"
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("Usage:");
        println!("  cargo run --bin tracker -- deposit add <user_addr> <coin> <amount> <sol_received>");
        println!("  cargo run --bin tracker -- deposit list");
        println!("  cargo run --bin tracker -- cashout add <user_addr> <coin_amount> <sol_sent> <dest_sol_addr>");
        println!("  cargo run --bin tracker -- cashout list");
        return;
    }

    match args[1].as_str() {
        "deposit" => {
            if args.len() < 3 {
                println!("deposit add or deposit list");
                return;
            }
            match args[2].as_str() {
                "add" => {
                    if args.len() < 7 {
                        println!("deposit add <user_addr> <coin> <amount> <sol_received>");
                        return;
                    }
                    let user = args[3].clone();
                    let coin = args[4].clone();
                    let amount: f64 = args[5].parse().unwrap_or(0.0);
                    let sol: f64 = args[6].parse().unwrap_or(0.0);
                    let fee_percent = 1.0;
                    let fee_sol = sol * (fee_percent / 100.0);
                    let credited = sol - fee_sol;

                    let mut deposits = load_deposits();
                    let id = next_id(&deposits, |d| d.id);
                    let now = chrono::Utc::now().to_rfc3339();

                    deposits.push(Deposit {
                        id,
                        user_chain_address: user.clone(),
                        incoming_coin: coin,
                        incoming_amount: amount,
                        sol_received: sol,
                        fee_percent,
                        fee_sol,
                        credited_coin: credited,
                        status: "credited".to_string(),
                        timestamp: now,
                    });
                    save_deposits(&deposits);
                    println!("Deposit #{} logged. Credited: {:.6}", id, credited);

                    if credit_on_chain(&user, credited) {
                        println!("Credited {:.6} to {} on chain", credited, user);
                    } else {
                        println!("Logged but failed to credit on chain (is the node running?)");
                    }
                }
                "list" => {
                    let deposits = load_deposits();
                    if deposits.is_empty() {
                        println!("No deposits.");
                    } else {
                        for d in deposits {
                            println!(
                                "#{} | {} | {} {} → {:.6} SOL | fee {:.6} | credited {:.6} | {}",
                                d.id, d.user_chain_address, d.incoming_amount, d.incoming_coin,
                                d.sol_received, d.fee_sol, d.credited_coin, d.status
                            );
                        }
                    }
                }
                _ => println!("Unknown deposit command"),
            }
        }
        "cashout" => {
            if args.len() < 3 {
                println!("cashout add or cashout list");
                return;
            }
            match args[2].as_str() {
                "add" => {
                    if args.len() < 7 {
                        println!("cashout add <user_addr> <coin_amount> <sol_sent> <dest_sol_addr>");
                        return;
                    }
                    let user = args[3].clone();
                    let coin_amount: f64 = args[4].parse().unwrap_or(0.0);
                    let sol_sent: f64 = args[5].parse().unwrap_or(0.0);
                    let dest = args[6].clone();
                    let fee_percent = 1.0;
                    let fee_sol = sol_sent * (fee_percent / 100.0);
                    let net_sol = sol_sent - fee_sol;

                    let mut cashouts = load_cashouts();
                    let id = next_id(&cashouts, |c| c.id);
                    let now = chrono::Utc::now().to_rfc3339();

                    cashouts.push(Cashout {
                        id,
                        user_chain_address: user,
                        coin_amount,
                        fee_percent,
                        fee_sol,
                        sol_sent: net_sol,
                        destination_sol_address: dest,
                        status: "paid".to_string(),
                        timestamp: now,
                    });
                    save_cashouts(&cashouts);
                    println!("Cashout #{} logged. SOL sent: {:.6}", id, net_sol);
                }
                "list" => {
                    let cashouts = load_cashouts();
                    if cashouts.is_empty() {
                        println!("No cashouts.");
                    } else {
                        for c in cashouts {
                            println!(
                                "#{} | {} | {:.6} coin → {:.6} SOL | fee {:.6} | to {} | {}",
                                c.id, c.user_chain_address, c.coin_amount, c.sol_sent,
                                c.fee_sol, c.destination_sol_address, c.status
                            );
                        }
                    }
                }
                _ => println!("Unknown cashout command"),
            }
        }
        _ => println!("Unknown command. Use deposit or cashout."),
    }
}