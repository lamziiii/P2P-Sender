//! Headless transfer benchmark using the app's P2P core.
//!
//!   bench recv <workdir>          wait for an offer, accept it, report
//!   bench send <workdir> <file>...  send the files to the receiver, report
//!
//! With several files, BENCH_COUNT tells the receiver how many to wait for;
//! the result covers all of them. A single folder is sent as a folder
//! (BENCH_COUNT is then its number of files), which the receiver accepts.
//! Both sides publish their ID in <workdir>/<role>.id and read the other's,
//! so the two roles can run as separate processes (see bench/run.ps1).
//! With BENCH_NO_LAN set, the local-network fast path is off (QUIC only).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::SecretKey;
use p2pshare_lib::localsend::Identity;
use p2pshare_lib::p2p::{Event, Node, Options, Status};
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
    let files: Vec<PathBuf> = args.iter().skip(2).map(PathBuf::from).collect();
    let expected = std::env::var("BENCH_COUNT")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(if sending { files.len() } else { 1 });
    if sending && files.is_empty() {
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
        invites: JsonStore::load(work.join(format!("{role}-v3-invites.json")), Vec::new()),
        messages_dir: work.join(format!("{role}-v3-messages")),
        download_dir: Arc::new(move || out.clone()),
        on_event: Arc::new(move |e| {
            let _ = tx.send(e);
        }),
        groups_dir: work.join(format!("{role}-v3-groups")),
        nickname: role.clone(),
        relay: None,
        lan: std::env::var_os("BENCH_NO_LAN").is_none().then(|| {
            Identity::load_or_create(&work.join(format!("{role}-lan.json"))).expect("lan identity")
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
    let mut via_lan = false;
    let mut route = None;
    let mut done: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut total_bytes = 0u64;
    loop {
        let event = tokio::time::timeout(CONNECT_TIMEOUT, events.recv()).await;
        let Ok(Some(event)) = event else {
            for t in node.transfers().iter().filter(|t| t.status != Status::Completed) {
                println!(
                    "[v3 {role}] en attente : {} {:?} {}%",
                    t.file_name, t.status, t.progress
                );
            }
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
                    if files.len() == 1 && files[0].is_dir() {
                        node.send_folder(&peer, &files[0]).expect("folder");
                    } else {
                        node.send_files(&peer, &files);
                    }
                }
            }
            Event::FileOffer(t, _) => {
                transfer_start.get_or_insert_with(Instant::now);
                match &t.folder {
                    Some(f) => {
                        node.respond_to_folder(&t.friend_id, &f.id, true);
                    }
                    None => node.respond_to_offer(&t.id, true),
                }
            }
            Event::Transfer(t) if t.lan && !via_lan => {
                via_lan = true;
                println!("[v3 {role}] via le réseau local");
            }
            Event::Transfer(t) if t.route.is_some() && route.is_none() => {
                route = t.route;
                println!("[v3 {role}] chemin : {}", t.route.unwrap_or_default());
            }
            Event::Transfer(t) if t.status == Status::Completed => {
                if done.insert(t.id.clone()) {
                    total_bytes += t.file_size;
                }
                if done.len() < expected {
                    continue;
                }
                let secs = transfer_start
                    .map(|s| s.elapsed().as_secs_f64())
                    .unwrap_or(0.0);
                let mbps = total_bytes as f64 / 1_048_576.0 / secs.max(1e-9);
                println!(
                    "RESULT {{\"ok\":true,\"role\":\"{role}\",\"files\":{},\"bytes\":{},\"seconds\":{secs:.3},\"mb_per_s\":{mbps:.1},\"total_s\":{:.3}}}",
                    done.len(),
                    total_bytes,
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
