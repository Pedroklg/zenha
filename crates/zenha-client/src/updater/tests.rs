//! Testes da atualização automática.

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::download::{download_to_paths_blocking, is_allowed_download_redirect};
use super::download::*;
use super::install::*;
use super::release::*;
use super::version::*;
use super::*;

static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(1);

fn isolated_test_dir(label: &str) -> PathBuf {
    let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "zenha-update-test-{label}-{}-{id}",
        std::process::id()
    ))
}

fn sha256_of(data: &[u8]) -> ([u8; 32], String) {
    let digest: [u8; 32] = Sha256::digest(data).into();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    (digest, format!("sha256:{hex}"))
}

fn sample_asset(name: &str, size: u64, digest: Option<&str>, url: &str) -> ReleaseAsset {
    ReleaseAsset {
        name: name.to_owned(),
        size,
        digest: digest.map(str::to_owned),
        browser_download_url: url.to_owned(),
    }
}

fn spawn_http_server(
    chunks: Vec<Vec<u8>>,
    declared_len: Option<u64>,
    delay_per_chunk: Duration,
) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut req_buf = [0u8; 1024];
        let _ = stream.read(&mut req_buf);
        let total: usize = chunks.iter().map(Vec::len).sum();
        let content_len = declared_len.unwrap_or(total as u64);
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {content_len}\r\nConnection: close\r\n\r\n"
        );
        if stream.write_all(header.as_bytes()).is_err() {
            return;
        }
        for chunk in chunks {
            if !delay_per_chunk.is_zero() {
                std::thread::sleep(delay_per_chunk);
            }
            if stream.write_all(&chunk).is_err() {
                return;
            }
            let _ = stream.flush();
        }
    });
    (
        format!("http://{addr}/Zenha-0.2.0-windows-x64-setup.exe"),
        handle,
    )
}

#[test]
fn parses_plain_and_v_prefixed_versions() {
    assert_eq!(
        parse_semver("0.1.0"),
        Some(SemVer {
            major: 0,
            minor: 1,
            patch: 0
        })
    );
    assert_eq!(
        parse_semver("v1.2.3"),
        Some(SemVer {
            major: 1,
            minor: 2,
            patch: 3
        })
    );
    assert_eq!(
        parse_semver(" V10.20.30+build1 "),
        Some(SemVer {
            major: 10,
            minor: 20,
            patch: 30
        })
    );
}

#[test]
fn ignores_prereleases_and_malformed_versions() {
    assert_eq!(parse_semver("v0.2.0-rc.1"), None);
    assert_eq!(parse_semver("0.2.0-alpha"), None);
    assert_eq!(parse_semver("1.0.0-beta+12"), None);
    assert_eq!(parse_semver(""), None);
    assert_eq!(parse_semver("v1.2"), None);
    assert_eq!(parse_semver("1.2.3.4"), None);
    assert_eq!(parse_semver("+1.0.0"), None);
    assert_eq!(parse_semver("1.two.3"), None);
}

#[test]
fn semver_ordering_compares_numerically() {
    let v0_2_0 = parse_semver("0.2.0");
    let v0_10_0 = parse_semver("0.10.0");
    assert!(v0_10_0 > v0_2_0);
}

#[test]
fn evaluate_release_accepts_only_newer_stable_releases() {
    let update = evaluate_release(
        "0.1.0",
        DEFAULT_RELEASES_REPO,
        "v0.2.0",
        "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0",
        false,
        false,
        &[],
    );
    assert_eq!(
        update,
        Some(AvailableUpdate {
            version: "0.2.0".to_owned(),
            url: "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0".to_owned(),
            installer: None,
        })
    );

    assert_eq!(
        evaluate_release(
            "0.2.0",
            DEFAULT_RELEASES_REPO,
            "v0.2.0",
            "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0",
            false,
            false,
            &[],
        ),
        None
    );
    assert_eq!(
        evaluate_release(
            "0.3.0",
            DEFAULT_RELEASES_REPO,
            "v0.2.9",
            "https://github.com/Pedroklg/zenha/releases/tag/v0.2.9",
            false,
            false,
            &[],
        ),
        None
    );
}

#[test]
fn evaluate_release_ignores_prereleases_drafts_and_unsafe_urls() {
    assert_eq!(
        evaluate_release(
            "0.1.0",
            DEFAULT_RELEASES_REPO,
            "v0.2.0",
            "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0",
            true,
            false,
            &[],
        ),
        None
    );
    assert_eq!(
        evaluate_release(
            "0.1.0",
            DEFAULT_RELEASES_REPO,
            "v0.2.0",
            "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0",
            false,
            true,
            &[],
        ),
        None
    );
    assert_eq!(
        evaluate_release(
            "0.1.0",
            DEFAULT_RELEASES_REPO,
            "v0.2.0-rc.1",
            "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0-rc.1",
            false,
            false,
            &[],
        ),
        None
    );
    assert_eq!(
        evaluate_release(
            "0.1.0",
            DEFAULT_RELEASES_REPO,
            "v0.2.0",
            "file:///etc/passwd",
            false,
            false,
            &[],
        ),
        None
    );
}

#[test]
fn should_show_update_respects_dismissed_version_until_newer_arrives() {
    let v0_2_0 = AvailableUpdate {
        version: "0.2.0".to_owned(),
        url: "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0".to_owned(),
        installer: None,
    };
    let v0_3_0 = AvailableUpdate {
        version: "0.3.0".to_owned(),
        url: "https://github.com/Pedroklg/zenha/releases/tag/v0.3.0".to_owned(),
        installer: None,
    };

    assert!(should_show_update(&v0_2_0, None));
    assert!(!should_show_update(&v0_2_0, Some("0.2.0")));
    assert!(!should_show_update(&v0_2_0, Some("v0.2.0")));
    assert!(!should_show_update(&v0_2_0, Some("0.2.5")));
    assert!(should_show_update(&v0_3_0, Some("0.2.0")));
    assert!(should_show_update(&v0_2_0, Some("corrupted")));
}

#[test]
fn resolves_releases_repo_with_default_fallback() {
    assert_eq!(resolve_releases_repo(None), "Pedroklg/zenha");
    assert_eq!(resolve_releases_repo(Some("")), "Pedroklg/zenha");
    assert_eq!(resolve_releases_repo(Some("  ")), "Pedroklg/zenha");
    assert_eq!(resolve_releases_repo(Some(" org/custom ")), "org/custom");
    assert_eq!(
        latest_release_url("org/custom"),
        "https://api.github.com/repos/org/custom/releases/latest"
    );
}

#[test]
fn parse_sha256_digest_validates_prefix_algorithm_and_hex() {
    let (bytes, valid) = sha256_of(b"zenha installer");
    assert_eq!(parse_sha256_digest(&valid), Some(bytes));
    assert_eq!(
        parse_sha256_digest(&valid.to_uppercase().replace("SHA256:", "sha256:")),
        Some(bytes)
    );
    assert_eq!(parse_sha256_digest(&format!("  {valid}  ")), Some(bytes));

    assert_eq!(parse_sha256_digest(""), None);
    assert_eq!(parse_sha256_digest("SHA256:0000"), None);
    assert_eq!(
        parse_sha256_digest(&valid.replace("sha256:", "sha512:")),
        None
    );
    assert_eq!(parse_sha256_digest(&valid[7..]), None);
    assert_eq!(parse_sha256_digest(&format!("{valid}00")), None);
    assert_eq!(parse_sha256_digest(&valid[..valid.len() - 2]), None);
    let non_hex = format!("sha256:{}zz", &valid[7..valid.len() - 2]);
    assert_eq!(parse_sha256_digest(&non_hex), None);
}

#[test]
fn select_installer_asset_validates_name_url_size_and_digest() {
    let (hash, digest) = sha256_of(b"setup-binary");
    let valid_url = "https://github.com/Pedroklg/zenha/releases/download/v0.2.0/Zenha-0.2.0-windows-x64-setup.exe";
    let zip_asset = sample_asset(
        "Zenha-0.2.0-windows-x64.zip",
        100,
        Some(&digest),
        "https://github.com/Pedroklg/zenha/releases/download/v0.2.0/Zenha-0.2.0-windows-x64.zip",
    );
    let valid_setup = sample_asset(
        "Zenha-0.2.0-windows-x64-setup.exe",
        12_345,
        Some(&digest),
        valid_url,
    );

    let update = evaluate_release(
        "0.1.5",
        DEFAULT_RELEASES_REPO,
        "v0.2.0",
        "https://github.com/Pedroklg/zenha/releases/tag/v0.2.0",
        false,
        false,
        &[zip_asset.clone(), valid_setup.clone()],
    )
    .unwrap();
    assert_eq!(
        update.installer,
        Some(InstallerAsset {
            url: valid_url.to_owned(),
            size: 12_345,
            sha256: hash,
        })
    );

    // Missing installer asset falls back to link-only update.
    assert_eq!(
        select_installer_asset("0.2.0", DEFAULT_RELEASES_REPO, &[zip_asset]),
        None
    );

    // Wrong version in installer name is rejected.
    let wrong_ver = sample_asset(
        "Zenha-0.2.1-windows-x64-setup.exe",
        100,
        Some(&digest),
        valid_url,
    );
    assert_eq!(
        select_installer_asset("0.2.0", DEFAULT_RELEASES_REPO, &[wrong_ver]),
        None
    );

    // Missing or invalid digest falls back to link-only.
    let no_digest = sample_asset("Zenha-0.2.0-windows-x64-setup.exe", 100, None, valid_url);
    assert_eq!(
        select_installer_asset("0.2.0", DEFAULT_RELEASES_REPO, &[no_digest]),
        None
    );
    let bad_algo = sample_asset(
        "Zenha-0.2.0-windows-x64-setup.exe",
        100,
        Some("sha512:abcd"),
        valid_url,
    );
    assert_eq!(
        select_installer_asset("0.2.0", DEFAULT_RELEASES_REPO, &[bad_algo]),
        None
    );

    // Size zero or above 200 MB is rejected.
    let zero_size = sample_asset(
        "Zenha-0.2.0-windows-x64-setup.exe",
        0,
        Some(&digest),
        valid_url,
    );
    assert_eq!(
        select_installer_asset("0.2.0", DEFAULT_RELEASES_REPO, &[zero_size]),
        None
    );
    let oversized = sample_asset(
        "Zenha-0.2.0-windows-x64-setup.exe",
        MAX_INSTALLER_BYTES + 1,
        Some(&digest),
        valid_url,
    );
    assert_eq!(
        select_installer_asset("0.2.0", DEFAULT_RELEASES_REPO, &[oversized]),
        None
    );
    let max_exact = sample_asset(
        "Zenha-0.2.0-windows-x64-setup.exe",
        MAX_INSTALLER_BYTES,
        Some(&digest),
        valid_url,
    );
    assert!(select_installer_asset("0.2.0", DEFAULT_RELEASES_REPO, &[max_exact]).is_some());

    // URL outside repository is rejected.
    let foreign_repo = sample_asset(
        "Zenha-0.2.0-windows-x64-setup.exe",
        100,
        Some(&digest),
        "https://github.com/other/zenha/releases/download/v0.2.0/Zenha-0.2.0-windows-x64-setup.exe",
    );
    assert_eq!(
        select_installer_asset("0.2.0", DEFAULT_RELEASES_REPO, &[foreign_repo]),
        None
    );
}

#[test]
fn repo_download_url_and_redirect_validation_block_untrusted_targets() {
    assert!(is_safe_repo_download_url(
        "https://github.com/Pedroklg/zenha/releases/download/v0.2.0/Zenha-0.2.0-windows-x64-setup.exe",
        "Pedroklg/zenha",
    ));
    assert!(is_safe_repo_download_url(
        "https://github.com/pedroklg/zenha/releases/download/v0.2.0/Zenha-0.2.0-windows-x64-setup.exe",
        "Pedroklg/zenha",
    ));
    assert!(!is_safe_repo_download_url(
        "http://github.com/Pedroklg/zenha/releases/download/v0.2.0/Zenha-0.2.0-windows-x64-setup.exe",
        "Pedroklg/zenha",
    ));
    assert!(!is_safe_repo_download_url(
        "https://github.com/Pedroklg/zenha-evil/releases/download/v0.2.0/setup.exe",
        "Pedroklg/zenha",
    ));
    assert!(!is_safe_repo_download_url(
        "https://github.com.evil.com/Pedroklg/zenha/releases/download/v0.2.0/setup.exe",
        "Pedroklg/zenha",
    ));
    assert!(!is_safe_repo_download_url(
        "https://user@github.com/Pedroklg/zenha/releases/download/v0.2.0/setup.exe",
        "Pedroklg/zenha",
    ));
    assert!(!is_safe_repo_download_url(
        "https://github.com/Pedroklg/zenha/../../other/repo/setup.exe",
        "Pedroklg/zenha",
    ));
    assert!(!is_safe_repo_download_url(
        "https://github.com/Pedroklg/zenha/",
        "Pedroklg/zenha",
    ));

    assert!(is_allowed_download_redirect(
        &reqwest::Url::parse("https://objects.githubusercontent.com/abc/setup.exe").unwrap()
    ));
    assert!(is_allowed_download_redirect(
        &reqwest::Url::parse("https://release-assets.githubusercontent.com/setup.exe").unwrap()
    ));
    assert!(!is_allowed_download_redirect(
        &reqwest::Url::parse("http://objects.githubusercontent.com/abc/setup.exe").unwrap()
    ));
    assert!(!is_allowed_download_redirect(
        &reqwest::Url::parse("https://evilgithubusercontent.com/setup.exe").unwrap()
    ));
    assert!(!is_allowed_download_redirect(
        &reqwest::Url::parse("https://example.com/setup.exe").unwrap()
    ));
}

#[test]
fn installed_exe_detection_compares_normalized_paths_case_insensitively() {
    let local = Path::new(r"C:\Users\Maria\AppData\Local");
    assert!(is_installed_exe_path(
        Path::new(r"C:\Users\Maria\AppData\Local\Programs\Zenha\zenha.exe"),
        local,
    ));
    assert!(is_installed_exe_path(
        Path::new(r"\\?\c:\users\MARIA\appdata\local\programs\zenha\ZENHA.EXE"),
        Path::new(r"C:/Users/Maria/AppData/Local/"),
    ));
    assert!(is_installed_exe_path(
        Path::new(r"C:\Users\Maria\AppData\Local\Programs\Zenha\.\sub\..\zenha.exe"),
        local,
    ));

    assert!(!is_installed_exe_path(
        Path::new(r"C:\Users\Maria\Downloads\Zenha\zenha.exe"),
        local,
    ));
    assert!(!is_installed_exe_path(
        Path::new(r"F:\cargo-target\zenha\debug\zenha.exe"),
        local,
    ));
    assert!(!is_installed_exe_path(
        Path::new(r"C:\Users\Maria\AppData\Local\Programs\Zenha\sub\zenha.exe"),
        local,
    ));
    assert!(!is_installed_exe_path(
        Path::new(r"C:\Users\Maria\AppData\Local\Programs\Zenha\zenha-portable.exe"),
        local,
    ));
    assert!(!is_installed_exe_path(Path::new(r"zenha.exe"), local));
}

#[test]
fn part_verifier_verifies_chunks_size_and_sha256() {
    let payload = b"installer-payload-chunk-data";
    let (hash, _) = sha256_of(payload);

    let mut sink = Vec::new();
    let mut verifier = PartVerifier::new(&mut sink, payload.len() as u64, hash).unwrap();
    assert_eq!(verifier.push_chunk(&payload[..10]), Ok(10));
    assert_eq!(
        verifier.push_chunk(&payload[10..]),
        Ok(payload.len() as u64)
    );
    assert_eq!(verifier.finish(), Ok(()));
    assert_eq!(sink, payload);

    let mut wrong_hash = hash;
    wrong_hash[0] ^= 0xff;
    let mut sink = Vec::new();
    let mut verifier = PartVerifier::new(&mut sink, payload.len() as u64, wrong_hash).unwrap();
    verifier.push_chunk(payload).unwrap();
    assert_eq!(verifier.finish(), Err(DownloadError::HashMismatch));

    let mut sink = Vec::new();
    let mut verifier = PartVerifier::new(&mut sink, payload.len() as u64 + 1, hash).unwrap();
    verifier.push_chunk(payload).unwrap();
    assert_eq!(verifier.finish(), Err(DownloadError::SizeMismatch));

    let mut sink = Vec::new();
    let mut verifier = PartVerifier::new(&mut sink, 5, hash).unwrap();
    assert_eq!(
        verifier.push_chunk(b"123456"),
        Err(DownloadError::SizeMismatch)
    );

    let mut sink = Vec::new();
    assert!(matches!(
        PartVerifier::new(&mut sink, MAX_INSTALLER_BYTES + 1, hash),
        Err(DownloadError::TooLarge)
    ));
}

#[test]
fn download_to_paths_streams_verifies_and_cleans_part_files() {
    let dir = isolated_test_dir("download");
    let (part_path, final_path) = installer_paths(&dir, "0.2.0").unwrap();
    let payload = b"MZ-fake-zenha-installer-payload-0123456789";
    let (hash, _) = sha256_of(payload);

    // 1. Valid multi-chunk download renames .part -> .exe and reports progress.
    let (url, server) = spawn_http_server(
        vec![payload[..15].to_vec(), payload[15..].to_vec()],
        None,
        Duration::ZERO,
    );
    let cancel = AtomicBool::new(false);
    let mut progress_steps = Vec::new();
    let out = download_to_paths_blocking(
        &url,
        payload.len() as u64,
        hash,
        &part_path,
        &final_path,
        &cancel,
        false,
        |downloaded, total| progress_steps.push((downloaded, total)),
    );
    let _ = server.join();
    assert_eq!(out, Ok(final_path.clone()));
    assert!(!part_path.exists());
    assert_eq!(fs::read(&final_path).unwrap(), payload);
    assert_eq!(
        progress_steps.last().copied(),
        Some((payload.len() as u64, payload.len() as u64))
    );

    // 2. Hash mismatch deletes .part and removes any prior final exe.
    let mut bad_hash = hash;
    bad_hash[0] ^= 1;
    let (url, server) = spawn_http_server(vec![payload.to_vec()], None, Duration::ZERO);
    let out = download_to_paths_blocking(
        &url,
        payload.len() as u64,
        bad_hash,
        &part_path,
        &final_path,
        &cancel,
        false,
        |_, _| {},
    );
    let _ = server.join();
    assert_eq!(out, Err(DownloadError::HashMismatch));
    assert!(!part_path.exists());
    assert!(!final_path.exists());

    // 3. Content-Length mismatch deletes .part.
    let (url, server) = spawn_http_server(
        vec![payload.to_vec()],
        Some(payload.len() as u64 + 5),
        Duration::ZERO,
    );
    let out = download_to_paths_blocking(
        &url,
        payload.len() as u64,
        hash,
        &part_path,
        &final_path,
        &cancel,
        false,
        |_, _| {},
    );
    let _ = server.join();
    assert_eq!(out, Err(DownloadError::SizeMismatch));
    assert!(!part_path.exists());
    assert!(!final_path.exists());

    // 4. Cancellation mid-stream deletes .part.
    let cancel_mid = Arc::new(AtomicBool::new(false));
    let cancel_flag = Arc::clone(&cancel_mid);
    let (url, server) = spawn_http_server(
        vec![payload[..10].to_vec(), payload[10..].to_vec()],
        None,
        Duration::from_millis(30),
    );
    let out = download_to_paths_blocking(
        &url,
        payload.len() as u64,
        hash,
        &part_path,
        &final_path,
        &cancel_mid,
        false,
        |_, _| cancel_flag.store(true, Ordering::Relaxed),
    );
    let _ = server.join();
    assert_eq!(out, Err(DownloadError::Cancelled));
    assert!(!part_path.exists());
    assert!(!final_path.exists());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn cleanup_update_dir_removes_leftovers_and_handles_missing_dir() {
    let dir = isolated_test_dir("cleanup");
    cleanup_update_dir(&dir);

    fs::create_dir_all(&dir).unwrap();
    let (part, exe) = installer_paths(&dir, "0.2.0").unwrap();
    fs::write(&part, b"partial").unwrap();
    fs::write(&exe, b"old-setup").unwrap();

    cleanup_update_dir(&dir);
    assert!(!part.exists());
    assert!(!exe.exists());
    assert!(!dir.exists());
}

#[test]
fn next_demo_version_bumps_minor_component() {
    assert_eq!(next_demo_version("0.1.5"), "0.2.0");
    assert_eq!(next_demo_version("1.4.2"), "1.5.0");
    assert_eq!(next_demo_version("invalid"), "0.2.0");
}

#[test]
fn launch_installer_rejects_missing_and_directory_paths() {
    let missing = isolated_test_dir("missing-installer").join("setup.exe");
    assert!(launch_installer(&missing).is_err());
    assert!(launch_installer(&std::env::temp_dir()).is_err());
    assert_eq!(
        INSTALLER_ARGS,
        &[
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/CLOSEAPPLICATIONS"
        ]
    );
}
