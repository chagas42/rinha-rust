use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use memmap2::Mmap;
use std::cell::RefCell;
use std::convert::Infallible;
use std::fs::File;
use std::net::SocketAddr;
use std::sync::OnceLock;
use tokio::net::{TcpListener, UnixListener};

use rinha_2026::{ivf_search, vectorize, FraudRequest, IndexView};

const READY_BODY: &[u8] = b"ok";

const RESPONSES: [&[u8]; 6] = [
    br#"{"approved":true,"fraud_score":0.0}"#,
    br#"{"approved":true,"fraud_score":0.2}"#,
    br#"{"approved":true,"fraud_score":0.4}"#,
    br#"{"approved":false,"fraud_score":0.6}"#,
    br#"{"approved":false,"fraud_score":0.8}"#,
    br#"{"approved":false,"fraud_score":1.0}"#,
];

static INDEX: OnceLock<IndexView<'static>> = OnceLock::new();
const NPROBE: usize = 32;
const SIMD_JSON_PADDING: usize = 32;

thread_local! {
    static JSON_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

async fn handle(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/ready") => Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Full::new(Bytes::from_static(READY_BODY)))
            .unwrap()),

        (&Method::POST, "/fraud-score") => {
            let body_bytes = match req.collect().await {
                Ok(c) => c.to_bytes(),
                Err(_) => return Ok(error_response(StatusCode::BAD_REQUEST)),
            };

            let frauds = JSON_BUF.with(|cell| {
                let mut buf = cell.borrow_mut();
                buf.clear();
                buf.reserve(body_bytes.len() + SIMD_JSON_PADDING);
                buf.extend_from_slice(&body_bytes);
                buf.resize(body_bytes.len() + SIMD_JSON_PADDING, 0);
                let parse_len = body_bytes.len();

                let parsed: FraudRequest = simd_json::serde::from_slice(&mut buf[..parse_len]).ok()?;
                let v = vectorize(&parsed);
                let idx = INDEX.get().expect("index not loaded");
                Some(ivf_search(idx, &v, NPROBE))
            });

            let Some(frauds) = frauds else {
                return Ok(error_response(StatusCode::BAD_REQUEST));
            };

            let body = Bytes::from_static(RESPONSES[frauds as usize]);

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "application/json")
                .body(Full::new(body))
                .unwrap())
        }

        _ => Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::new()))
            .unwrap()),
    }
}

fn error_response(status: StatusCode) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::new()))
        .unwrap()
}

fn load_index_static(path: &str) -> IndexView<'static> {
    let file = File::open(path).expect("open index.bin");
    let mmap = unsafe { Mmap::map(&file).expect("mmap index.bin") };
    let leaked: &'static Mmap = Box::leak(Box::new(mmap));
    IndexView::from_bytes(leaked.as_ref()).expect("parse index.bin")
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let index_path =
        std::env::var("INDEX_PATH").unwrap_or_else(|_| "/app/index.bin".to_string());

    eprintln!("rinha-2026 loading index from {}", index_path);
    let idx = load_index_static(&index_path);
    eprintln!(
        "rinha-2026 index loaded: {} vectors, {} centroids",
        idx.vectors.len(),
        idx.centroids.len()
    );
    INDEX.set(idx).map_err(|_| "INDEX already set")?;

    if let Ok(uds_path) = std::env::var("UDS_PATH") {
        let _ = std::fs::remove_file(&uds_path);
        let listener = UnixListener::bind(&uds_path)?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&uds_path, std::fs::Permissions::from_mode(0o666))?;
        eprintln!("rinha-2026 listening on uds://{}", uds_path);
        loop {
            let (stream, _) = listener.accept().await?;
            let io = TokioIo::new(stream);
            tokio::task::spawn(async move {
                if let Err(err) = http1::Builder::new()
                    .serve_connection(io, service_fn(handle))
                    .await
                {
                    eprintln!("connection error: {:?}", err);
                }
            });
        }
    } else {
        let port: u16 = std::env::var("PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(8080);
        let addr: SocketAddr = ([0, 0, 0, 0], port).into();
        let listener = TcpListener::bind(addr).await?;
        eprintln!("rinha-2026 listening on http://{}", addr);
        loop {
            let (stream, _) = listener.accept().await?;
            let io = TokioIo::new(stream);
            tokio::task::spawn(async move {
                if let Err(err) = http1::Builder::new()
                    .serve_connection(io, service_fn(handle))
                    .await
                {
                    eprintln!("connection error: {:?}", err);
                }
            });
        }
    }
}
