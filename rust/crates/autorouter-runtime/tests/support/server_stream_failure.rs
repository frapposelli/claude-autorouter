//! Bounded raw HTTP provider: finite response, deliberate truncation or held stream.
//! Intended truncation is recorded separately from unexpected fixture failures.
use hyper::HeaderMap;
use serde_json::Value;
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

const LIMIT: usize = 2 * 1024 * 1024;
const BOUND: Duration = Duration::from_secs(5);
#[derive(Clone, Debug)]
pub struct Observation {
    pub method: String,
    pub path: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}
#[derive(Default)]
struct State {
    calls: Mutex<Vec<Observation>>,
    faults: Mutex<Vec<String>>,
    destroyed: AtomicBool,
    peer_closed: AtomicBool,
}
pub struct Peer {
    pub address: SocketAddr,
    state: Arc<State>,
    stop: CancellationToken,
    destroy: CancellationToken,
    owner: Option<JoinHandle<()>>,
}
impl Peer {
    pub async fn start(number: usize, script: Vec<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let state = Arc::new(State::default());
        let stop = CancellationToken::new();
        let destroy = CancellationToken::new();
        let (shared, closing, abrupt) = (state.clone(), stop.clone(), destroy.clone());
        let owner = tokio::spawn(async move {
            let accepted =
                tokio::select! {_ = closing.cancelled()=>return,result=listener.accept()=>result};
            let (mut socket, _) = match accepted {
                Ok(value) => value,
                Err(_) => {
                    shared
                        .faults
                        .lock()
                        .unwrap()
                        .push("provider accept failed".into());
                    return;
                }
            };
            let observed = tokio::time::timeout(BOUND, read_request(&mut socket)).await;
            let observed = match observed {
                Ok(Ok(value)) => value,
                _ => {
                    shared
                        .faults
                        .lock()
                        .unwrap()
                        .push("bounded provider request read failed".into());
                    return;
                }
            };
            shared.calls.lock().unwrap().push(observed);
            if number == 28 {
                shared.faults.lock().unwrap().push(
                    "server#28:assert-1: Cancelled classification must not reach upstream".into(),
                );
                return;
            }
            let row = &script[0];
            let status = row["response_status"].as_u64().unwrap();
            let mut head = format!("HTTP/1.1 {status} synthetic\r\nTransfer-Encoding: chunked\r\n");
            for (key, value) in row["response_headers"].as_object().unwrap() {
                head.push_str(&format!("{key}: {}\r\n", value.as_str().unwrap()));
            }
            head.push_str("\r\n");
            if socket.write_all(head.as_bytes()).await.is_err() {
                shared
                    .faults
                    .lock()
                    .unwrap()
                    .push("provider response head write failed".into());
                return;
            }
            for write in row["response_writes"].as_array().unwrap() {
                let bytes =
                    openssl::base64::decode_block(write["base64"].as_str().unwrap()).unwrap();
                if bytes.is_empty() {
                    continue;
                }
                let chunk = format!("{:x}\r\n", bytes.len());
                if socket.write_all(chunk.as_bytes()).await.is_err()
                    || socket.write_all(&bytes).await.is_err()
                    || socket.write_all(b"\r\n").await.is_err()
                {
                    shared
                        .faults
                        .lock()
                        .unwrap()
                        .push("provider prefix write failed".into());
                    return;
                }
            }
            if number == 26 {
                if socket.write_all(b"0\r\n\r\n").await.is_err() {
                    shared
                        .faults
                        .lock()
                        .unwrap()
                        .push("provider terminal write failed".into());
                }
                return;
            }
            let mut one = [0u8; 1];
            tokio::select! {
                _ = closing.cancelled()=>{},
                _ = abrupt.cancelled()=>{shared.destroyed.store(true,Ordering::SeqCst);},
                _ = listener.accept()=>{shared.faults.lock().unwrap().push("unexpected additional provider connection".into());},
                result=socket.read(&mut one)=>{if matches!(result,Ok(0)|Err(_)){shared.peer_closed.store(true,Ordering::SeqCst);}else{shared.faults.lock().unwrap().push("unexpected provider client data".into());}}
            }
            // Dropping a socket without the terminal chunk causes a genuine
            // HTTP body error. No manufactured client/body result is returned.
        });
        Self {
            address,
            state,
            stop,
            destroy,
            owner: Some(owner),
        }
    }
    pub fn disconnect(&self) {
        self.destroy.cancel();
    }
    pub fn destroyed(&self) -> bool {
        self.state.destroyed.load(Ordering::SeqCst)
    }
    pub fn peer_closed(&self) -> bool {
        self.state.peer_closed.load(Ordering::SeqCst)
    }
    pub fn observations(&self) -> Vec<Observation> {
        self.state.calls.lock().unwrap().clone()
    }
    pub async fn close(mut self) -> Result<(), String> {
        self.stop.cancel();
        let mut owner = self.owner.take().unwrap();
        match tokio::time::timeout(BOUND, &mut owner).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => return Err("provider owner task failed".into()),
            Err(_) => {
                owner.abort();
                let _ = owner.await;
                return Err("provider cleanup timed out".into());
            }
        }
        let faults = self.state.faults.lock().unwrap().clone();
        if faults.is_empty() {
            Ok(())
        } else {
            Err(faults.join("; "))
        }
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
async fn read_request(socket: &mut TcpStream) -> Result<Observation, String> {
    let mut bytes = Vec::new();
    let head_end;
    loop {
        if let Some(index) = bytes.windows(4).position(|chunk| chunk == b"\r\n\r\n") {
            head_end = index + 4;
            break;
        }
        let mut chunk = [0u8; 1024];
        let length = socket
            .read(&mut chunk)
            .await
            .map_err(|_| "request head read failed")?;
        if length == 0 || bytes.len() + length > 8192 {
            return Err("request head incomplete or oversized".into());
        }
        bytes.extend_from_slice(&chunk[..length]);
    }
    let head = std::str::from_utf8(&bytes[..head_end]).map_err(|_| "request head not UTF-8")?;
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap().split(' ');
    let method = first.next().unwrap().to_owned();
    let path = first.next().ok_or("request path absent")?.to_owned();
    let mut headers = HeaderMap::new();
    for line in lines.take_while(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or("invalid request field")?;
        headers.append(
            hyper::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| "invalid request name")?,
            value.trim().parse().map_err(|_| "invalid request value")?,
        );
    }
    let length = headers
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("0")
        .parse::<usize>()
        .map_err(|_| "invalid request length")?;
    if length > LIMIT || headers.contains_key("transfer-encoding") {
        return Err("unexpected request body framing or limit".into());
    }
    let mut body = bytes.split_off(head_end);
    while body.len() < length {
        let mut chunk = [0u8; 1024];
        let got = socket
            .read(&mut chunk)
            .await
            .map_err(|_| "request body read failed")?;
        if got == 0 || body.len() + got > length {
            return Err("request body incomplete or contains extra bytes".into());
        }
        body.extend_from_slice(&chunk[..got]);
    }
    if body.len() != length {
        return Err("extra request bytes".into());
    }
    Ok(Observation {
        method,
        path,
        headers,
        body,
    })
}
