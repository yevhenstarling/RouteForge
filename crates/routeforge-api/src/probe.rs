//! The health check of the image: `routeforge-api healthcheck` asks the API that runs
//! beside it whether it is ready, and says so with its exit code (D55).
//!
//! The image the API runs in has no `curl` to ask with, and nothing else it would
//! need one for. So the binary asks itself. It speaks just enough HTTP for that, over
//! a plain socket.

use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

use anyhow::{Context, bail};

use crate::routes::health::READYZ;

/// How long each step of asking may take: connecting, sending, and waiting for the
/// answer. Docker gives a check three seconds in all, and ends one that takes longer.
const WITHIN: Duration = Duration::from_secs(2);

/// Asks the API that listens on `bind` whether it is ready.
///
/// # Errors
///
/// If nothing listens there, or what does answers anything but `200`, or not in time.
pub(crate) fn probe(bind: SocketAddr) -> anyhow::Result<()> {
    probe_within(bind, WITHIN)
}

fn probe_within(bind: SocketAddr, within: Duration) -> anyhow::Result<()> {
    let address = reachable(bind);
    let mut stream = TcpStream::connect_timeout(&address, within)
        .with_context(|| format!("nothing answers at {address}"))?;
    stream.set_read_timeout(Some(within))?;
    stream.set_write_timeout(Some(within))?;
    // `Connection: close`, so that the server does not wait for a second request.
    write!(
        stream,
        "GET {READYZ} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .with_context(|| format!("could not ask {address}"))?;

    let mut status_line = String::new();
    BufReader::new(stream)
        .read_line(&mut status_line)
        .with_context(|| format!("{address} did not answer"))?;
    let status_line = status_line.trim_end();
    // `HTTP/1.1 200 OK`: the status is the second word.
    if status_line.starts_with("HTTP/") && status_line.split_whitespace().nth(1) == Some("200") {
        Ok(())
    } else {
        bail!("{address} answered {READYZ} with `{status_line}`")
    }
}

/// Where to reach a server that listens on `bind`. One that listens on every address,
/// as the API does in a container, is asked on the loopback.
fn reachable(bind: SocketAddr) -> SocketAddr {
    let ip = match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    SocketAddr::new(ip, bind.port())
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Instant;

    use sqlx::PgPool;

    use super::*;
    use crate::{startup, testing};

    /// Short enough for a test to wait out.
    const BRIEFLY: Duration = Duration::from_millis(200);

    /// Something on a port of this machine that takes one request and answers it with
    /// `reply`, or with nothing at all. Gives its address, and the request when it has
    /// come.
    fn answering(reply: Option<&'static str>) -> (SocketAddr, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (asked, request) = mpsc::channel();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut read = Vec::new();
            let mut byte = [0];
            while !read.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap() == 1 {
                read.push(byte[0]);
            }
            asked.send(String::from_utf8(read).unwrap()).unwrap();
            match reply {
                Some(reply) => stream.write_all(reply.as_bytes()).unwrap(),
                // Keeps the connection, and keeps quiet, for longer than anybody waits.
                None => thread::sleep(BRIEFLY * 5),
            }
        });
        (address, request)
    }

    #[test]
    fn it_asks_whether_the_api_is_ready_and_takes_only_a_yes() {
        let ready = "HTTP/1.1 200 OK\r\ncontent-length: 18\r\n\r\n{\"status\":\"ready\"}";
        let (address, request) = answering(Some(ready));
        probe_within(address, BRIEFLY).unwrap();
        let request = request.recv().unwrap();
        assert!(request.starts_with("GET /readyz HTTP/1.1\r\n"), "{request}");
        assert!(
            request.contains(&format!("\r\nHost: {address}\r\n")),
            "{request}"
        );
        assert!(request.contains("\r\nConnection: close\r\n"), "{request}");

        for (reply, said) in [
            ("HTTP/1.1 503 Service Unavailable\r\n\r\n", "503"),
            ("HTTP/1.1 404 Not Found\r\n\r\n", "404"),
            // What listens there is no API, whatever its second word.
            ("220 200 ESMTP\r\n", "ESMTP"),
            // And one that hangs up without a word.
            ("", "``"),
        ] {
            let (address, _request) = answering(Some(reply));
            let error = probe_within(address, BRIEFLY).unwrap_err();
            assert!(error.to_string().contains(said), "{reply:?}: {error}");
        }
    }

    #[test]
    fn an_api_that_listens_on_every_address_is_asked_on_the_loopback() {
        let reached = |bind: &str| reachable(bind.parse().unwrap()).to_string();
        assert_eq!(reached("0.0.0.0:8080"), "127.0.0.1:8080");
        assert_eq!(reached("[::]:8080"), "[::1]:8080");
        // An address that names one place is asked where it says.
        assert_eq!(reached("127.0.0.1:9000"), "127.0.0.1:9000");
        assert_eq!(reached("192.0.2.7:8080"), "192.0.2.7:8080");

        let (address, request) = answering(Some("HTTP/1.1 200 OK\r\n\r\n"));
        let everywhere = SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), address.port());
        probe_within(everywhere, BRIEFLY).unwrap();
        let request = request.recv().unwrap();
        assert!(request.contains(&format!("Host: {address}")), "{request}");
    }

    #[test]
    fn it_does_not_wait_long_for_an_answer_that_does_not_come() {
        let (address, _request) = answering(None);
        let asked = Instant::now();
        let error = probe_within(address, BRIEFLY).unwrap_err();
        assert!(error.to_string().contains("did not answer"), "{error}");
        assert!(asked.elapsed() < BRIEFLY * 4, "{:?}", asked.elapsed());

        // A port that was given back a moment ago: nothing listens on it.
        let free = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let error = probe_within(free, BRIEFLY).unwrap_err();
        assert!(error.to_string().contains("nothing answers"), "{error}");
    }

    /// The check as the stack runs it: over a socket, against the application.
    #[sqlx::test(migrations = false)]
    async fn the_application_passes_the_check_once_it_can_work(pool: PgPool) {
        let api = testing::api(&pool);
        let (address, server) = api.serve();
        let check = async || {
            tokio::task::spawn_blocking(move || probe(address))
                .await
                .unwrap()
        };

        // Up, and without a schema to work with.
        let unready = check().await.unwrap_err();
        assert!(unready.to_string().contains("503"), "{unready}");

        startup::migrate(&pool).await.unwrap();
        check().await.unwrap();

        server.stop(true).await;
    }
}
