use ed25519_dalek::{SigningKey, Signer};
use rand::rngs::ThreadRng;
use rand::Rng;
use hex;
use std::env;
use std::fs;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 4 {
        println!("Usage: cargo run --bin sign -- <from> <to> <amount>");
        return;
    }
    let from = &args[1];
    let to = &args[2];
    let amount = &args[3];

    let mut rng = ThreadRng::default();
    let mut secret = [0u8; 32];
    rng.fill_bytes(&mut secret);

    let signing_key = SigningKey::from_bytes(&secret);
    let verifying_key = signing_key.verifying_key();

    let msg = format!("{}{}{}", from, to, amount);
    let signature = signing_key.sign(msg.as_bytes());

    let pub_hex = hex::encode(verifying_key.as_bytes());
    let sig_hex = hex::encode(signature.to_bytes());

    let line = format!("TIP|{}|{}|{}|{}|{}", from, to, amount, sig_hex, pub_hex);

    fs::write("signed_tx.txt", &line).unwrap();

    println!("Wrote signed_tx.txt");
    println!("Public key: {}", pub_hex);
    println!("Signature:  {}", sig_hex);
    println!("\nContent:");
    println!("{}", line);
}