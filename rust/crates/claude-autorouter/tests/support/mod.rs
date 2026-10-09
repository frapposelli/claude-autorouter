#![allow(dead_code)]
//! Synthetic executable fixtures. No Node, real home, login or provider service.
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub struct Home(pub PathBuf);
impl Home {
    pub fn new() -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "autorouter native integration {}-{nanos}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    pub fn config(&self) -> PathBuf {
        self.0.join("config.json")
    }
    pub fn write(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        path
    }
    pub fn save(&self, value: &Value) {
        self.write("config.json", serde_json::to_vec(value).unwrap());
    }
    pub fn saved(&self) -> Value {
        serde_json::from_slice(&fs::read(self.config()).unwrap()).unwrap()
    }
    pub fn claude(&self, script: &str) {
        let path = self.write("claude", script);
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    pub fn command(&self) -> Command {
        let executable = std::env::var_os("AUTOROUTER_TEST_EXECUTABLE")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_claude-autorouter").into());
        let mut command = Command::new(executable);
        command
            .current_dir(&self.0)
            .env_clear()
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", &self.0)
            .env("TMPDIR", &self.0)
            .env("PATH", &self.0)
            .env("AUTOROUTER_CONFIG", self.config())
            .env("AUTOROUTER_SECRET_STORE", "file")
            .stdin(Stdio::null());
        command
    }
    pub fn names(&self) -> Vec<String> {
        let mut names = fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        names
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
pub fn quote(path: &Path) -> String {
    format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
}
pub fn output(command: &mut Command) -> Output {
    command
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let read = |mut stream: Box<dyn Read + Send>| {
        let mut bytes = Vec::new();
        stream
            .by_ref()
            .take(2 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    };
    let out = thread::spawn(move || read(Box::new(stdout)));
    let err = thread::spawn(move || read(Box::new(stderr)));
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(child.id() as i32),
                nix::sys::signal::Signal::SIGKILL,
            );
            let _ = child.wait();
            panic!("Synthetic CLI did not exit before deadline")
        }
        thread::sleep(Duration::from_millis(3));
    };
    Output {
        status,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    }
}
pub fn success(output: &Output) -> String {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

#[derive(Clone, Debug)]
pub struct Request {
    pub path: String,
    pub method: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
impl Request {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}
pub struct Response {
    pub status: u16,
    pub kind: &'static str,
    pub body: Vec<u8>,
}
impl Response {
    pub fn json(value: Value) -> Self {
        Self {
            status: 200,
            kind: "application/json",
            body: serde_json::to_vec(&value).unwrap(),
        }
    }
    pub fn sse(body: String) -> Self {
        Self {
            status: 200,
            kind: "text/event-stream",
            body: body.into_bytes(),
        }
    }
}
pub struct Server {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    calls: Arc<Mutex<Vec<Request>>>,
    errors: Arc<Mutex<Vec<&'static str>>>,
}
impl Server {
    pub fn new(handler: impl Fn(&Request) -> Response + Send + Sync + 'static) -> Self {
        Self::with_response(move |request| Some(handler(request)))
    }
    pub fn disconnecting() -> Self {
        Self::with_response(|_| None)
    }
    fn with_response(
        handler: impl Fn(&Request) -> Option<Response> + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let done = stop.clone();
        let recorded = calls.clone();
        let failures = errors.clone();
        let handler = Arc::new(handler);
        let worker = thread::spawn(move || {
            let mut workers = Vec::new();
            while !done.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let recorded = recorded.clone();
                        let failures = failures.clone();
                        let handler = handler.clone();
                        workers.push(thread::spawn(move||{stream.set_nonblocking(false).unwrap();
                            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();let request=match read_request(&mut stream){Ok(request)=>request,Err(error)=>{failures.lock().unwrap().push(error);return}};recorded.lock().unwrap().push(request.clone());let Some(response)=handler(&request) else{return};let header=format!("HTTP/1.1 {} OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",response.status,response.kind,response.body.len());if stream.write_all(header.as_bytes()).and_then(|()|stream.write_all(&response.body)).is_err(){failures.lock().unwrap().push("Synthetic response write failed")}}));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(_) => {
                        failures.lock().unwrap().push("Synthetic listener failed");
                        break;
                    }
                }
            }
            for worker in workers {
                if worker.join().is_err() {
                    failures
                        .lock()
                        .unwrap()
                        .push("Synthetic handler assertion failed")
                }
            }
        });
        Self {
            address,
            stop,
            thread: Some(worker),
            calls,
            errors,
        }
    }
    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }
    pub fn calls(&self) -> Vec<Request> {
        self.calls.lock().unwrap().clone()
    }
    pub fn paths(&self) -> Vec<String> {
        self.calls().into_iter().map(|r| r.path).collect()
    }
    pub fn clear(&self) {
        self.calls.lock().unwrap().clear();
    }
    pub fn assert_clean(&self) {
        let errors = self.errors.lock().unwrap().clone();
        assert!(errors.is_empty(), "Synthetic service failed: {errors:?}");
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.thread.take() {
            worker.join().unwrap();
        }
        if !std::thread::panicking() {
            self.assert_clean();
        }
    }
}
fn read_request(stream: &mut TcpStream) -> Result<Request, &'static str> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let boundary = loop {
        if let Some(at) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
            break at + 4;
        }
        if bytes.len() > 65536 {
            return Err("Synthetic request head exceeded limit");
        }
        let count = stream
            .read(&mut buffer)
            .map_err(|_| "Synthetic head read failed")?;
        if count == 0 {
            return Err("Synthetic head ended early");
        }
        bytes.extend_from_slice(&buffer[..count]);
    };
    let text =
        std::str::from_utf8(&bytes[..boundary]).map_err(|_| "Synthetic head invalid UTF8")?;
    let mut lines = text.split("\r\n");
    let mut first = lines
        .next()
        .ok_or("Synthetic first line missing")?
        .split_whitespace();
    let method = first.next().ok_or("Synthetic method missing")?.to_owned();
    let path = first.next().ok_or("Synthetic path missing")?.to_owned();
    let mut headers = BTreeMap::new();
    for line in lines.filter(|l| !l.is_empty()) {
        let (key, value) = line.split_once(':').ok_or("Synthetic header malformed")?;
        headers.insert(key.to_ascii_lowercase(), value.trim().to_owned());
    }
    if headers.contains_key("transfer-encoding") {
        return Err("Synthetic fixture expected bounded content length");
    }
    let length = headers
        .get("content-length")
        .map(|s| s.parse::<usize>())
        .transpose()
        .map_err(|_| "Synthetic length invalid")?
        .unwrap_or(0);
    if length > 1024 * 1024 {
        return Err("Synthetic body too large");
    }
    while bytes.len() < boundary + length {
        let count = stream
            .read(&mut buffer)
            .map_err(|_| "Synthetic body read failed")?;
        if count == 0 {
            return Err("Synthetic body ended early");
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    Ok(Request {
        path,
        method,
        headers,
        body: bytes[boundary..boundary + length].to_vec(),
    })
}

pub fn hidden_secret(command: &mut Command, label: &str, secret: &str) -> Output {
    hidden_inputs(command, &[(label, HiddenInput::Secret(secret))])
}

pub enum HiddenInput<'a> {
    Secret(&'a str),
    Cancel,
}

pub fn hidden_inputs(command: &mut Command, inputs: &[(&str, HiddenInput<'_>)]) -> Output {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    use nix::pty::openpty;
    use std::fs::File;
    struct PromptChild(std::process::Child, bool);
    impl PromptChild {
        fn clean_group(&mut self) {
            if !self.1 {
                let _ = nix::sys::signal::killpg(
                    nix::unistd::Pid::from_raw(self.0.id() as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
                self.1 = true;
            }
        }
    }
    impl Drop for PromptChild {
        fn drop(&mut self) {
            self.clean_group();
            let _ = self.0.wait();
        }
    }
    // Observe without reaping, so the process-group ID remains reserved until
    // descendants have been stopped. This follows xtask's fixture ownership.
    struct ExitObserver {
        #[cfg(target_os = "macos")]
        queue: nix::sys::event::Kqueue,
        #[cfg(target_os = "macos")]
        exited: bool,
    }
    impl ExitObserver {
        fn new(pid: u32) -> Self {
            #[cfg(target_os = "macos")]
            {
                use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};
                let queue = Kqueue::new().unwrap();
                let event = KEvent::new(
                    pid as usize,
                    EventFilter::EVFILT_PROC,
                    EvFlags::EV_ADD | EvFlags::EV_ONESHOT,
                    FilterFlag::NOTE_EXIT,
                    0,
                    0,
                );
                let exited = match queue.kevent(&[event], &mut [], None) {
                    Ok(_) => false,
                    Err(nix::errno::Errno::ESRCH) => true,
                    Err(error) => panic!("Cannot observe synthetic prompt process: {error}"),
                };
                Self { queue, exited }
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = pid;
                Self {}
            }
        }
        fn exited(&mut self, child: &mut PromptChild) -> bool {
            #[cfg(target_os = "macos")]
            {
                use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent};
                if self.exited {
                    return true;
                }
                let mut events = [KEvent::new(
                    child.0.id() as usize,
                    EventFilter::EVFILT_PROC,
                    EvFlags::empty(),
                    FilterFlag::empty(),
                    0,
                    0,
                )];
                match self.queue.kevent(
                    &[],
                    &mut events,
                    Some(nix::libc::timespec {
                        tv_sec: 0,
                        tv_nsec: 0,
                    }),
                ) {
                    Ok(0) | Err(nix::errno::Errno::EINTR) => false,
                    Ok(1)
                        if !events[0].flags().contains(EvFlags::EV_ERROR)
                            && events[0].ident() == child.0.id() as usize
                            && events[0].fflags().contains(FilterFlag::NOTE_EXIT) =>
                    {
                        self.exited = true;
                        true
                    }
                    _ => panic!("Cannot observe synthetic prompt process"),
                }
            }
            #[cfg(target_os = "linux")]
            {
                use nix::sys::wait::{Id, WaitPidFlag, WaitStatus, waitid};
                match waitid(
                    Id::Pid(nix::unistd::Pid::from_raw(child.0.id() as i32)),
                    WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT,
                ) {
                    Ok(WaitStatus::Exited(_, _) | WaitStatus::Signaled(_, _, _)) => true,
                    Ok(WaitStatus::StillAlive) | Err(nix::errno::Errno::EINTR) => false,
                    Err(nix::errno::Errno::ECHILD) => {
                        child.1 = true;
                        panic!("Synthetic prompt process was reaped outside its fixture");
                    }
                    _ => panic!("Cannot observe synthetic prompt process"),
                }
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                let _ = child;
                panic!("Synthetic prompt observation supports macOS and Linux only");
            }
        }
    }
    fn read_ready(reader: &mut impl Read, output: &mut Vec<u8>, limit: usize, pty: bool) -> bool {
        let mut bytes = [0; 4096];
        match reader.read(&mut bytes) {
            Ok(0) => true,
            Ok(count) => {
                output.extend_from_slice(&bytes[..count]);
                assert!(
                    output.len() <= limit,
                    "Synthetic prompt output exceeded its bound"
                );
                false
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::Interrupted =>
            {
                false
            }
            Err(error) if pty && error.raw_os_error() == Some(5) => true,
            Err(error) => panic!("Synthetic prompt read failed: {error}"),
        }
    }
    let pair = openpty(None, None).unwrap();
    let mut master = File::from(pair.master);
    let slave = File::from(pair.slave);
    let flags = OFlag::from_bits_truncate(fcntl(&master, FcntlArg::F_GETFL).unwrap());
    fcntl(&master, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).unwrap();
    let mut child = PromptChild(
        command
            .process_group(0)
            .stdin(slave.try_clone().unwrap())
            .stderr(slave)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
        false,
    );
    // Command retains its configured files after spawn. Release those parent
    // slave handles so the PTY can reach EOF after the owned child group exits.
    command.stdin(Stdio::null()).stderr(Stdio::null());
    let mut child_stdout = child.0.stdout.take().unwrap();
    let mut observer = ExitObserver::new(child.0.id());
    let flags = OFlag::from_bits_truncate(fcntl(&child_stdout, FcntlArg::F_GETFL).unwrap());
    fcntl(&child_stdout, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).unwrap();
    let mut stdout = Vec::new();
    let mut visible = Vec::new();
    let mut stdout_closed = false;
    let mut terminal_closed = false;
    let mut exited = None;
    let mut sent = 0;
    let mut prompt_start = 0;
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if !terminal_closed {
            terminal_closed = read_ready(&mut master, &mut visible, 65536, true);
        }
        if !stdout_closed {
            stdout_closed = read_ready(&mut child_stdout, &mut stdout, 2 * 1024 * 1024, false);
        }
        if let Some((label, input)) = inputs.get(sent) {
            let prompt = format!("{label} (hidden): ");
            if let Some(at) = visible[prompt_start..]
                .windows(prompt.len())
                .position(|bytes| bytes == prompt.as_bytes())
            {
                match input {
                    HiddenInput::Secret(secret) => {
                        master.write_all(format!("{secret}\r").as_bytes()).unwrap();
                    }
                    HiddenInput::Cancel => master.write_all(b"\x03").unwrap(),
                }
                prompt_start += at + prompt.len();
                sent += 1;
            }
        }
        if exited.is_none() && observer.exited(&mut child) {
            child.clean_group();
            exited = Some(child.0.wait().unwrap());
        }
        if let Some(status) = exited
            && stdout_closed
            && terminal_closed
        {
            break status;
        }
        if Instant::now() >= deadline {
            panic!(
                "Synthetic prompt timed out after {sent} of {} inputs",
                inputs.len()
            );
        }
        std::thread::sleep(Duration::from_millis(3));
    };
    assert_eq!(
        sent,
        inputs.len(),
        "Requested secrets were not all prompted"
    );
    for (_, input) in inputs {
        if let HiddenInput::Secret(secret) = input
            && !secret.is_empty()
        {
            assert!(!String::from_utf8_lossy(&visible).contains(*secret));
        }
    }
    Output {
        status,
        stdout,
        stderr: visible,
    }
}
