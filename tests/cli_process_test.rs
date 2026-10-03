#![cfg(unix)]

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const COOKIE: &str = "__Secure-1PSID=process-psid-secret; __Secure-1PSIDTS=process-ts-secret; SAPISID=process-sapisid-secret";
const SECRET_VALUES: [&str; 3] = [
    "process-psid-secret",
    "process-ts-secret",
    "process-sapisid-secret",
];

struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_for_child(child: &mut std::process::Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().expect("poll CLI process") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "CLI process exceeded test deadline"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_gemini-bridge"))
}

fn write_config(root: &Path, proxy_url: Option<&str>) -> PathBuf {
    let config = root.join("bridge.toml");
    let proxy = proxy_url
        .map(|url| format!("proxy_url = \"{url}\"\n"))
        .unwrap_or_default();
    fs::write(
        &config,
        format!(
            r#"[storage]
data_dir = "{}"
media_ttl_days = 30

[transport]
tls_profile = "chrome"
{proxy}timeout_secs = 2
"#,
            root.join("data").display()
        ),
    )
    .expect("write CLI test config");
    config
}

fn assert_no_credentials(output: &Output) {
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for secret in SECRET_VALUES {
        assert!(
            !combined.contains(secret),
            "process output exposed a credential value: {combined}"
        );
    }
}

fn run(config: &Path, args: &[&str]) -> Output {
    Command::new(binary())
        .arg("--config")
        .arg(config)
        .args(args)
        .stdin(Stdio::null())
        .env_remove("BRIDGE_SECRET")
        .env_remove("RUST_LOG")
        .output()
        .expect("run gemini-bridge")
}

fn run_login_in_pty(config: &Path, cookie: &str) -> Output {
    let command = format!(
        "{} --config {} auth login",
        binary().display(),
        config.display()
    );
    let child = Command::new("script")
        .args(["--quiet", "--return", "--command"])
        .arg(command)
        .arg("/dev/null")
        .env_remove("BRIDGE_SECRET")
        .env_remove("RUST_LOG")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start login in pseudo-terminal");
    let mut child_guard = ChildGuard(child);
    let child = &mut child_guard.0;

    let mut stdout = child.stdout.take().expect("capture pseudo-terminal output");
    let (prompt_tx, prompt_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut byte = [0_u8; 1];
        let prompt = b"(input is hidden): ";
        loop {
            match stdout.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    output.push(byte[0]);
                    if output.ends_with(prompt) {
                        let _ = prompt_tx.send(());
                    }
                }
                Err(error) => panic!("read pseudo-terminal output: {error}"),
            }
        }
        output
    });

    prompt_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("login emitted its input prompt");
    let mut stdin = child.stdin.take().expect("open pseudo-terminal input");
    stdin.write_all(cookie.as_bytes()).expect("write cookie");
    stdin.write_all(b"\n").expect("submit cookie");
    drop(stdin);

    let status = wait_for_child(child);
    let stdout = reader.join().expect("join output reader");
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .expect("capture script stderr")
        .read_to_end(&mut stderr)
        .expect("read script stderr");
    Output {
        status,
        stdout,
        stderr,
    }
}

struct RejectingProxy {
    url: String,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for RejectingProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Start a local HTTP proxy that rejects upstream requests. This exercises the
/// real executable and transport without contacting Gemini.
fn rejecting_proxy() -> RejectingProxy {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind rejecting proxy");
    listener
        .set_nonblocking(true)
        .expect("configure rejecting proxy");
    let address = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let worker = std::thread::spawn(move || {
        while !worker_stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut request = [0_u8; 1024];
                    let _ = stream.read(&mut request);
                    let _ = stream.write_all(
                        b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });
    RejectingProxy {
        url: format!("http://{address}"),
        stop,
        worker: Some(worker),
    }
}

#[test]
fn auth_login_rejects_non_terminal_input_without_echoing_it() {
    let temp = tempfile::tempdir().unwrap();
    let config = write_config(temp.path(), None);
    let mut child = Command::new(binary())
        .args(["--config", config.to_str().unwrap(), "auth", "login"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("BRIDGE_SECRET")
        .spawn()
        .expect("start login with piped input");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(COOKIE.as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("wait for login");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not a terminal"));
    assert_no_credentials(&output);
}

#[test]
fn auth_login_rejects_empty_terminal_input() {
    let temp = tempfile::tempdir().unwrap();
    let config = write_config(temp.path(), None);

    let output = run_login_in_pty(&config, "");

    let diagnostics = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success());
    assert!(diagnostics.contains("Cookie string is empty"));
    assert_no_credentials(&output);
}

#[test]
fn doctor_reports_missing_credentials_without_exposing_values() {
    let temp = tempfile::tempdir().unwrap();
    let config = write_config(temp.path(), None);

    let output = run(&config, &["doctor"]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Session status: Unconfigured"));
    assert!(stdout.contains("auth login"));
    assert_no_credentials(&output);
}

#[test]
fn doctor_fails_for_a_rejected_probe_without_exposing_values() {
    let temp = tempfile::tempdir().unwrap();
    let proxy = rejecting_proxy();
    let config = write_config(temp.path(), Some(&proxy.url));
    fs::create_dir_all(temp.path().join("data")).unwrap();
    fs::write(
        temp.path().join("data/cookies.json"),
        r#"{"psid":"process-psid-secret","psidts":"process-ts-secret","sapisid":"process-sapisid-secret","imported_at":"2024-01-01T00:00:00Z"}"#,
    )
    .unwrap();

    let output = run(&config, &["doctor"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Probe failed"));
    assert!(stderr.contains("Session status: NeedsReauth"));
    assert_no_credentials(&output);
}

#[test]
fn auth_login_does_not_persist_credentials_when_the_probe_fails() {
    let temp = tempfile::tempdir().unwrap();
    let proxy = rejecting_proxy();
    let config = write_config(temp.path(), Some(&proxy.url));

    let output = run_login_in_pty(&config, COOKIE);

    assert!(!output.status.success());
    let diagnostics = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(diagnostics.contains("Failed to import credentials"));
    assert!(!temp.path().join("data/cookies.json").exists());
    assert_no_credentials(&output);
}
