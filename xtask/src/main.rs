//! Automação do repositório: verificação local e geração de assets.

mod icon;

use std::env;
use std::process::{Command, ExitCode};

const STEPS: &[(&str, &[&str])] = &[
    ("fmt", &["fmt", "--all", "--check"]),
    (
        "clippy",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    ),
    ("test", &["test", "--workspace"]),
];

fn main() -> ExitCode {
    match env::args().nth(1).as_deref() {
        Some("ci") => ci(),
        Some("icon") => run_icon(),
        other => {
            eprintln!("unknown task: {other:?}\nusage: cargo xtask <ci|icon>");
            ExitCode::FAILURE
        }
    }
}

fn ci() -> ExitCode {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    for (name, args) in STEPS {
        eprintln!("==> {name}");
        match Command::new(&cargo).args(*args).status() {
            Ok(status) if status.success() => {}
            Ok(status) => {
                eprintln!("{name} failed: {status}");
                return ExitCode::FAILURE;
            }
            Err(err) => {
                eprintln!("could not run cargo {name}: {err}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

fn run_icon() -> ExitCode {
    let path = match icon::default_output_path() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("could not resolve icon output path: {err}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(err) = icon::generate(&path) {
        eprintln!("icon generation failed: {err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
