//! Headless transfer benchmark using the app's P2P core.
//!
//!   bench recv <workdir>          wait for an offer, accept it, report
//!   bench send <workdir> <file>   send <file> to the receiver, report
//!
//! Both sides publish their ID in <workdir>/<role>.id and read the other's,
//! so the same workdir can pair this binary with bench/electron-bench.mjs.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::SecretKey;
use p2pshare_lib::p2p::{Event, Messages, Node, Options, Status};
use p2pshare_lib::store::JsonStore;
use tokio::sync::mpsc;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(90);

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (role, work) = match (args.first(), args.get(1)) {
        (Some(r), Some(w)) if r == "recv" || r == "send" => (r.clone(), PathBuf::from(w)),
        _ => {
            eprintln!("usage: bench <recv|send> <workdir> [file]");
            std::process::exit(2);
        }
    };
    let sending = role == "send";
    let file = args.get(2).map(PathBuf::from);
    if sending && file.is_none() {
        eprintln!("send needs a file");
        std::process::exit(2);
    }
    std::fs::create_dir_all(&work).unwrap();
    let out = work.join("out-v3");

    let (tx, mut events) = mpsc::unbounded_channel();
    let started = Instant::now();
    let node = Node::start(Options {
        secret: SecretKey::generate(),
        friends: JsonStore::load(work.join(format!("{role}-v3-friends.json")), Vec::new()),
        messages: JsonStore::load(
            work.join(format!("{role}-v3-messages.json")),
            Messages::new(),
        ),
        download_dir: Arc::new(move || out.clone()),
        on_event: Arc::new(move |e| {
            let _ = tx.send(e);
        }),
    })
    .await
    .expect("bind");
    std::fs::write(work.join(format!("{role}.id")), node.id()).unwrap();
    println!("[v3 {role}] id {}", node.id());

    let peer_file = work.join(if sending { "recv.id" } else { "send.id" });
    let peer = loop {
        if let Ok(id) = std::fs::read_to_string(&peer_file) {
            if id.trim().len() == 64 {
                break id.trim().to_string();
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    node.add_friend(&peer, "peer").expect("peer id");
    let paired = Instant::now();

    let mut transfer_start = None;
    loop {
        let event = tokio::time::timeout(CONNECT_TIMEOUT, events.recv()).await;
        let Ok(Some(event)) = event else {
            println!(
                "RESULT {{\"ok\":false,\"error\":\"pas de connexion après {}s\"}}",
                CONNECT_TIMEOUT.as_secs()
            );
            std::process::exit(1);
        };
        match event {
            Event::FriendStatus { online: true, .. } => {
                println!(
                    "[v3 {role}] connecté en {} ms",
                    paired.elapsed().as_millis()
                );
                if sending && transfer_start.is_none() {
                    transfer_start = Some(Instant::now());
                    node.send_files(&peer, &[file.clone().unwrap()]);
                }
            }
            Event::FileOffer(t) => {
                transfer_start = Some(Instant::now());
                node.respond_to_offer(&t.id, true);
            }
            Event::Transfer(t) if t.status == Status::Completed => {
                let secs = transfer_start
                    .map(|s| s.elapsed().as_secs_f64())
                    .unwrap_or(0.0);
                let mbps = t.file_size as f64 / 1_048_576.0 / secs.max(1e-9);
                println!(
                    "RESULT {{\"ok\":true,\"role\":\"{role}\",\"bytes\":{},\"seconds\":{secs:.3},\"mb_per_s\":{mbps:.1},\"total_s\":{:.3}}}",
                    t.file_size,
                    started.elapsed().as_secs_f64()
                );
                // Let the final acks reach the peer before closing.
                tokio::time::sleep(Duration::from_millis(500)).await;
                node.shutdown().await;
                return;
            }
            Event::Transfer(t) if t.status.is_final() => {
                println!(
                    "RESULT {{\"ok\":false,\"error\":\"{:?} {:?}\"}}",
                    t.status, t.error
                );
                std::process::exit(1);
            }
            _ => {}
        }
    }
}
