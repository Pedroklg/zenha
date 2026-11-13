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
        other => {
            eprintln!("unknown task: {other:?}\nusage: cargo xtask ci");
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
