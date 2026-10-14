//! Shared bounded privacy subprocess; all callers retain their exact markers.
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn assert_private_child(
    name: &str,
    private: &str,
    public: &str,
    expected_public: usize,
    timeout_message: &str,
) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--ignored", "--nocapture"])
        .env("RUST_BACKTRACE", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let read = |stream: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            stream.take(65_537).read_to_end(&mut bytes).unwrap();
            bytes
        })
    };
    let stdout = read(Box::new(child.stdout.take().unwrap()));
    let stderr = read(Box::new(child.stderr.take().unwrap()));
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{timeout_message}");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = stdout.join().unwrap();
    let stderr = stderr.join().unwrap();
    assert!(status.success());
    assert!(stdout.len() <= 65_536 && stderr.len() <= 65_536);
    let stdout = String::from_utf8(stdout).unwrap();
    let stderr = String::from_utf8(stderr).unwrap();
    assert!(!stdout.contains(private));
    assert!(!stderr.contains(private));
    assert_eq!(stderr.matches(public).count(), expected_public);
}

// Install only inside the existing isolated subprocess, before initialization.
pub fn initialize_privacy() {
    std::panic::set_hook(Box::new(|info| {
        if let Some(payload) = info.payload().downcast_ref::<&str>() {
            eprintln!("{payload}");
        } else if let Some(payload) = info.payload().downcast_ref::<String>() {
            eprintln!("{payload}");
        }
    }));
    autorouter_runtime::callback_privacy::initialize_observer_privacy();
}
