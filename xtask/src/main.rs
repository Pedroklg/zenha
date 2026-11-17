//! Repository automation: local verification and asset generation.

mod icon;
mod review_checks;

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
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("ci") => ci(),
        Some("icon") => run_icon(),
        Some("review-checks") => review_checks::run(args.get(1).map_or("HEAD", String::as_str)),
        other => {
            eprintln!("unknown task: {other:?}\nusage: cargo xtask <ci|icon|review-checks>");
            ExitCode::FAILURE
        }
    }
}

fn ci() -> ExitCode {
    eprintln!("==> boundaries");
    if !review_checks::boundaries() {
        return ExitCode::FAILURE;
    }
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
    deny(&cargo)
}

fn deny(cargo: &str) -> ExitCode {
    eprintln!("==> deny");
    match Command::new(cargo).args(["deny", "check"]).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            eprintln!("deny failed: {status} (if cargo-deny is missing: cargo install cargo-deny)");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("could not run cargo deny: {err}");
            ExitCode::FAILURE
        }
    }
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
