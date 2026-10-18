//! A node with LocalSend on, to test against a real LocalSend app or CLI.
//!
//!   cargo run --example localsend_receiver -- <workdir> [seconds]
//!
//! Accepts every offered file, except those whose name contains "refuse"
//! (declined) or "attente" (left unanswered), cancels those whose name
//! contains "annule" midway, and prints each transfer as it changes. Files land in <workdir>/downloads.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::SecretKey;
use p2pshare_lib::localsend::{Hooks, Identity, LocalSend};
use p2pshare_lib::p2p::{Messages, Node, Options, Status};
use p2pshare_lib::store::JsonStore;

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let work = PathBuf::from(
        args.next()
            .expect("usage: localsend_receiver <workdir> [seconds]"),
    );
    let duration = Duration::from_secs(args.next().map_or(120, |s| s.parse().expect("seconds")));
    std::fs::create_dir_all(&work).unwrap();
    let downloads = work.join("downloads");
    let node = Node::start(Options {
        secret: SecretKey::generate(),
        friends: JsonStore::load(work.join("friends.json"), Vec::new()),
        messages: JsonStore::load(work.join("messages.json"), Messages::new()),
        download_dir: Arc::new(move || downloads.clone()),
        on_event: Arc::new(|_| {}),
        groups_dir: work.join("groups"),
        nickname: "test".into(),
        relay: None,
        lan: None,
    })
    .await
    .expect("node");
    let id = Identity::load_or_create(&work.join("localsend.json")).expect("identity");
    let hooks = Hooks {
        on_text: Arc::new(|from, text| println!("Message de {} : {text}", from.alias)),
        on_devices: Arc::new(|| {}),
    };
    let service = LocalSend::start(node.clone(), &id, "P2P Share test", hooks)
        .await
        .expect("localsend");
    let status = service.status();
    println!(
        "LocalSend « {} » sur le port {} ({}){}",
        status.alias,
        status.port,
        &status.fingerprint[..16],
        status.error.map(|e| format!(" — {e}")).unwrap_or_default()
    );

    let start = Instant::now();
    let mut seen = HashMap::new();
    while start.elapsed() < duration {
        for t in node.transfers() {
            let line = format!(
                "{:?} {}% {}{}",
                t.status,
                t.progress,
                t.saved_path.as_deref().unwrap_or(""),
                t.error
                    .as_deref()
                    .map(|e| format!(" ({e})"))
                    .unwrap_or_default()
            );
            if seen.get(&t.id) != Some(&line) {
                println!(
                    "{} de {} ({} o) : {line}",
                    t.file_name,
                    t.peer_name.as_deref().unwrap_or("?"),
                    t.file_size
                );
                seen.insert(t.id.clone(), line);
            }
            if t.status == Status::Pending && !t.file_name.contains("attente") {
                node.respond_to_offer(&t.id, !t.file_name.contains("refuse"));
            }
            if t.status == Status::Receiving && t.progress >= 5 && t.file_name.contains("annule") {
                node.cancel_transfer(&t.id);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    drop(service);
    node.shutdown().await;
}
