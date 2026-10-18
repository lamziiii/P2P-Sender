//! Send files to a LocalSend device, to test against a real LocalSend app
//! or CLI.
//!
//!   cargo run --example localsend_send -- <workdir> <device name> <file>... [--cancel-at <percent>]
//!   cargo run --example localsend_send -- <workdir> <device name> --text <message>
//!
//! Looks for the device, sends the files and prints each transfer as it
//! changes until they are all over. `--cancel-at` cancels each file once
//! that much of it is sent (0: while the device has not answered yet, after
//! a few seconds).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iroh::SecretKey;
use p2pshare_lib::localsend::{Hooks, Identity, LocalSend};
use p2pshare_lib::p2p::{Messages, Node, Options, Status};
use p2pshare_lib::store::JsonStore;

const FIND_TIMEOUT: Duration = Duration::from_secs(20);
const WAIT_BEFORE_CANCEL: Duration = Duration::from_secs(3);

#[tokio::main]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let cancel_at = args.iter().position(|a| a == "--cancel-at").map(|i| {
        let percent: u32 = args[i + 1].parse().expect("--cancel-at <percent>");
        args.drain(i..i + 2);
        percent
    });
    let text = args.iter().position(|a| a == "--text").map(|i| {
        let text = args[i + 1].clone();
        args.drain(i..i + 2);
        text
    });
    let [work, device, files @ ..] = args.as_slice() else {
        panic!("usage: localsend_send <workdir> <device name> <file>... [--cancel-at <percent>]");
    };
    let work = PathBuf::from(work);
    let files: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
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
        on_text: Arc::new(|_, _| {}),
        on_devices: Arc::new(|| {}),
    };
    let service = LocalSend::start(node.clone(), &id, "P2P Share envoi", hooks)
        .await
        .expect("localsend");

    service.scan();
    let mut start = Instant::now();
    let target = loop {
        if let Some(d) = service.devices().into_iter().find(|d| &d.alias == device) {
            break d;
        }
        if start.elapsed() > FIND_TIMEOUT {
            let seen: Vec<String> = service.devices().into_iter().map(|d| d.alias).collect();
            panic!("« {device} » introuvable (vus : {seen:?})");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    println!(
        "Trouvé « {} » ({:?}, {:?}) en {:.1}s",
        target.alias,
        target.device_model,
        target.device_type,
        start.elapsed().as_secs_f64()
    );
    if let Some(text) = text {
        let (tx, rx) = tokio::sync::oneshot::channel();
        service
            .send_text(&target.fingerprint, text, move |result| {
                let _ = tx.send(result);
            })
            .expect("send");
        println!("Texte : {:?}", rx.await.expect("outcome"));
        drop(service);
        node.shutdown().await;
        return;
    }
    let count = service
        .send_files(&target.fingerprint, &files)
        .expect("send");
    println!("{count} fichier(s) proposé(s)");
    start = Instant::now();

    let mut seen = HashMap::new();
    loop {
        let transfers = node.transfers();
        for t in &transfers {
            let line = format!(
                "{:?} {}%{}",
                t.status,
                t.progress,
                t.error
                    .as_deref()
                    .map(|e| format!(" ({e})"))
                    .unwrap_or_default()
            );
            if seen.get(&t.id) != Some(&line) {
                println!("{} ({} o) : {line}", t.file_name, t.file_size);
                seen.insert(t.id.clone(), line);
            }
            let waited = t.status == Status::WaitingConsent && start.elapsed() > WAIT_BEFORE_CANCEL;
            if cancel_at.is_some_and(|p| {
                (t.status == Status::Sending && t.progress >= p) || (p == 0 && waited)
            }) {
                node.cancel_transfer(&t.id);
            }
        }
        if !transfers.is_empty() && transfers.iter().all(|t| t.status.is_final()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Lets a cancellation reach the device.
    tokio::time::sleep(Duration::from_secs(1)).await;
    drop(service);
    node.shutdown().await;
}
