//! Child processes, ports and a minimal HTTP call to the token server.

use std::fs::File;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::num::NonZeroUsize;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};

/// A child process that is killed when dropped, with stdout and stderr sent to a log file.
pub struct ManagedProcess {
    name: String,
    child: Option<Child>,
}

impl ManagedProcess {
    pub fn spawn(name: &str, command: &mut Command, log_path: &Path) -> anyhow::Result<Self> {
        let log = File::create(log_path)
            .with_context(|| format!("creating the log file {}", log_path.display()))?;
        let log_for_stderr = log.try_clone().context("duplicating the log handle")?;
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_for_stderr))
            .spawn()
            .with_context(|| format!("starting {name}"))?;
        Ok(Self {
            name: name.to_owned(),
            child: Some(child),
        })
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.as_mut().map(Child::try_wait), Some(Ok(None)))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn port_open(addr: SocketAddr) -> bool {
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

pub fn free_port() -> anyhow::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("binding a free port")?;
    Ok(listener
        .local_addr()
        .context("reading the bound port")?
        .port())
}

pub fn wait_until(
    timeout: Duration,
    interval: Duration,
    mut condition: impl FnMut() -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(interval);
    }
}

pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

pub fn parse_response(raw: &str) -> anyhow::Result<HttpResponse> {
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .context("response without a header terminator")?;
    let status_line = head.lines().next().context("empty response")?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .with_context(|| format!("bad status line {status_line:?}"))?;
    Ok(HttpResponse {
        status,
        body: body.to_owned(),
    })
}

/// One HTTP/1.1 request over a fresh connection; `body` is sent as JSON when present.
pub fn http_request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> anyhow::Result<HttpResponse> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))
        .with_context(|| format!("connecting to {addr}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let payload = body.unwrap_or_default();
    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
    if body.is_some() {
        request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            payload.len()
        ));
    }
    request.push_str("\r\n");
    request.push_str(payload);
    stream
        .write_all(request.as_bytes())
        .context("sending the request")?;
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .context("reading the response")?;
    parse_response(&String::from_utf8_lossy(&raw))
}

pub fn require_status(response: &HttpResponse, expected: u16, what: &str) -> anyhow::Result<()> {
    if response.status != expected {
        bail!(
            "{what}: expected HTTP {expected}, got {} ({})",
            response.status,
            response.body.chars().take(120).collect::<String>()
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct CpuSample {
    pub cpu: Duration,
    pub wall: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuUsage {
    pub cpu_secs: f64,
    pub wall_secs: f64,
    /// CPU seconds per wall second (1.0 = one logical core saturated).
    pub cores_used: f64,
    /// CPU time divided by wall time and by logical core count, as a percentage (0..100).
    pub per_core_pct: f64,
    pub logical_cores: usize,
}

pub fn logical_cores() -> usize {
    std::thread::available_parallelism()
        .map(NonZeroUsize::get)
        .unwrap_or(1)
}

pub fn parse_process_cpu_ticks(stdout: &str) -> anyhow::Result<u64> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        bail!("empty CPU time output");
    }
    trimmed
        .parse::<u64>()
        .with_context(|| format!("invalid CPU ticks output {trimmed:?}"))
}

pub fn compute_cpu_usage(
    start: CpuSample,
    end: CpuSample,
    logical_cores: usize,
) -> anyhow::Result<CpuUsage> {
    let wall_secs = end.wall.saturating_duration_since(start.wall).as_secs_f64();
    if wall_secs <= 0.0 {
        bail!("wall duration must be positive to compute CPU usage");
    }
    let cpu_secs = end.cpu.saturating_sub(start.cpu).as_secs_f64();
    let cores = logical_cores.max(1);
    let cores_used = cpu_secs / wall_secs;
    let per_core_pct = (cores_used / cores as f64) * 100.0;
    Ok(CpuUsage {
        cpu_secs,
        wall_secs,
        cores_used,
        per_core_pct,
        logical_cores: cores,
    })
}

#[cfg(windows)]
pub fn sample_process_cpu(pid: u32) -> anyhow::Result<CpuSample> {
    let before = Instant::now();
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("(Get-Process -Id {pid}).TotalProcessorTime.Ticks"),
        ])
        .output()
        .with_context(|| format!("running powershell to sample CPU of pid {pid}"))?;
    let after = Instant::now();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "powershell CPU query failed for pid {pid} ({}): {}",
            output.status,
            stderr.trim()
        );
    }
    let ticks = parse_process_cpu_ticks(&String::from_utf8_lossy(&output.stdout))?;
    let wall = before + after.saturating_duration_since(before) / 2;
    Ok(CpuSample {
        cpu: Duration::from_nanos(ticks.saturating_mul(100)),
        wall,
    })
}

#[cfg(not(windows))]
pub fn sample_process_cpu(_pid: u32) -> anyhow::Result<CpuSample> {
    bail!("process CPU sampling is only supported on Windows")
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};

    use super::*;

    #[test]
    fn response_parsing_extracts_status_and_body() {
        let parsed =
            parse_response("HTTP/1.1 401 Unauthorized\r\nA: b\r\n\r\n{\"error\":\"x\"}").unwrap();
        assert_eq!(parsed.status, 401);
        assert_eq!(parsed.body, "{\"error\":\"x\"}");
        assert!(parse_response("garbage").is_err());
        assert!(parse_response("HTTP/1.1 abc\r\n\r\n").is_err());
    }

    #[test]
    fn free_port_can_be_bound_and_open_ports_are_detected() {
        let port = free_port().unwrap();
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        assert!(!port_open(addr));
        let listener = TcpListener::bind(addr).unwrap();
        assert!(port_open(addr));
        drop(listener);
    }

    #[test]
    fn http_request_round_trips_with_a_local_server() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut content_length = 0usize;
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length: ") {
                    content_length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body).unwrap();
            let reply = format!("{{\"echo\":{}}}", String::from_utf8_lossy(&body));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.len(),
                reply
            )
            .unwrap();
            first
        });
        let response = http_request(addr, "POST", "/token", Some("{\"a\":1}")).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "{\"echo\":{\"a\":1}}");
        assert!(server.join().unwrap().starts_with("POST /token HTTP/1.1"));
        assert!(require_status(&response, 200, "x").is_ok());
        assert!(require_status(&response, 401, "x").is_err());
    }

    #[test]
    fn wait_until_returns_when_the_condition_holds_or_times_out() {
        let mut calls = 0;
        assert!(wait_until(
            Duration::from_secs(1),
            Duration::from_millis(1),
            || {
                calls += 1;
                calls >= 3
            }
        ));
        assert!(!wait_until(
            Duration::from_millis(20),
            Duration::from_millis(5),
            || false
        ));
    }

    #[test]
    fn process_cpu_ticks_parsing_accepts_integers_and_rejects_garbage() {
        assert_eq!(parse_process_cpu_ticks("12345678\r\n").unwrap(), 12_345_678);
        assert_eq!(parse_process_cpu_ticks("0").unwrap(), 0);
        assert!(parse_process_cpu_ticks("").is_err());
        assert!(parse_process_cpu_ticks("  \n").is_err());
        assert!(parse_process_cpu_ticks("-5").is_err());
        assert!(parse_process_cpu_ticks("abc").is_err());
    }

    #[test]
    fn cpu_usage_computes_cores_used_and_per_core_percentage() {
        let t0 = Instant::now();
        let start = CpuSample {
            cpu: Duration::from_secs(10),
            wall: t0,
        };
        let end = CpuSample {
            cpu: Duration::from_secs(25),
            wall: t0 + Duration::from_secs(30),
        };
        let usage = compute_cpu_usage(start, end, 10).unwrap();
        assert!((usage.cpu_secs - 15.0).abs() < 1e-9);
        assert!((usage.wall_secs - 30.0).abs() < 1e-9);
        assert!((usage.cores_used - 0.5).abs() < 1e-9);
        assert!((usage.per_core_pct - 5.0).abs() < 1e-9);
        assert_eq!(usage.logical_cores, 10);

        // Saturating CPU delta and zero wall duration guard.
        assert!(compute_cpu_usage(end, start, 10).is_err());
        assert!(compute_cpu_usage(start, start, 10).is_err());
        let zero_cpu = compute_cpu_usage(
            CpuSample {
                cpu: Duration::from_secs(20),
                wall: t0,
            },
            CpuSample {
                cpu: Duration::from_secs(10),
                wall: t0 + Duration::from_secs(10),
            },
            0,
        )
        .unwrap();
        assert_eq!(zero_cpu.cpu_secs, 0.0);
        assert_eq!(zero_cpu.logical_cores, 1);
    }
}
