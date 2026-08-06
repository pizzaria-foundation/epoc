//! Drive the real protocol against the real servers, from Rust.
//!
//! ```text
//! cargo run --release -p tg-proto --example live
//! ```
//!
//! Everything else in this crate is checked against a transcript. This is the one thing
//! that cannot be: whether Telegram accepts what we send. A fixture proves the Rust agrees
//! with the Python; only the server proves the Python was right.
//!
//! It runs on the host with `std`, which is not the target — but the code being exercised is
//! the same `Client` the phone runs, and the parts that differ (a socket, a clock, a random
//! source) are exactly the parts `Client` deliberately does not contain. What this validates
//! is the protocol; the device self test validates the platform.
//!
//! No account is touched. `help.getConfig` needs no login, and the key negotiated here is
//! discarded when the process exits.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{SystemTime, UNIX_EPOCH};

use tg_proto::auth::{self, Action, Login};
use tg_proto::client::{Client, Step};
use tg_proto::crypto::Rng;
use tg_proto::rpc::{self, Update};

/// Telegram's production data centres. `TG_DC` selects one; the default is where a client
/// with no stored session begins.
const DCS: [&str; 6] = [
    "149.154.167.51:443",
    "149.154.175.53:443",
    "149.154.167.51:443",
    "149.154.175.100:443",
    "149.154.167.91:443",
    "91.108.56.130:443",
];

/// Entropy from the host, standing in for `symbian::random::Random`.
///
/// A deliberately unremarkable source: this example is testing the protocol, not the
/// randomness, and using something elaborate here would only obscure which of the two
/// failed. On the device the pool comes from `shim_entropy` and is documented there.
struct HostRng(u64);

impl Rng for HostRng {
    fn fill(&mut self, out: &mut [u8]) {
        for b in out.iter_mut() {
            // xorshift64*, seeded from the clock. Fine for a host smoke test and stated as
            // such rather than dressed up.
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            *b = (self.0 >> 33) as u8;
        }
    }
}

fn now() -> (i64, u32) {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
    (d.as_secs() as i64, d.subsec_nanos())
}

fn main() -> std::io::Result<()> {
    let seed = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64;
    let mut rng = HostRng(seed | 1);

    let api_id: i32 = std::env::var("TG_API_ID").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let api_hash = std::env::var("TG_API_HASH").unwrap_or_default();
    let dc: usize = std::env::var("TG_DC").ok().and_then(|s| s.parse().ok()).unwrap_or(2);
    let mut login = Login::new(api_id, &api_hash);

    let addr = DCS[dc.min(DCS.len() - 1)];
    let mut sock = TcpStream::connect(addr)?;
    sock.set_read_timeout(Some(std::time::Duration::from_secs(20)))?;
    println!("connected to DC{dc} at {addr}");
    if api_id == 0 {
        println!("  no TG_API_ID: auth calls will answer API_ID_INVALID");
    }

    let (mut client, first) = Client::connect(&mut rng);
    sock.write_all(client.greeting())?;

    let mut pending = vec![first];
    let mut buf = [0u8; 4096];
    let mut asked_config = false;
    let mut deadline = 40;

    loop {
        // Drain whatever the client wants done before reading again.
        while let Some(step) = pending.pop() {
            match step {
                Step::Send(bytes) => {
                    sock.write_all(&bytes)?;
                }
                Step::ModPow { base, exp, modulus } => {
                    // On the phone this goes to the worker thread; here it just blocks.
                    // The timing is worth printing: it is the number that forced the worker
                    // thread to exist, and the ratio between host and handset is 20x.
                    let t = std::time::Instant::now();
                    let m = symbian_crypto::Modulus::new(&modulus).expect("bad modulus");
                    let mut out = vec![0u8; modulus.len()];
                    symbian_crypto::modpow(&base, &exp, &m, &mut out).expect("modpow");
                    println!("  modpow: {} ms on the host", t.elapsed().as_millis());
                    pending.extend(client.on_modpow(&out, &mut rng).expect("modpow rejected"));
                }
                Step::Ready => {
                    let key = client.auth_key().expect("ready without a key");
                    println!("auth key negotiated");
                    println!("  auth_key_id: {:016x}", key.id);

                    // initConnection is required once per connection, and wrapping the
                    // first real call in it is how every client does it.
                    let inner = match std::env::var("TG_PHONE") {
                        Ok(phone) => {
                            println!("  asking for a code for {phone}");
                            let a = login.send_code(&phone);
                            let Action::Call { body, .. } = a else { unreachable!() };
                            body
                        }
                        Err(_) => {
                            println!("  no TG_PHONE set, asking for the config instead");
                            rpc::get_config()
                        }
                    };
                    let query = rpc::init_connection(
                        api_id, "Nokia E72", "Symbian 9.3", "0.1", &inner,
                    );
                    let (t, _) = now();
                    let (_, step) = client.call(&query, 1, t, 0, &mut rng).expect("call");
                    asked_config = true;
                    pending.push(step);
                }
                Step::Update(u) => match u {
                    Update::Result { req_msg_id, body, .. } => {
                        let ctor = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
                        println!(
                            "result for {req_msg_id:016x}: {} bytes, constructor {ctor:08x}",
                            body.len()
                        );
                        if client.tag_of(req_msg_id) == Some(1) {
                            if std::env::var("TG_PHONE").is_ok() {
                                match login.on_reply(auth::tag::SEND_CODE, &body, &mut rng) {
                                    Action::CodeSent { length } => {
                                        println!("\na code was sent. digits: {length:?}");
                                        println!("the login flow works end to end.");
                                    }
                                    other => println!("\nunexpected: {other:?}"),
                                }
                            } else {
                                println!("\nhelp.getConfig answered. The protocol works.");
                            }
                            return Ok(());
                        }
                    }
                    Update::RpcError { code, text, .. } => {
                        // The interesting ones are named. A Brazilian number asked at DC2
                        // answers PHONE_MIGRATE_n, which is an instruction rather than a
                        // failure -- and the most likely thing to happen here.
                        println!("rpc error {code}: {text}");
                        if let Some(dc) = auth::migrate_target(&text) {
                            println!("  -> this account lives on DC{dc}; a real client");
                            println!("     redoes the handshake there. Set TG_DC={dc}.");
                        } else {
                            println!("  -> classified as {:?}", auth::AuthError::classify(&text));
                        }
                        return Ok(());
                    }
                    other => println!("update: {other:?}"),
                },
            }
        }

        if asked_config {
            let (t, n) = now();
            if let Some(step) = client.pending_ack(t, n, &mut rng) {
                pending.push(step);
                continue;
            }
        }

        let n = sock.read(&mut buf)?;
        if n == 0 {
            println!("the server closed the connection");
            return Ok(());
        }
        pending.extend(client.feed(&buf[..n], &mut rng).expect("feed").into_iter().rev());

        deadline -= 1;
        if deadline == 0 {
            println!("gave up waiting");
            return Ok(());
        }
    }
}
