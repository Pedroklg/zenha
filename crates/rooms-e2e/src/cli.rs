//! Argumentos de linha de comando, sem dependências externas.

use std::path::PathBuf;

pub const USAGE: &str = "uso:
  rooms-e2e run [--filter nome[,nome...]] [--list] [--repo-root CAMINHO]
  rooms-e2e bot --name NOME --token-url URL --invite CODIGO [--tone HZ] [--amplitude A] [--test-pattern]";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunArgs {
    pub filters: Vec<String>,
    pub list: bool,
    pub allow_non_windows: bool,
    pub repo_root: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BotArgs {
    pub name: String,
    pub token_url: String,
    pub invite: String,
    pub tone_hz: Option<f64>,
    pub amplitude: f64,
    pub test_pattern: bool,
    pub allow_non_windows: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cli {
    Run(RunArgs),
    Bot(BotArgs),
    /// Internal: plays a tone on a third output device until stdin closes.
    SideTone {
        hz: f64,
        amplitude: f64,
    },
    Help,
}

fn value<'a>(args: &mut impl Iterator<Item = &'a String>, flag: &str) -> Result<String, String> {
    args.next()
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value"))
}

fn number(text: &str, flag: &str) -> Result<f64, String> {
    text.parse()
        .map_err(|_| format!("{flag}: {text:?} is not a number"))
}

pub fn parse(args: &[String]) -> Result<Cli, String> {
    let mut iter = args.iter();
    match iter.next().map(String::as_str) {
        None | Some("help" | "--help" | "-h") => Ok(Cli::Help),
        Some("run") => parse_run(iter),
        Some("bot") => parse_bot(iter),
        Some("side-tone") => {
            let (mut hz, mut amplitude) = (880.0, 0.05);
            while let Some(flag) = iter.next() {
                match flag.as_str() {
                    "--hz" => hz = number(&value(&mut iter, "--hz")?, "--hz")?,
                    "--amplitude" => {
                        amplitude = number(&value(&mut iter, "--amplitude")?, "--amplitude")?;
                    }
                    other => return Err(format!("unknown option {other:?} for side-tone")),
                }
            }
            Ok(Cli::SideTone { hz, amplitude })
        }
        Some(other) => Err(format!("unknown command {other:?}")),
    }
}

fn parse_run<'a>(mut iter: impl Iterator<Item = &'a String>) -> Result<Cli, String> {
    let mut run = RunArgs::default();
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--filter" => {
                let list = value(&mut iter, "--filter")?;
                run.filters.extend(
                    list.split(',')
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned),
                );
            }
            "--list" => run.list = true,
            "--allow-non-windows" => run.allow_non_windows = true,
            "--repo-root" => run.repo_root = Some(PathBuf::from(value(&mut iter, "--repo-root")?)),
            other => return Err(format!("unknown option {other:?} for run")),
        }
    }
    Ok(Cli::Run(run))
}

fn parse_bot<'a>(mut iter: impl Iterator<Item = &'a String>) -> Result<Cli, String> {
    let (mut name, mut token_url, mut invite) = (None, None, None);
    let mut bot = BotArgs {
        name: String::new(),
        token_url: String::new(),
        invite: String::new(),
        tone_hz: None,
        amplitude: crate::detect::TONE_AMPLITUDE,
        test_pattern: false,
        allow_non_windows: false,
    };
    while let Some(flag) = iter.next() {
        match flag.as_str() {
            "--name" => name = Some(value(&mut iter, "--name")?),
            "--token-url" => token_url = Some(value(&mut iter, "--token-url")?),
            "--invite" => invite = Some(value(&mut iter, "--invite")?),
            "--tone" => bot.tone_hz = Some(number(&value(&mut iter, "--tone")?, "--tone")?),
            "--amplitude" => {
                bot.amplitude = number(&value(&mut iter, "--amplitude")?, "--amplitude")?;
            }
            "--test-pattern" => bot.test_pattern = true,
            "--allow-non-windows" => bot.allow_non_windows = true,
            other => return Err(format!("unknown option {other:?} for bot")),
        }
    }
    bot.name = name.ok_or("--name is required")?;
    bot.token_url = token_url.ok_or("--token-url is required")?;
    bot.invite = invite.ok_or("--invite is required")?;
    Ok(Cli::Bot(bot))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn run_accepts_filters_and_flags() {
        let parsed = parse(&args(&[
            "run",
            "--filter",
            "token, mute",
            "--filter",
            "chat",
            "--list",
        ]));
        assert_eq!(
            parsed,
            Ok(Cli::Run(RunArgs {
                filters: vec!["token".into(), "mute".into(), "chat".into()],
                list: true,
                ..RunArgs::default()
            }))
        );
        assert!(parse(&args(&["run", "--filter"])).is_err());
        assert!(parse(&args(&["run", "--nope"])).is_err());
    }

    #[test]
    fn bot_requires_name_url_and_invite() {
        let ok = parse(&args(&[
            "bot",
            "--name",
            "t",
            "--token-url",
            "http://x",
            "--invite",
            "c",
            "--tone",
            "1290",
            "--test-pattern",
        ]));
        match ok {
            Ok(Cli::Bot(bot)) => {
                assert_eq!(bot.name, "t");
                assert_eq!(bot.tone_hz, Some(1290.0));
                assert!(bot.test_pattern);
                assert!(!bot.allow_non_windows);
            }
            other => panic!("{other:?}"),
        }
        assert!(parse(&args(&["bot", "--name", "t"])).is_err());
        assert!(
            parse(&args(&[
                "bot",
                "--name",
                "t",
                "--token-url",
                "u",
                "--invite",
                "c",
                "--tone",
                "x"
            ]))
            .is_err()
        );
    }

    #[test]
    fn help_and_unknown_commands() {
        assert_eq!(parse(&[]), Ok(Cli::Help));
        assert_eq!(parse(&args(&["--help"])), Ok(Cli::Help));
        assert!(parse(&args(&["fly"])).is_err());
    }
}
