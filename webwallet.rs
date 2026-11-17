use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

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

fn handle_client(mut stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    let _ = reader.read_line(&mut request_line);

    let response = if request_line.contains("GET /balance") {
        proxy_to_node("GET_BALANCES\n")
    } else if request_line.contains("GET /chain") {
        proxy_to_node("GET_CHAIN\n")
    } else if request_line.contains("GET / ") || request_line.contains("GET /index") {
        fs::read_to_string("wallet.html").unwrap_or_else(|_| "Missing wallet.html".to_string())
    } else {
        "Not found".to_string()
    };

    let http = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/html\r\n\r\n{}",
        response.len(),
        response
    );
    let _ = stream.write_all(http.as_bytes());
}

fn main() {
    let listener = TcpListener::bind("127.0.0.1:3000").unwrap();
    println!("Web wallet running at http://127.0.0.1:3000");
    for stream in listener.incoming() {
        if let Ok(stream) = stream {
            thread::spawn(|| handle_client(stream));
        }
    }
}