use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn binary_starts_and_serves_healthz() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve local test port");
    let port = listener
        .local_addr()
        .expect("read local test address")
        .port();
    drop(listener);

    let temp = tempfile::tempdir().expect("create temporary release smoke directory");
    let config_path = temp.path().join("bridge.toml");
    let data_dir = temp.path().join("data");
    fs::write(
        &config_path,
        format!(
            r#"[server]
bind_addr = "127.0.0.1"
port = {port}

[storage]
data_dir = "{}"
media_ttl_days = 30

[transport]
tls_profile = "chrome"
timeout_secs = 30
"#,
            data_dir.display()
        ),
    )
    .expect("write release smoke config");

    let startup_started = Instant::now();
    let binary = std::env::var_os("GEMINI_BRIDGE_SMOKE_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_gemini-bridge")));
    let child = Command::new(binary)
        .args(["--config", config_path.to_str().expect("UTF-8 config path")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start gemini-bridge binary");
    let mut child = ChildGuard(child);

    let client = reqwest::Client::new();
    let health_url = format!("http://127.0.0.1:{port}/healthz");
    let deadline = Instant::now() + Duration::from_secs(5);

    loop {
        if let Some(status) = child.0.try_wait().expect("inspect bridge process") {
            panic!("gemini-bridge exited before becoming healthy: {status}");
        }

        if let Ok(response) = client.get(&health_url).send().await
            && response.status().is_success()
        {
            let body: serde_json::Value = response.json().await.expect("decode health response");
            assert_eq!(body["status"], "ok");
            assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
            assert!(body["uptime_secs"].is_u64());
            println!(
                "binary startup-to-health: {} ms",
                startup_started.elapsed().as_millis()
            );
            break;
        }

        assert!(
            Instant::now() < deadline,
            "gemini-bridge did not serve /healthz within five seconds"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let readiness = client
        .get(format!("http://127.0.0.1:{port}/readyz"))
        .send()
        .await
        .expect("request readiness");
    assert_eq!(readiness.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let readiness_body: serde_json::Value = readiness.json().await.expect("decode readiness");
    assert_eq!(readiness_body["session_status"], "unconfigured");

    let admin_status = client
        .get(format!("http://127.0.0.1:{port}/admin/status"))
        .send()
        .await
        .expect("request protected admin status");
    assert_eq!(admin_status.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert!(data_dir.join("bridge.sqlite").is_file());
}
