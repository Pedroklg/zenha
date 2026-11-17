use std::process::ExitCode;

use zenha_e2e::bot_main::bot_main;
use zenha_e2e::cli::{self, BotArgs, Cli, RunArgs, USAGE};
use zenha_e2e::runner;

const NOT_WINDOWS: &str = "zenha-e2e: o harness só roda no Windows (precisa do LiveKit, do áudio da plataforma e do WASAPI). Nada foi executado.";

fn run(args: &RunArgs) -> ExitCode {
    if cfg!(not(windows)) && !args.allow_non_windows && !args.list {
        println!("{NOT_WINDOWS}");
        return ExitCode::SUCCESS;
    }
    match runner::run(args) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(_) => ExitCode::from(1),
        Err(err) => {
            eprintln!("zenha-e2e: {err:#}");
            ExitCode::from(2)
        }
    }
}

fn bot(args: &BotArgs) -> ExitCode {
    if cfg!(not(windows)) && !args.allow_non_windows {
        println!("{NOT_WINDOWS}");
        return ExitCode::SUCCESS;
    }
    match bot_main(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("zenha-e2e bot: {err:#}");
            ExitCode::from(2)
        }
    }
}

#[cfg(windows)]
fn side_tone(hz: f64, amplitude: f64) -> ExitCode {
    match zenha_e2e::loopback::side_tone_main(hz, amplitude) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("zenha-e2e side-tone: {err:#}");
            ExitCode::from(2)
        }
    }
}

#[cfg(not(windows))]
fn side_tone(_hz: f64, _amplitude: f64) -> ExitCode {
    println!("{NOT_WINDOWS}");
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse(&args) {
        Ok(Cli::Run(run_args)) => run(&run_args),
        Ok(Cli::Bot(bot_args)) => bot(&bot_args),
        Ok(Cli::SideTone { hz, amplitude }) => side_tone(hz, amplitude),
        Ok(Cli::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("zenha-e2e: {message}\n{USAGE}");
            ExitCode::from(2)
        }
    }
}
