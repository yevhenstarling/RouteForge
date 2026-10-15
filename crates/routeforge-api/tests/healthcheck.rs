//! The API's binary as Docker runs it for a health check: with one argument, against
//! whatever listens on `API_BIND` (D55).
//!
//! What it asks and how it reads the answer is tested where it is written, in
//! `probe.rs`. This is the way in to it, which only the binary itself can show.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::process::{Command, Output};
use std::thread;

/// Something on a port of this machine that answers the one request it gets with
/// `reply`.
fn answering(reply: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a free port on this machine");
    let address = listener.local_addr().expect("the port it was given");
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut read = Vec::new();
        let mut byte = [0];
        while !read.ends_with(b"\r\n\r\n") && matches!(stream.read(&mut byte), Ok(1)) {
            read.push(byte[0]);
        }
        // Whoever asked may have gone by now, which is its own business.
        let _ = stream.write_all(reply.as_bytes());
    });
    address
}

/// Runs the binary with one argument, told that its API listens on `bind`.
///
/// It is given no database and no key. Serving needs both, so a binary that took the
/// argument for a reason to serve ends with an error that names what it misses.
fn api(argument: &str, bind: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_routeforge-api"))
        .arg(argument)
        .env("API_BIND", bind)
        .env_remove("DATABASE_URL")
        .env_remove("JWT_SECRET")
        .output()
        .expect("the binary of the API runs")
}

#[test]
fn given_healthcheck_it_asks_and_says_what_it_heard_with_its_exit_code() {
    let ready = answering("HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
    let asked = api("healthcheck", &ready.to_string());
    assert!(asked.status.success(), "{asked:?}");
    // It serves nothing, so it has nothing to log either.
    assert!(asked.stdout.is_empty(), "{asked:?}");

    let unready = answering("HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\n\r\n");
    let asked = api("healthcheck", &unready.to_string());
    assert_eq!(asked.status.code(), Some(1), "{asked:?}");
    let said = String::from_utf8_lossy(&asked.stderr);
    assert!(said.contains("503"), "{said}");
}

#[test]
fn an_argument_it_does_not_know_is_refused_by_name() {
    let refused = api("health", "127.0.0.1:8080");
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(said.contains("`health`"), "{said}");
    assert!(said.contains("`healthcheck`"), "{said}");
    // It did not go on to serve, which would have ended on the database it lacks.
    assert!(!said.contains("DATABASE_URL"), "{said}");
}
