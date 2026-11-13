//! Orquestrador: sobe os servidores que faltam, roda os cenários e grava o relatório.

use std::env::consts::EXE_SUFFIX;
use std::fs;
use std::net::SocketAddr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};

use crate::cli::RunArgs;
use crate::detect::{TONE_AMPLITUDE, TONE_HZ};
use crate::probe::AudioProbe;
use crate::proc::{ManagedProcess, free_port, http_request, port_open, wait_until};
use crate::report::{Environment, Report, now_utc};
use crate::scenario::{Recorder, ScenarioResult, Status};
use crate::scenarios::{ALL, Ctx, ScenarioDef, Setup, all_room_names};

pub const SDK_VERSIONS: &str = "livekit 0.10.1, libwebrtc 0.4.0, wasapi 0.27.0";
const LIVEKIT_ADDR: ([u8; 4], u16) = ([127, 0, 0, 1], 7880);
const LIVEKIT_KEY: &str = "devkey";
const LIVEKIT_SECRET: &str = "secret";
const LIVEKIT_START_TIMEOUT: Duration = Duration::from_secs(30);
const TOKEN_SERVER_START_TIMEOUT: Duration = Duration::from_secs(20);

pub fn select(filters: &[String]) -> Vec<&'static ScenarioDef> {
    ALL.iter()
        .filter(|def| filters.is_empty() || filters.iter().any(|f| def.name.contains(f.as_str())))
        .collect()
}

#[cfg(windows)]
fn platform_probe() -> Box<dyn AudioProbe> {
    Box::new(crate::loopback::WasapiProbe::from_env())
}

#[cfg(not(windows))]
fn platform_probe() -> Box<dyn AudioProbe> {
    Box::new(crate::probe::UnavailableProbe)
}

struct Servers {
    _livekit: Option<ManagedProcess>,
    _token: ManagedProcess,
    token_addr: SocketAddr,
    invite: String,
}

fn livekit_version(exe: &Path) -> Option<String> {
    let output = Command::new(exe).arg("--version").output().ok()?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

fn start_livekit(
    repo_root: &Path,
    out_dir: &Path,
    work_dir: &Path,
    env: &mut Environment,
) -> anyhow::Result<Option<ManagedProcess>> {
    let exe = repo_root
        .join("tools")
        .join("livekit")
        .join(format!("livekit-server{EXE_SUFFIX}"));
    if exe.exists() {
        env.livekit_server = livekit_version(&exe);
    }
    let addr = SocketAddr::from(LIVEKIT_ADDR);
    if port_open(addr) {
        env.notes.push(
            "livekit-server já respondia na porta 7880 e foi reaproveitado (chave devkey/secret assumida)"
                .to_owned(),
        );
        return Ok(None);
    }
    if !exe.exists() {
        bail!(
            "{} not found and nothing answers on port 7880",
            exe.display()
        );
    }
    let mut command = Command::new(&exe);
    command.arg("--dev").current_dir(work_dir);
    let process = ManagedProcess::spawn(
        "livekit-server",
        &mut command,
        &out_dir.join("livekit-server.log"),
    )?;
    let mut process = process;
    let up = wait_until(LIVEKIT_START_TIMEOUT, Duration::from_millis(250), || {
        process.is_running() && port_open(addr)
    });
    if !up {
        bail!("livekit-server did not open port 7880 (see livekit-server.log)");
    }
    env.notes
        .push("livekit-server iniciado pelo harness com --dev e encerrado ao final".to_owned());
    Ok(Some(process))
}

fn start_token_server(
    out_dir: &Path,
    work_dir: &Path,
    env: &mut Environment,
) -> anyhow::Result<(ManagedProcess, SocketAddr, String)> {
    let exe = std::env::current_exe()
        .context("locating this executable")?
        .with_file_name(format!("rooms-token-server{EXE_SUFFIX}"));
    if !exe.exists() {
        bail!(
            "{} not found; run `cargo build -p rooms-token-server` first",
            exe.display()
        );
    }
    let port = free_port()?;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let invite = uuid::Uuid::new_v4().simple().to_string();
    let mut command = Command::new(&exe);
    command
        .current_dir(work_dir)
        .env("ROOMS_BIND_ADDR", addr.to_string())
        .env("ROOMS_INVITE_CODE", &invite)
        .env("ROOMS_TOKEN_TTL_SECS", "3600")
        .env("ROOMS_ROOM_LIST", all_room_names().join(","))
        .env("LIVEKIT_URL", format!("ws://127.0.0.1:{}", LIVEKIT_ADDR.1))
        .env("LIVEKIT_API_KEY", LIVEKIT_KEY)
        .env("LIVEKIT_API_SECRET", LIVEKIT_SECRET)
        .env("RUST_LOG", "info");
    let mut process = ManagedProcess::spawn(
        "rooms-token-server",
        &mut command,
        &out_dir.join("token-server.log"),
    )?;
    let up = wait_until(
        TOKEN_SERVER_START_TIMEOUT,
        Duration::from_millis(100),
        || {
            process.is_running()
                && http_request(addr, "GET", "/health", None).is_ok_and(|r| r.status == 200)
        },
    );
    if !up {
        bail!("the token server did not become healthy (see token-server.log)");
    }
    env.notes.push(format!(
        "rooms-token-server iniciado em {addr} com variáveis de ambiente explícitas, em diretório fora do repositório (nenhum .env é lido)"
    ));
    Ok((process, addr, invite))
}

fn start_servers(
    repo_root: &Path,
    out_dir: &Path,
    work_dir: &Path,
    with_livekit: bool,
    env: &mut Environment,
) -> anyhow::Result<Servers> {
    let livekit = if with_livekit {
        start_livekit(repo_root, out_dir, work_dir, env)?
    } else {
        None
    };
    let (token, token_addr, invite) = start_token_server(out_dir, work_dir, env)?;
    Ok(Servers {
        _livekit: livekit,
        _token: token,
        token_addr,
        invite,
    })
}

fn describe_outcome(result: &ScenarioResult) -> String {
    match result.status {
        Status::Pass => format!("PASS ({} ms)", result.duration_ms),
        Status::Fail => format!(
            "FAIL ({} ms): {}",
            result.duration_ms,
            result.failure.as_deref().unwrap_or("unknown")
        ),
        Status::Skipped => "SKIPPED".to_owned(),
    }
}

fn run_scenarios(
    selected: &[&ScenarioDef],
    setup: &Setup,
    probe: &dyn AudioProbe,
    env: &mut Environment,
) -> Vec<ScenarioResult> {
    let mut results = Vec::new();
    for (index, def) in selected.iter().enumerate() {
        println!("[{}/{}] {} ...", index + 1, selected.len(), def.name);
        let result = if def.needs_media && !cfg!(windows) {
            Recorder::new(def.name, def.title)
                .skipped("needs the LiveKit server and the Windows audio probe")
        } else {
            let mut ctx = Ctx::new(def, setup, probe, env);
            let outcome = catch_unwind(AssertUnwindSafe(|| (def.run)(&mut ctx)))
                .unwrap_or_else(|_| Err(anyhow!("the scenario panicked")));
            ctx.finish(outcome)
        };
        println!("    {}", describe_outcome(&result));
        results.push(result);
    }
    results
}

pub fn run(args: &RunArgs) -> anyhow::Result<i32> {
    if args.list {
        for def in ALL {
            println!("{:<26} {}", def.name, def.title);
        }
        return Ok(0);
    }
    let selected = select(&args.filters);
    if selected.is_empty() {
        bail!("no scenario matches the filter {:?}", args.filters);
    }
    let repo_root = match &args.repo_root {
        Some(path) => path.clone(),
        None => std::env::current_dir().context("reading the current directory")?,
    };
    let out_dir = repo_root.join("dev-logs").join("e2e");
    fs::create_dir_all(&out_dir).with_context(|| format!("creating {}", out_dir.display()))?;
    let work_dir: PathBuf = std::env::temp_dir().join("rooms-e2e-work");
    fs::create_dir_all(&work_dir).context("creating the work directory")?;

    let started = Instant::now();
    let mut env = Environment {
        started_at_utc: now_utc(),
        harness_version: env!("CARGO_PKG_VERSION").to_owned(),
        sdk_versions: SDK_VERSIONS.to_owned(),
        os: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        ..Environment::default()
    };
    env.notes.push(format!(
        "tom de teste de {TONE_HZ} Hz com amplitude {TONE_AMPLITUDE}, tocado de verdade no dispositivo de saída"
    ));

    let probe = platform_probe();
    match probe.render_devices() {
        Ok(devices) => {
            env.default_render_device = devices.iter().find(|d| d.is_default).cloned();
            env.render_devices = devices;
        }
        Err(err) => env
            .notes
            .push(format!("sonda de áudio indisponível: {err:#}")),
    }

    let pinned_note = match (probe.pinned(), &env.default_render_device) {
        (true, Some(device)) => format!(
            "saída de teste fixada em `{}` (ROOMS_E2E_OUTPUT): os ouvintes escolhem esse aparelho e o caminho do padrão do sistema não é exercitado",
            device.name
        ),
        _ => {
            "sem ROOMS_E2E_OUTPUT: o tom toca no dispositivo de saída padrão do Windows".to_owned()
        }
    };
    println!("{pinned_note}");
    env.notes.push(pinned_note);

    println!("starting servers ...");
    let servers = start_servers(&repo_root, &out_dir, &work_dir, cfg!(windows), &mut env)?;
    let setup = Setup {
        exe: std::env::current_exe().context("locating this executable")?,
        work_dir,
        out_dir: out_dir.clone(),
        token_addr: servers.token_addr,
        invite: servers.invite.clone(),
    };
    let scenarios = run_scenarios(&selected, &setup, probe.as_ref(), &mut env);

    let report = Report {
        environment: env,
        scenarios,
        total_duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    };
    fs::write(out_dir.join("report.md"), report.to_markdown()).context("writing report.md")?;
    fs::write(out_dir.join("report.json"), report.to_json()).context("writing report.json")?;
    println!(
        "{} passed, {} failed, {} skipped. Report: {}",
        report.count(Status::Pass),
        report.count(Status::Fail),
        report.count(Status::Skipped),
        out_dir.join("report.md").display()
    );
    drop(servers);
    Ok(if report.success(!cfg!(windows)) { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_filter_selects_everything_and_filters_match_by_substring() {
        assert_eq!(select(&[]).len(), ALL.len());
        let playout: Vec<_> = select(&["playout".to_owned()])
            .iter()
            .map(|d| d.name)
            .collect();
        assert_eq!(
            playout,
            [
                "playout-listener-first",
                "playout-tone-first",
                "playout-explicit-device"
            ]
        );
        assert_eq!(select(&["chat".to_owned(), "mute".to_owned()]).len(), 2);
        assert!(select(&["nope".to_owned()]).is_empty());
    }
}
