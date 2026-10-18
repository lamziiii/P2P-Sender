//! Sending files to a LocalSend device: `prepare-upload` lists the files and
//! waits for the user of the device to answer, then one `upload` per
//! accepted file, one after the other, streamed from the disk.
//!
//! Stopping a file from our side drops its upload, which fails it on the
//! device. Accepted files never started would keep the device waiting: the
//! session is then cancelled there.

use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::BytesMut;
use http_body_util::combinators::BoxBody;
use hyper::body::{Frame, SizeHint};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};

use super::*;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CANCEL_TIMEOUT: Duration = Duration::from_secs(3);
/// How often we look whether the user stopped every file of a request.
const STOP_CHECK: Duration = Duration::from_millis(300);
const CHUNK: usize = 256 * 1024;

pub(super) type Body = BoxBody<Bytes, io::Error>;

pub(super) fn json_body(value: &Value) -> Body {
    Full::new(Bytes::from(value.to_string()))
        .map_err(|never: Infallible| match never {})
        .boxed()
}

fn bytes_body(bytes: Bytes) -> Body {
    Full::new(bytes)
        .map_err(|never: Infallible| match never {})
        .boxed()
}

fn empty_body() -> Body {
    Full::new(Bytes::new())
        .map_err(|never: Infallible| match never {})
        .boxed()
}

/// Where to reach a device, and the fingerprint its certificate must have.
pub(super) struct Peer {
    pub ip: IpAddr,
    pub port: u16,
    pub https: bool,
    pub pin: Option<String>,
}

impl Peer {
    fn of(device: &Device) -> Peer {
        Peer {
            ip: device.ip,
            port: device.port,
            https: device.https,
            pin: device.https.then(|| device.fingerprint.clone()),
        }
    }
}

trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

/// An upload body fed from the disk, with its exact length announced.
struct FileBody {
    rx: mpsc::Receiver<io::Result<Bytes>>,
    size: u64,
}

impl hyper::body::Body for FileBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        self.rx
            .poll_recv(cx)
            .map(|chunk| chunk.map(|r| r.map(Frame::data)))
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.size)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrepareReply {
    session_id: String,
    /// Token per accepted file.
    #[serde(default)]
    files: HashMap<String, String>,
}

/// LocalSend decides from it where a file goes (photos to the gallery…).
fn mime_type(name: &str) -> &'static str {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "zip" => "application/zip",
        "apk" => "application/vnd.android.package-archive",
        _ => "application/octet-stream",
    }
}

fn unreachable(e: &io::Error) -> String {
    match e.kind() {
        io::ErrorKind::TimedOut => "Appareil injoignable (délai dépassé)".into(),
        io::ErrorKind::ConnectionRefused => "Appareil injoignable (LocalSend fermé ?)".into(),
        _ => format!("Connexion à l’appareil impossible : {e}"),
    }
}

fn refusal(status: StatusCode) -> String {
    match status.as_u16() {
        401 => "L’appareil demande un code PIN (non pris en charge)".into(),
        409 => "L’appareil est occupé par un autre transfert".into(),
        429 => "Trop de demandes : réessayez dans un moment".into(),
        code => format!("Refusé par l’appareil (erreur {code})"),
    }
}

fn query(pairs: &[(&str, &str)]) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in pairs {
        query.append_pair(key, value);
    }
    query.finish()
}

/// Stream a file into an upload body; fails if the file cannot be read in
/// full. Stops quietly if the request ends first.
async fn read_file(file: &OutgoingFile, tx: mpsc::Sender<io::Result<Bytes>>) -> io::Result<()> {
    let result = async {
        let mut f = tokio::fs::File::open(&file.path).await?;
        let mut sent = 0;
        while sent < file.size {
            let mut buf = BytesMut::with_capacity(CHUNK.min((file.size - sent) as usize));
            if f.read_buf(&mut buf).await? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "fichier raccourci",
                ));
            }
            sent += buf.len() as u64;
            if tx.send(Ok(buf.freeze())).await.is_err() {
                return Ok(());
            }
            file.set_sent(sent);
        }
        Ok(())
    }
    .await;
    if let Err(e) = &result {
        let _ = tx.send(Err(io::Error::new(e.kind(), e.to_string()))).await;
    }
    result
}

/// Resolves once the upload of `file` made no progress for `IDLE_TIMEOUT`
/// (a device that stops reading, or never answers once it has everything).
async fn stalled(file: &OutgoingFile) {
    let mut last = (file.sent(), Instant::now());
    loop {
        tokio::time::sleep(SESSION_CHECK).await;
        let sent = file.sent();
        if sent != last.0 {
            last = (sent, Instant::now());
        } else if last.1.elapsed() >= IDLE_TIMEOUT {
            return;
        }
    }
}

impl Inner {
    /// One POST on a new connection: the status, the response body (up to
    /// `MAX_JSON`) and the fingerprint of the server's certificate.
    pub(super) async fn request(
        &self,
        peer: &Peer,
        path: &str,
        content_type: &str,
        body: Body,
    ) -> io::Result<(StatusCode, Bytes, Option<String>)> {
        let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((peer.ip, peer.port)))
            .await
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
        let _ = tcp.set_nodelay(true);
        let (stream, cert): (Box<dyn Io>, Option<String>) = if peer.https {
            let connector = match &peer.pin {
                Some(pin) => tls_connector(&self.identity, Some(pin)).map_err(io::Error::other)?,
                None => self.connector.clone(),
            };
            let tls = connector
                .connect(ServerName::IpAddress(peer.ip.into()), tcp)
                .await?;
            let cert = tls
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|c| c.first())
                .map(|c| fingerprint(c));
            (Box::new(tls), cert)
        } else {
            (Box::new(tcp), None)
        };
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(io::Error::other)?;
        let req = Request::post(path)
            .header(HOST, SocketAddr::new(peer.ip, peer.port).to_string())
            .header(CONTENT_TYPE, content_type)
            .body(body)
            .map_err(io::Error::other)?;
        // The sender is dropped with the exchange, which lets the connection end.
        let exchange = async move {
            let res = sender.send_request(req).await.map_err(io::Error::other)?;
            let status = res.status();
            let body = Limited::new(res.into_body(), MAX_JSON)
                .collect()
                .await
                .map_err(io::Error::other)?
                .to_bytes();
            Ok::<_, io::Error>((status, body, cert))
        };
        let (result, _) = tokio::join!(exchange, conn);
        result
    }

    pub(super) async fn send_session(self: Arc<Self>, device: Device, files: Vec<OutgoingFile>) {
        let tids: Vec<String> = files.iter().map(|f| f.tid.clone()).collect();
        let _guard = StopGuard {
            node: self.node.clone(),
            tids: tids.clone(),
            status: Status::Cancelled,
            error: "LocalSend désactivé",
            armed: true,
        };
        let peer = Peer::of(&device);
        let list: serde_json::Map<String, Value> = files
            .iter()
            .map(|f| {
                let info = json!({
                    "id": f.tid,
                    "fileName": f.name,
                    "size": f.size,
                    "fileType": mime_type(&f.name),
                    "sha256": null,
                    "preview": null,
                });
                (f.tid.clone(), info)
            })
            .collect();
        let body = json_body(&json!({ "info": self.advertisement(None), "files": list }));
        let path = format!("{API}/prepare-upload");
        let prepare = self.request(&peer, &path, "application/json", body);
        let answer = tokio::select! {
            answer = tokio::time::timeout(DECISION_TIMEOUT, prepare) => answer.unwrap_or_else(|_| {
                Err(io::Error::new(io::ErrorKind::TimedOut, "pas de réponse"))
            }),
            () = self.all_stopped(&tids) => {
                // Until it answers, the device knows the request by our address.
                self.cancel_remote(&peer, None).await;
                return;
            }
        };
        let stop_all = |status, error: Option<&str>| {
            for tid in &tids {
                self.node.external_stop(tid, status, error);
            }
        };
        let reply = match answer {
            Ok((StatusCode::OK, body, _)) => match serde_json::from_slice::<PrepareReply>(&body) {
                Ok(reply) => reply,
                Err(_) => return stop_all(Status::Error, Some("Réponse invalide de l’appareil")),
            },
            // Nothing to transfer: the device already has everything.
            Ok((StatusCode::NO_CONTENT, ..)) => {
                for tid in &tids {
                    self.node.external_sent(tid);
                }
                return;
            }
            Ok((StatusCode::FORBIDDEN, ..)) => return stop_all(Status::Declined, None),
            Ok((status, ..)) => return stop_all(Status::Error, Some(&refusal(status))),
            Err(e) if e.kind() == io::ErrorKind::TimedOut => {
                return stop_all(Status::Error, Some("Pas de réponse de l’appareil"))
            }
            Err(e) => return stop_all(Status::Error, Some(&unreachable(&e))),
        };

        let mut unstarted = false;
        for file in &files {
            let Some(token) = reply.files.get(&file.tid) else {
                self.node.external_stop(&file.tid, Status::Declined, None);
                continue;
            };
            if !self.node.external_sending(&file.tid) {
                unstarted = true;
                continue;
            }
            let path = format!(
                "{API}/upload?{}",
                query(&[
                    ("sessionId", &reply.session_id),
                    ("fileId", &file.tid),
                    ("token", token),
                ])
            );
            let (tx, rx) = mpsc::channel(4);
            let body = FileBody {
                rx,
                size: file.size,
            }
            .boxed();
            let upload = async {
                tokio::join!(
                    read_file(file, tx),
                    self.request(&peer, &path, "application/octet-stream", body)
                )
            };
            let result = tokio::select! {
                result = upload => result,
                // Already stopped by the user: dropping the upload is enough.
                () = file.cancelled() => continue,
                () = stalled(file) => {
                    self.node.external_stop(
                        &file.tid,
                        Status::Error,
                        Some("L’appareil ne répond plus"),
                    );
                    continue;
                }
            };
            let failure = match result {
                (Err(_), _) => Some((Status::Error, "Lecture du fichier impossible".to_string())),
                (_, Ok((StatusCode::OK, ..))) => None,
                (_, Ok((StatusCode::FORBIDDEN | StatusCode::CONFLICT, ..))) => {
                    Some((Status::Cancelled, "Annulé par le destinataire".to_string()))
                }
                (_, Ok((StatusCode::UNPROCESSABLE_ENTITY, ..))) => {
                    Some((Status::Error, "Fichier corrompu en route".to_string()))
                }
                (_, Ok((status, ..))) => Some((
                    Status::Error,
                    format!("Erreur de l’appareil (erreur {})", status.as_u16()),
                )),
                (_, Err(_)) => Some((
                    Status::Error,
                    "Connexion perdue avec l’appareil".to_string(),
                )),
            };
            match failure {
                None => self.node.external_sent(&file.tid),
                Some((status, error)) => self.node.external_stop(&file.tid, status, Some(&error)),
            }
        }
        if unstarted {
            self.cancel_remote(&peer, Some(&reply.session_id)).await;
        }
    }

    /// A text message, as LocalSend sends one: a lone text file whose
    /// preview is the text. Devices show it without any upload; one that
    /// asks for the file gets it.
    pub(super) async fn send_text(&self, device: Device, text: String) -> Result<(), String> {
        let peer = Peer::of(&device);
        let id = uuid::Uuid::new_v4().to_string();
        let file = json!({
            "id": id,
            "fileName": format!("{id}.txt"),
            "size": text.len(),
            "fileType": "text/plain",
            "sha256": null,
            "preview": text,
        });
        let body = json_body(&json!({
            "info": self.advertisement(None),
            "files": { id.clone(): file },
        }));
        let path = format!("{API}/prepare-upload");
        let prepare = self.request(&peer, &path, "application/json", body);
        let answer = tokio::time::timeout(DECISION_TIMEOUT, prepare)
            .await
            .map_err(|_| "Pas de réponse de l’appareil".to_string())?;
        let reply = match answer {
            Ok((StatusCode::NO_CONTENT, ..)) => return Ok(()),
            Ok((StatusCode::OK, body, _)) => serde_json::from_slice::<PrepareReply>(&body)
                .map_err(|_| "Réponse invalide de l’appareil".to_string())?,
            Ok((StatusCode::FORBIDDEN, ..)) => return Err("Refusé par l’appareil".into()),
            Ok((status, ..)) => return Err(refusal(status)),
            Err(e) => return Err(unreachable(&e)),
        };
        let Some(token) = reply.files.get(&id) else {
            return Err("Refusé par l’appareil".into());
        };
        let path = format!(
            "{API}/upload?{}",
            query(&[
                ("sessionId", &reply.session_id),
                ("fileId", &id),
                ("token", token),
            ])
        );
        let body = bytes_body(Bytes::from(text));
        match self.request(&peer, &path, "text/plain", body).await {
            Ok((StatusCode::OK, ..)) => Ok(()),
            Ok((status, ..)) => Err(format!("Erreur de l’appareil (erreur {})", status.as_u16())),
            Err(e) => Err(unreachable(&e)),
        }
    }

    /// Resolves once every one of `tids` is over.
    async fn all_stopped(&self, tids: &[String]) {
        loop {
            tokio::time::sleep(STOP_CHECK).await;
            if self.node.external_progress(tids).1 {
                return;
            }
        }
    }

    async fn cancel_remote(&self, peer: &Peer, session_id: Option<&str>) {
        let path = match session_id {
            Some(id) => format!("{API}/cancel?{}", query(&[("sessionId", id)])),
            None => format!("{API}/cancel"),
        };
        let request = self.request(peer, &path, "application/json", empty_body());
        let _ = tokio::time::timeout(CANCEL_TIMEOUT, request).await;
    }
}
