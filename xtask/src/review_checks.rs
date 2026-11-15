//! Checagens rápidas de arquitetura e regras do Zenha (`cargo review`), só com `std`.
//!
//! As de fronteira também rodam no `cargo ci`; as demais só no `cargo review`.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Instant;

const CORE_DIR: &str = "crates/rooms-core/";
const PROTOCOL_DIR: &str = "crates/rooms-protocol/";
const SERVER_DIR: &str = "crates/rooms-token-server/src/";
const CORE_API: &str = "crates/rooms-core/src/api.rs";
const SDK_CRATES: &[&str] = &["livekit", "libwebrtc"];
const EXACT_PINS: &[&str] = &["livekit", "livekit-api", "libwebrtc"];
const PROTOCOL_DEPS: &[&str] = &["serde", "thiserror"];
const PROTOCOL_FORBIDDEN: &[&str] = &["async fn", "tokio", "std::fs", "std::net"];
const CORE_PUBLIC_FILES: &[&str] = &[
    "crates/rooms-core/src/lib.rs",
    "crates/rooms-core/src/api.rs",
    "crates/rooms-core/src/backend.rs",
    "crates/rooms-core/src/handle.rs",
    "crates/rooms-core/src/video.rs",
    "crates/rooms-core/src/capture.rs",
];
const SECRET_FIELDS: &[&str] = &[
    "api_secret",
    "livekit_api_secret",
    "invite_code_hash",
    "admin_code_hash",
    "password_hash",
    "private_key",
];
const SQL_MARKERS: &[&str] = &[
    "SELECT",
    "INSERT",
    "UPDATE",
    "DELETE",
    "execute(",
    "prepare(",
    "query_row(",
];
const TRACING_MACROS: &[&str] = &["trace!(", "debug!(", "info!(", "warn!(", "error!("];
const CORE_12_BINDINGS: &[&str] = &[
    "crates/rooms-core/src/backend.rs",
    "crates/rooms-core/src/session.rs",
    "crates/rooms-core/src/testing.rs",
    "crates/rooms-core/src/no_media.rs",
    "crates/rooms-core/src/livekit_backend.rs",
    "crates/rooms-client/src/",
    "crates/rooms-e2e/src/protocol.rs",
];

struct Finding {
    path: String,
    line: usize,
    rule: &'static str,
    message: String,
}

impl Finding {
    fn new(path: &str, line: usize, rule: &'static str, message: impl Into<String>) -> Self {
        Self {
            path: path.to_owned(),
            line,
            rule,
            message: message.into(),
        }
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(f, "{}: {}: {}", self.path, self.rule, self.message)
        } else {
            write!(
                f,
                "{}:{}: {}: {}",
                self.path, self.line, self.rule, self.message
            )
        }
    }
}

struct Source {
    path: String,
    text: String,
}

struct Workspace {
    root: PathBuf,
    forbidden: Vec<String>,
    files: Vec<Source>,
}

/// Checagens de fronteira para o `cargo ci`; imprime os achados e diz se passou.
pub fn boundaries() -> bool {
    match Workspace::load() {
        Ok(ws) => report(&boundary_findings(&ws), &[]),
        Err(err) => {
            eprintln!("review-checks: {err}");
            false
        }
    }
}

/// `cargo review [base]`: todas as checagens; `base` (padrão `HEAD`) só serve à CORE-12.
pub fn run(base: &str) -> ExitCode {
    let start = Instant::now();
    let ws = match Workspace::load() {
        Ok(ws) => ws,
        Err(err) => {
            eprintln!("review-checks: {err}");
            return ExitCode::FAILURE;
        }
    };
    let mut errors = boundary_findings(&ws);
    let mut warnings = Vec::new();
    for src in &ws.files {
        debug_secrets(src, &mut errors);
        sql_format(src, &mut errors);
        latest_tag(src, &mut errors);
        tracing_case(src, &mut warnings);
        bare_ignore(src, &mut warnings);
    }
    core_12(&ws.root, base, &mut warnings);
    let ok = report(&errors, &warnings);
    eprintln!(
        "review-checks: {} error(s), {} warning(s) in {} ms",
        errors.len(),
        warnings.len(),
        start.elapsed().as_millis()
    );
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn report(errors: &[Finding], warnings: &[Finding]) -> bool {
    for finding in errors {
        println!("error: {finding}");
    }
    for finding in warnings {
        println!("warning: {finding}");
    }
    errors.is_empty()
}

impl Workspace {
    fn load() -> Result<Self, String> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(Path::to_path_buf)
            .ok_or("could not resolve the workspace root")?;
        let mut forbidden = Vec::new();
        let mut files = Vec::new();
        for path in git(
            &root,
            &["ls-files", "--cached", "--others", "--exclude-standard"],
        )?
        .lines()
        {
            if is_forbidden_path(path) {
                forbidden.push(path.to_owned());
                continue;
            }
            if !is_checked_file(path) {
                continue;
            }
            // Tracked files deleted in the working tree are skipped.
            if let Ok(text) = fs::read_to_string(root.join(path)) {
                files.push(Source {
                    path: path.to_owned(),
                    text,
                });
            }
        }
        Ok(Self {
            root,
            forbidden,
            files,
        })
    }
}

fn is_checked_file(path: &str) -> bool {
    let code = (path.starts_with("crates/") || path.starts_with("xtask/"))
        && (path.ends_with(".rs") || path.ends_with("/Cargo.toml"));
    let deploy = path.starts_with("deploy/")
        && (path.ends_with(".yml")
            || path.ends_with(".yaml")
            || path.ends_with(".sh")
            || path.ends_with("Dockerfile"));
    code || deploy || path == "Cargo.toml"
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|err| format!("could not run git: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|err| format!("git output is not utf-8: {err}"))
}

fn boundary_findings(ws: &Workspace) -> Vec<Finding> {
    let mut out = Vec::new();
    for src in &ws.files {
        sdk_outside_core(src, &mut out);
        sdk_in_core_public_api(src, &mut out);
        protocol_purity(src, &mut out);
        workspace_deps(src, &mut out);
        unsafe_outside_core(src, &mut out);
    }
    for path in &ws.forbidden {
        out.push(Finding::new(
            path,
            0,
            "OPS-06",
            "arquivo proibido versionado ou fora do .gitignore; tire do git com `git rm --cached` e cubra no .gitignore",
        ));
    }
    out
}

/// Linhas antes do primeiro `#[cfg(test)]`, numeradas a partir de 1, sem linhas de comentário.
fn code_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .enumerate()
        .map(|(idx, line)| (idx + 1, line))
        .take_while(|(_, line)| line.trim() != "#[cfg(test)]")
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
}

/// Dependências declaradas no manifesto: (número da linha, seção, nome, linha inteira).
fn manifest_deps(text: &str) -> Vec<(usize, String, &str, &str)> {
    let mut section = String::new();
    let mut deps = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).to_owned();
            continue;
        }
        if !section.ends_with("dependencies") || line.starts_with('#') {
            continue;
        }
        if let Some((name, _)) = line.split_once('=') {
            let name = name.split('.').next().unwrap_or(name).trim();
            deps.push((idx + 1, section.clone(), name, line));
        }
    }
    deps
}

fn uses_sdk(line: &str) -> Option<&'static str> {
    SDK_CRATES.iter().copied().find(|sdk| {
        let pattern = format!("{sdk}::");
        line.match_indices(&pattern).any(|(at, _)| {
            !line[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
    })
}

fn sdk_outside_core(src: &Source, out: &mut Vec<Finding>) {
    if src.path.starts_with(CORE_DIR) || src.path == "Cargo.toml" {
        return;
    }
    if src.path.ends_with("Cargo.toml") {
        for (line, _, name, _) in manifest_deps(&src.text) {
            if SDK_CRATES.contains(&name) {
                out.push(Finding::new(
                    &src.path,
                    line,
                    "CORE-01",
                    format!("só rooms-core pode depender de `{name}`; leve o uso para rooms-core e exponha um `Command`/`Event`"),
                ));
            }
        }
        return;
    }
    for (line, text) in code_lines(&src.text) {
        if let Some(sdk) = uses_sdk(text) {
            out.push(Finding::new(
                &src.path,
                line,
                "CORE-01",
                format!("`{sdk}` usado fora de rooms-core; mova o código para rooms-core e fale com ela por `Command`/`Event`"),
            ));
        }
    }
}

fn sdk_in_core_public_api(src: &Source, out: &mut Vec<Finding>) {
    if !CORE_PUBLIC_FILES.contains(&src.path.as_str()) {
        return;
    }
    for (line, text) in code_lines(&src.text) {
        if !text.trim_start().starts_with("pub ") {
            continue;
        }
        if let Some(sdk) = uses_sdk(text) {
            out.push(Finding::new(
                &src.path,
                line,
                "CORE-01",
                format!("item público de rooms-core expõe tipo de `{sdk}`; troque por um tipo próprio da crate ou deixe o item `pub(crate)`"),
            ));
        }
    }
}

fn protocol_purity(src: &Source, out: &mut Vec<Finding>) {
    if !src.path.starts_with(PROTOCOL_DIR) {
        return;
    }
    if src.path.ends_with("Cargo.toml") {
        for (line, section, name, _) in manifest_deps(&src.text) {
            if section == "dependencies" && !PROTOCOL_DEPS.contains(&name) {
                out.push(Finding::new(
                    &src.path,
                    line,
                    "PROTO-01",
                    format!("rooms-protocol só depende de serde e thiserror; remova `{name}` ou mova a lógica para quem usa o protocolo"),
                ));
            }
        }
        return;
    }
    for (line, text) in code_lines(&src.text) {
        if let Some(token) = PROTOCOL_FORBIDDEN
            .iter()
            .find(|token| text.contains(**token))
        {
            out.push(Finding::new(
                &src.path,
                line,
                "PROTO-01",
                format!("rooms-protocol não faz I/O nem async (`{token}`); deixe só tipos serde e validação pura"),
            ));
        }
    }
}

fn workspace_deps(src: &Source, out: &mut Vec<Finding>) {
    if src.path == "Cargo.toml" {
        for (line, section, name, decl) in manifest_deps(&src.text) {
            if section == "workspace.dependencies"
                && EXACT_PINS.contains(&name)
                && !decl.contains("\"=")
            {
                out.push(Finding::new(
                    &src.path,
                    line,
                    "GER-07",
                    format!("`{name}` precisa de versão exata; use `\"=x.y.z\"`"),
                ));
            }
        }
        return;
    }
    if !src.path.ends_with("Cargo.toml") {
        return;
    }
    for (line, _, name, decl) in manifest_deps(&src.text) {
        if !decl.contains("workspace = true") && !decl.contains("workspace=true") {
            out.push(Finding::new(
                &src.path,
                line,
                "GER-07",
                format!("`{name}` declarado fora de `[workspace.dependencies]`; mova a versão para o Cargo.toml da raiz e use `{name}.workspace = true`"),
            ));
        }
    }
}

fn unsafe_outside_core(src: &Source, out: &mut Vec<Finding>) {
    if src.path.starts_with(CORE_DIR) || !src.path.ends_with(".rs") {
        return;
    }
    let marker = "unsafe_code";
    for (line, text) in code_lines(&src.text) {
        let text = text.trim();
        let attribute = ["#[allow(", "#[expect(", "#![allow(", "#![expect("]
            .iter()
            .any(|prefix| {
                text.strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with(marker))
            });
        if attribute || text.starts_with(marker) {
            out.push(Finding::new(
                &src.path,
                line,
                "GER-03",
                "liberação de `unsafe` fora de rooms-core; reescreva sem unsafe ou mova o trecho para rooms-core com `#[expect(..)]` e `// SAFETY:`",
            ));
        }
    }
}

fn debug_secrets(src: &Source, out: &mut Vec<Finding>) {
    if !src.path.ends_with(".rs") {
        return;
    }
    let mut derive_line = None;
    let mut in_struct = false;
    for (line, text) in code_lines(&src.text) {
        let text = text.trim();
        if in_struct {
            if text.starts_with('}') {
                in_struct = false;
                derive_line = None;
                continue;
            }
            let field = text
                .trim_start_matches("pub(crate) ")
                .trim_start_matches("pub ")
                .split(':')
                .next()
                .unwrap_or_default()
                .trim();
            if SECRET_FIELDS.contains(&field)
                && let Some(at) = derive_line
            {
                out.push(Finding::new(
                    &src.path,
                    at,
                    "GER-06",
                    format!("`#[derive(Debug)]` em struct com o campo secreto `{field}`; tire `Debug` do derive e implemente `fmt::Debug` à mão omitindo o campo"),
                ));
            }
        } else if text.starts_with("#[derive(") && text.contains("Debug") {
            derive_line = Some(line);
        } else if derive_line.is_some() && text.contains("struct ") {
            in_struct = text.ends_with('{');
            if !in_struct {
                derive_line = None;
            }
        } else if !text.starts_with("#[") {
            derive_line = None;
        }
    }
}

fn sql_format(src: &Source, out: &mut Vec<Finding>) {
    if !src.path.starts_with(SERVER_DIR) {
        return;
    }
    for (line, text) in code_lines(&src.text) {
        if text.contains("format!(") && SQL_MARKERS.iter().any(|m| text.contains(m)) {
            out.push(Finding::new(
                &src.path,
                line,
                "SRV-05",
                "SQL montado com `format!`; use SQL fixo com `?N` e `params![]`",
            ));
        }
    }
}

fn latest_tag(src: &Source, out: &mut Vec<Finding>) {
    if !src.path.starts_with("deploy/") {
        return;
    }
    for (idx, text) in src.text.lines().enumerate() {
        if text.contains(":latest") {
            out.push(Finding::new(
                &src.path,
                idx + 1,
                "OPS-03",
                "imagem com tag `:latest`; fixe a versão (ex.: `v1.13.11`)",
            ));
        }
    }
}

fn tracing_case(src: &Source, out: &mut Vec<Finding>) {
    if !src.path.ends_with(".rs") {
        return;
    }
    let lines: Vec<&str> = src.text.lines().collect();
    for (line, text) in code_lines(&src.text) {
        let Some(open) = TRACING_MACROS
            .iter()
            .find_map(|m| macro_args_start(text, m))
        else {
            continue;
        };
        let call: String = std::iter::once(&text[open..])
            .chain(lines.iter().skip(line).take(6).copied())
            .collect::<Vec<_>>()
            .join(" ");
        let call = call.split(");").next().unwrap_or_default();
        if tracing_message(call).is_some_and(|msg| msg.starts_with(char::is_uppercase)) {
            out.push(Finding::new(
                &src.path,
                line,
                "GER-04",
                "mensagem de log começa com maiúscula; logs são em inglês e minúsculos",
            ));
        }
    }
}

fn macro_args_start(text: &str, name: &str) -> Option<usize> {
    text.match_indices(name).find_map(|(at, _)| {
        let standalone = !text[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        standalone.then_some(at + name.len())
    })
}

/// Primeiro literal de string que não é valor de campo (`campo = "..."`).
fn tracing_message(call: &str) -> Option<&str> {
    let mut rest = call;
    loop {
        let quote = rest.find('"')?;
        let is_field = rest[..quote].trim_end().ends_with('=');
        let body = &rest[quote + 1..];
        if !is_field {
            return Some(body);
        }
        let end = body.find('"')?;
        rest = &body[end + 1..];
    }
}

fn bare_ignore(src: &Source, out: &mut Vec<Finding>) {
    if !src.path.ends_with(".rs") {
        return;
    }
    for (idx, text) in src.text.lines().enumerate() {
        if text.trim() == "#[ignore]" {
            out.push(Finding::new(
                &src.path,
                idx + 1,
                "TST-04",
                "`#[ignore]` sem motivo; use `#[ignore = \"motivo\"]` ou faça o teste rodar",
            ));
        }
    }
}

fn core_12(root: &Path, base: &str, out: &mut Vec<Finding>) {
    let diff = match git(root, &["diff", "-U0", base]) {
        Ok(diff) => diff,
        Err(err) => {
            out.push(Finding::new(
                CORE_API,
                0,
                "CORE-12",
                format!("checagem pulada: {err}"),
            ));
            return;
        }
    };
    let Ok(api) = fs::read_to_string(root.join(CORE_API)) else {
        return;
    };
    if let Some((line, missing)) = core_12_missing(&diff, &api) {
        out.push(Finding::new(
            CORE_API,
            line,
            "CORE-12",
            format!(
                "`Command`/`Event` mudou mas estes pontos de ligação não: {}; confira se a variante nova é tratada em cada um",
                missing.join(", ")
            ),
        ));
    }
}

fn core_12_missing(diff: &str, api: &str) -> Option<(usize, Vec<&'static str>)> {
    let spans: Vec<(usize, usize)> = ["pub enum Command", "pub enum Event"]
        .iter()
        .filter_map(|decl| enum_span(api, decl))
        .collect();
    let mut changed = Vec::new();
    let mut enum_start = None;
    let mut in_api = false;
    for line in diff.lines() {
        if let Some(paths) = line.strip_prefix("diff --git ") {
            let path = paths.rsplit(" b/").next().unwrap_or(paths);
            in_api = path == CORE_API;
            changed.push(path);
        } else if in_api
            && enum_start.is_none()
            && let Some((a, b)) = hunk_new_range(line)
        {
            enum_start = spans
                .iter()
                .find(|&&(s, e)| a <= e && s <= b)
                .map(|span| span.0);
        }
    }
    let missing: Vec<&'static str> = CORE_12_BINDINGS
        .iter()
        .copied()
        .filter(|binding| !changed.iter().any(|path| path.starts_with(binding)))
        .collect();
    enum_start
        .filter(|_| !missing.is_empty())
        .map(|start| (start, missing))
}

fn enum_span(src: &str, decl: &str) -> Option<(usize, usize)> {
    let start = src.lines().position(|line| line.starts_with(decl))?;
    let len = src.lines().skip(start).position(|line| line == "}")?;
    Some((start + 1, start + len + 1))
}

fn hunk_new_range(line: &str) -> Option<(usize, usize)> {
    let new = line
        .strip_prefix("@@ ")?
        .split(' ')
        .nth(1)?
        .strip_prefix('+')?;
    let (start, count) = new.split_once(',').unwrap_or((new, "1"));
    let start: usize = start.parse().ok()?;
    let count: usize = count.parse().ok()?;
    Some((start, start + count.saturating_sub(1)))
}

fn is_forbidden_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let env = name == ".env" || (name.starts_with(".env.") && name != ".env.example");
    env || [".pem", ".key", ".tfvars", ".tfvars.json"]
        .iter()
        .any(|ext| name.ends_with(ext))
        || path == "deploy/livekit.prod.yaml"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(path: &str, text: &str) -> Source {
        Source {
            path: path.to_owned(),
            text: text.to_owned(),
        }
    }

    fn rules(check: fn(&Source, &mut Vec<Finding>), path: &str, text: &str) -> Vec<usize> {
        let mut out = Vec::new();
        check(&src(path, text), &mut out);
        out.iter().map(|finding| finding.line).collect()
    }

    #[test]
    fn sdk_paths_need_a_word_boundary() {
        assert_eq!(uses_sdk("use livekit::Room;"), Some("livekit"));
        assert_eq!(
            uses_sdk("(libwebrtc::video_frame::I420Buffer)"),
            Some("libwebrtc")
        );
        assert_eq!(uses_sdk("use livekit_api::access_token;"), None);
        assert_eq!(uses_sdk("rooms_livekit::x()"), None);
    }

    #[test]
    fn sdk_outside_core_skips_core_comments_and_tests() {
        let client = "use livekit::Room;\n// livekit::x\n#[cfg(test)]\nuse livekit::y;\n";
        assert_eq!(
            rules(sdk_outside_core, "crates/rooms-client/src/a.rs", client),
            [1]
        );
        assert!(rules(sdk_outside_core, "crates/rooms-core/src/a.rs", client).is_empty());
        let manifest = "[dependencies]\nlivekit.workspace = true\nlivekit-api.workspace = true\n";
        assert_eq!(
            rules(sdk_outside_core, "crates/rooms-e2e/Cargo.toml", manifest),
            [2]
        );
    }

    #[test]
    fn core_public_api_flags_only_pub_items() {
        let api = "pub fn a(r: livekit::Room) {}\npub(crate) fn b(r: livekit::Room) {}\n";
        assert_eq!(rules(sdk_in_core_public_api, CORE_API, api), [1]);
    }

    #[test]
    fn protocol_allows_serde_and_thiserror_only() {
        let manifest = "[dependencies]\nserde.workspace = true\nthiserror.workspace = true\ntokio.workspace = true\n[dev-dependencies]\nserde_json.workspace = true\n";
        assert_eq!(
            rules(
                protocol_purity,
                "crates/rooms-protocol/Cargo.toml",
                manifest
            ),
            [4]
        );
        let code = "pub struct A;\npub async fn b() {}\n";
        assert_eq!(
            rules(protocol_purity, "crates/rooms-protocol/src/lib.rs", code),
            [2]
        );
    }

    #[test]
    fn crate_deps_must_come_from_the_workspace() {
        let manifest = "[package]\nname = \"x\"\n[target.'cfg(windows)'.dependencies]\nrfd = { version = \"0.15\" }\nkeyring.workspace = true\nrooms-core = { workspace = true, features = [\"test-util\"] }\n";
        assert_eq!(
            rules(workspace_deps, "crates/rooms-client/Cargo.toml", manifest),
            [4]
        );
        let root = "[workspace.dependencies]\nlivekit = { version = \"0.10.1\" }\nlivekit-api = { version = \"=0.10.1\" }\n";
        assert_eq!(rules(workspace_deps, "Cargo.toml", root), [2]);
    }

    #[test]
    fn unsafe_attributes_are_only_allowed_in_core() {
        let code = "#[expect(\n    unsafe_code,\n    reason = \"ffi\"\n)]\n#[allow(unsafe_code)]\nlet m = \"unsafe_code\";\n";
        assert_eq!(rules(unsafe_outside_core, "xtask/src/a.rs", code), [2, 5]);
        assert!(rules(unsafe_outside_core, "crates/rooms-core/src/a.rs", code).is_empty());
    }

    #[test]
    fn derived_debug_with_secret_field_is_flagged() {
        let code = "#[derive(Debug, Clone)]\npub struct Config {\n    pub bind: String,\n    pub livekit_api_secret: String,\n}\n#[derive(Clone)]\npub struct Safe {\n    api_secret: String,\n}\n";
        assert_eq!(
            rules(
                debug_secrets,
                "crates/rooms-token-server/src/config.rs",
                code
            ),
            [1]
        );
    }

    #[test]
    fn sql_and_deploy_and_ignore_rules() {
        let sql = "let q = format!(\"SELECT * FROM {t}\");\nlet id = format!(\"msg-{i}\");\n";
        assert_eq!(
            rules(
                sql_format,
                "crates/rooms-token-server/src/chat_store.rs",
                sql
            ),
            [1]
        );
        let compose = "image: caddy:2.11.9-alpine\nimage: livekit/livekit-server:latest\n";
        assert_eq!(rules(latest_tag, "deploy/docker-compose.yml", compose), [2]);
        let test = "#[test]\n#[ignore]\nfn a() {}\n#[ignore = \"needs hardware\"]\nfn b() {}\n";
        assert_eq!(rules(bare_ignore, "crates/rooms-e2e/src/a.rs", test), [2]);
    }

    #[test]
    fn tracing_case_skips_field_values() {
        let code = "tracing::warn!(\n    var = \"ROOMS_X\",\n    \"using plaintext\"\n);\ninfo!(\"Connected\");\neprintln!(\"Usage\");\n";
        assert_eq!(rules(tracing_case, "crates/a/src/a.rs", code), [5]);
    }

    #[test]
    fn forbidden_paths() {
        assert!(is_forbidden_path(".env"));
        assert!(is_forbidden_path("deploy/.env.prod"));
        assert!(is_forbidden_path("deploy/livekit.prod.yaml"));
        assert!(is_forbidden_path("certs/server.pem"));
        assert!(!is_forbidden_path(".env.example"));
        assert!(!is_forbidden_path("deploy/livekit.example.yaml"));
    }

    #[test]
    fn core_12_lists_untouched_bindings_when_enums_change() {
        let api = "pub struct A;\npub enum Command {\n    Join,\n    Leave,\n}\npub enum Event {\n    Joined,\n}\n";
        let others = "diff --git a/crates/rooms-core/src/session.rs b/crates/rooms-core/src/session.rs\n@@ -9 +9 @@\ndiff --git a/crates/rooms-client/src/state.rs b/crates/rooms-client/src/state.rs\n@@ -1,0 +2 @@\n";
        let api_head = "diff --git a/crates/rooms-core/src/api.rs b/crates/rooms-core/src/api.rs\n";
        let in_command =
            format!("{api_head}@@ -3,0 +4 @@ pub enum Command {{\n+    Mute,\n{others}");
        let outside = format!("{api_head}@@ -1 +1 @@\n-pub struct A;\n+pub struct B;\n{others}");
        let (line, missing) = core_12_missing(&in_command, api).unwrap_or_default();
        assert_eq!(line, 2);
        assert!(!missing.contains(&"crates/rooms-core/src/session.rs"));
        assert!(!missing.contains(&"crates/rooms-client/src/"));
        assert!(missing.contains(&"crates/rooms-e2e/src/protocol.rs"));
        assert_eq!(core_12_missing(&outside, api), None);
        assert_eq!(core_12_missing(others, api), None);
    }
}
