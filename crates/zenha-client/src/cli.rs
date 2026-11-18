use crate::invite::{self, Invite};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchArgs {
    pub demo: bool,
    /// `--debug`: more detailed logs, also on stdout when it is redirected.
    pub debug: bool,
    /// `--diagnostico`: writes a troubleshooting report and exits without opening the window.
    pub diagnostics: bool,
    pub startup_link: StartupLink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupLink {
    None,
    Invite(Invite),
    Invalid(String),
}

pub fn parse_args<I, S>(args: I) -> LaunchArgs
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut demo = false;
    let mut debug = false;
    let mut diagnostics = false;
    let mut first_arg: Option<String> = None;

    for raw in args {
        let arg = raw.as_ref();
        match arg {
            "--demo" => demo = true,
            "--debug" => debug = true,
            "--diagnostico" | "--diagnostics" => diagnostics = true,
            _ => {}
        }
        if arg.starts_with("--") {
            continue;
        }
        if first_arg.is_none() && !arg.trim().is_empty() {
            first_arg = Some(arg.to_owned());
        }
    }

    if demo {
        return LaunchArgs {
            demo: true,
            debug,
            diagnostics,
            startup_link: StartupLink::None,
        };
    }

    let startup_link = match first_arg {
        Some(raw) => match invite::parse(&raw) {
            Some(invite) => StartupLink::Invite(invite),
            None => StartupLink::Invalid(raw),
        },
        None => StartupLink::None,
    };

    LaunchArgs {
        demo: false,
        debug,
        diagnostics,
        startup_link,
    }
}

pub const fn compile_time_default_server() -> Option<&'static str> {
    option_env!("ZENHA_DEFAULT_SERVER")
}

pub fn resolve_initial_server(saved: &str, build_default: Option<&str>) -> String {
    if !saved.trim().is_empty() {
        return saved.to_owned();
    }
    build_default
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_args_launch_normally_without_link() {
        let parsed = parse_args::<[&str; 0], &str>([]);
        assert_eq!(
            parsed,
            LaunchArgs {
                demo: false,
                debug: false,
                diagnostics: false,
                startup_link: StartupLink::None,
            }
        );
    }

    #[test]
    fn demo_flag_is_recognized_without_treating_it_as_invalid_link() {
        let parsed = parse_args(["--demo"]);
        assert_eq!(
            parsed,
            LaunchArgs {
                demo: true,
                debug: false,
                diagnostics: false,
                startup_link: StartupLink::None,
            }
        );
    }

    #[test]
    fn demo_flag_ignores_trailing_link_argument() {
        let parsed = parse_args(["--demo", "not-a-link"]);
        assert_eq!(
            parsed,
            LaunchArgs {
                demo: true,
                debug: false,
                diagnostics: false,
                startup_link: StartupLink::None,
            }
        );
    }

    #[test]
    fn blank_arguments_are_ignored() {
        let parsed = parse_args(["", "   "]);
        assert_eq!(
            parsed,
            LaunchArgs {
                demo: false,
                debug: false,
                diagnostics: false,
                startup_link: StartupLink::None,
            }
        );
    }

    #[test]
    fn valid_invite_link_is_parsed() {
        let link = invite::format("https://voz.exemplo.com", "segredo");
        let parsed = parse_args([&link]);
        assert_eq!(
            parsed,
            LaunchArgs {
                demo: false,
                debug: false,
                diagnostics: false,
                startup_link: StartupLink::Invite(Invite {
                    server: "https://voz.exemplo.com".to_owned(),
                    password: "segredo".to_owned(),
                }),
            }
        );
    }

    #[test]
    fn invalid_link_argument_is_reported() {
        let parsed = parse_args(["zenha://invalido"]);
        assert_eq!(
            parsed,
            LaunchArgs {
                demo: false,
                debug: false,
                diagnostics: false,
                startup_link: StartupLink::Invalid("zenha://invalido".to_owned()),
            }
        );
    }

    #[test]
    fn http_url_is_not_treated_as_valid_zenha_invite_link() {
        let parsed = parse_args(["https://voz.exemplo.com"]);
        assert_eq!(
            parsed,
            LaunchArgs {
                demo: false,
                debug: false,
                diagnostics: false,
                startup_link: StartupLink::Invalid("https://voz.exemplo.com".to_owned()),
            }
        );
    }

    #[test]
    fn resolve_initial_server_uses_empty_when_saved_and_build_default_are_empty() {
        assert_eq!(resolve_initial_server("", None), "");
        assert_eq!(resolve_initial_server("   ", Some("")), "");
        assert_eq!(resolve_initial_server("", Some("   ")), "");
    }

    #[test]
    fn resolve_initial_server_uses_trimmed_build_default_when_saved_is_empty() {
        assert_eq!(
            resolve_initial_server("", Some("  https://voz.exemplo.com  ")),
            "https://voz.exemplo.com"
        );
    }

    #[test]
    fn resolve_initial_server_preserves_saved_user_server() {
        assert_eq!(
            resolve_initial_server("https://meu.servidor", Some("https://padrao.servidor")),
            "https://meu.servidor"
        );
    }

    #[test]
    fn debug_and_diagnostics_flags_are_not_links() {
        let parsed = parse_args(["--debug", "--diagnostico"]);
        assert_eq!(
            parsed,
            LaunchArgs {
                demo: false,
                debug: true,
                diagnostics: true,
                startup_link: StartupLink::None,
            }
        );
        let link = invite::format("https://voz.exemplo.com", "segredo");
        let parsed = parse_args(["--debug", link.as_str()]);
        assert!(parsed.debug && !parsed.diagnostics);
        assert!(matches!(parsed.startup_link, StartupLink::Invite(_)));
        assert!(parse_args(["--diagnostics"]).diagnostics);
    }
}
