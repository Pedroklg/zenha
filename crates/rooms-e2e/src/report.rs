//! Relatório final em Markdown (português) e JSON.

use std::fmt::Write as _;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::detect::{TONE_AMPLITUDE, TONE_HZ};
use crate::probe::RenderDevice;
use crate::protocol::DeviceInfo;
use crate::scenario::{ScenarioResult, Status};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Environment {
    pub started_at_utc: String,
    pub harness_version: String,
    pub sdk_versions: String,
    pub livekit_server: Option<String>,
    pub os: String,
    pub default_render_device: Option<RenderDevice>,
    pub render_devices: Vec<RenderDevice>,
    pub listener_outputs: Vec<DeviceInfo>,
    pub listener_inputs: Vec<DeviceInfo>,
    pub listener_selected_output: Option<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub environment: Environment,
    pub scenarios: Vec<ScenarioResult>,
    pub total_duration_ms: u64,
}

pub fn utc_timestamp(unix_secs: u64) -> String {
    let days = i64::try_from(unix_secs / 86_400).unwrap_or(0);
    let secs = unix_secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

pub fn now_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    utc_timestamp(secs)
}

fn seconds(ms: u64) -> String {
    format!("{:.1} s", ms as f64 / 1000.0)
}

fn status_label(status: Status) -> &'static str {
    match status {
        Status::Pass => "PASSOU",
        Status::Fail => "FALHOU",
        Status::Skipped => "IGNORADO",
    }
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

impl Report {
    pub fn count(&self, status: Status) -> usize {
        self.scenarios
            .iter()
            .filter(|scenario| scenario.status == status)
            .count()
    }

    /// Skipped scenarios count as success only when `allow_skipped` is set.
    pub fn success(&self, allow_skipped: bool) -> bool {
        self.count(Status::Fail) == 0 && (allow_skipped || self.count(Status::Skipped) == 0)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|err| format!("{{\"error\":\"{err}\"}}"))
    }

    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        let env = &self.environment;
        let _ = writeln!(out, "# Relatório de testes ponta a ponta\n");
        let _ = writeln!(
            out,
            "Gerado em {} (UTC) por rooms-e2e {}. Duração total: {}.\n",
            env.started_at_utc,
            env.harness_version,
            seconds(self.total_duration_ms)
        );
        let _ = writeln!(
            out,
            "> **Atenção:** durante esta execução um tom de teste audível ({TONE_HZ} Hz, volume baixo, amplitude {TONE_AMPLITUDE}) é tocado no dispositivo de saída padrão do Windows, em vários momentos. Avise quem estiver por perto e não use o PC para chamadas ou música durante o teste.\n"
        );
        let _ = writeln!(
            out,
            "**Resultado: {} passaram, {} falharam, {} ignorados (de {}).**\n",
            self.count(Status::Pass),
            self.count(Status::Fail),
            self.count(Status::Skipped),
            self.scenarios.len()
        );

        let _ = writeln!(out, "## Resumo\n");
        let _ = writeln!(
            out,
            "| # | Cenário | Resultado | Duração | Medições principais |"
        );
        let _ = writeln!(out, "| --- | --- | --- | --- | --- |");
        for (index, scenario) in self.scenarios.iter().enumerate() {
            let main: Vec<String> = scenario
                .measurements
                .iter()
                .take(3)
                .map(|m| {
                    format!("{} = {} {}", m.name, m.value.display(), m.unit)
                        .trim_end()
                        .to_owned()
                })
                .collect();
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} | {} |",
                index + 1,
                cell(&scenario.name),
                status_label(scenario.status),
                seconds(scenario.duration_ms),
                cell(&main.join("; "))
            );
        }

        let _ = writeln!(out, "\n## Detalhes\n");
        for (index, scenario) in self.scenarios.iter().enumerate() {
            let _ = writeln!(
                out,
                "### {}. {} ({})\n",
                index + 1,
                scenario.name,
                scenario.title
            );
            let _ = writeln!(
                out,
                "**{}** em {}.\n",
                status_label(scenario.status),
                seconds(scenario.duration_ms)
            );
            if let Some(failure) = &scenario.failure {
                let _ = writeln!(out, "**Motivo da falha:** {failure}\n");
            }
            if !scenario.steps.is_empty() {
                let _ = writeln!(out, "Passos:\n");
                for step in &scenario.steps {
                    let _ = writeln!(out, "- {step}");
                }
                out.push('\n');
            }
            if !scenario.measurements.is_empty() {
                let _ = writeln!(out, "| Medição | Valor | Unidade |");
                let _ = writeln!(out, "| --- | --- | --- |");
                for m in &scenario.measurements {
                    let _ = writeln!(
                        out,
                        "| {} | {} | {} |",
                        cell(&m.name),
                        cell(&m.value.display()),
                        cell(&m.unit)
                    );
                }
                out.push('\n');
            }
            for log in &scenario.bot_logs {
                let _ = writeln!(out, "Últimas linhas do log do bot `{}`:\n", log.bot);
                let _ = writeln!(out, "```");
                for line in &log.tail {
                    let _ = writeln!(out, "{line}");
                }
                let _ = writeln!(out, "```\n");
            }
        }

        let _ = writeln!(out, "## Ambiente\n");
        let _ = writeln!(out, "- Sistema: {}", env.os);
        let _ = writeln!(out, "- SDK: {}", env.sdk_versions);
        let _ = writeln!(
            out,
            "- livekit-server: {}",
            env.livekit_server.as_deref().unwrap_or("não identificado")
        );
        match &env.default_render_device {
            Some(device) => {
                let _ = writeln!(
                    out,
                    "- Dispositivo de saída padrão (onde a sonda escutou): {} (`{}`)",
                    device.name, device.id
                );
            }
            None => {
                let _ = writeln!(out, "- Dispositivo de saída padrão: indisponível");
            }
        }
        let _ = writeln!(out, "- Dispositivos de saída do Windows:");
        for device in &env.render_devices {
            let mark = if device.is_default { " (padrão)" } else { "" };
            let _ = writeln!(out, "  - {}{} `{}`", device.name, mark, device.id);
        }
        let _ = writeln!(out, "- Saídas reportadas pelo cliente (ouvinte):");
        for device in &env.listener_outputs {
            let _ = writeln!(out, "  - {} `{}`", device.name, device.id);
        }
        let _ = writeln!(
            out,
            "- Saída selecionada no ouvinte: {}",
            env.listener_selected_output
                .as_deref()
                .unwrap_or("padrão do sistema")
        );
        let _ = writeln!(out, "- Entradas reportadas pelo cliente (ouvinte):");
        for device in &env.listener_inputs {
            let _ = writeln!(out, "  - {} `{}`", device.name, device.id);
        }
        for note in &env.notes {
            let _ = writeln!(out, "- Nota: {note}");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{BotLog, Measurement, Value};

    fn scenario(name: &str, status: Status) -> ScenarioResult {
        ScenarioResult {
            name: name.into(),
            title: "Título".into(),
            status,
            duration_ms: 2500,
            steps: vec!["[+0.1s] passo".into()],
            measurements: vec![Measurement {
                name: "tone_rms".into(),
                value: Value::Number(0.0212),
                unit: "".into(),
            }],
            failure: (status == Status::Fail).then(|| "o tom não apareceu".to_owned()),
            bot_logs: if status == Status::Fail {
                vec![BotLog {
                    bot: "listener".into(),
                    tail: vec!["linha de log".into()],
                }]
            } else {
                Vec::new()
            },
        }
    }

    fn report() -> Report {
        Report {
            environment: Environment {
                started_at_utc: "2026-11-12T10:00:00Z".into(),
                harness_version: "0.1.0".into(),
                sdk_versions: "livekit 0.10.1".into(),
                default_render_device: Some(RenderDevice {
                    id: "{id}".into(),
                    name: "Speakers".into(),
                    is_default: true,
                }),
                ..Environment::default()
            },
            scenarios: vec![
                scenario("token", Status::Pass),
                scenario("playout-tone-first", Status::Fail),
                scenario("share", Status::Skipped),
            ],
            total_duration_ms: 9000,
        }
    }

    #[test]
    fn markdown_has_warning_summary_failure_and_environment() {
        let text = report().to_markdown();
        assert!(text.contains("tom de teste audível"));
        assert!(text.contains("**Resultado: 1 passaram, 1 falharam, 1 ignorados (de 3).**"));
        assert!(text.contains("| 2 | playout-tone-first | FALHOU | 2.5 s |"));
        assert!(text.contains("**Motivo da falha:** o tom não apareceu"));
        assert!(text.contains("```\nlinha de log\n```"));
        assert!(text.contains("Speakers"));
        let summary_at = text.find("## Resumo").unwrap();
        let details_at = text.find("## Detalhes").unwrap();
        let env_at = text.find("## Ambiente").unwrap();
        assert!(summary_at < details_at && details_at < env_at);
    }

    #[test]
    fn json_round_trips() {
        let original = report();
        let parsed: Report = serde_json::from_str(&original.to_json()).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn success_rules() {
        let mut r = report();
        assert!(!r.success(true));
        r.scenarios.remove(1);
        assert!(!r.success(false));
        assert!(r.success(true));
    }

    #[test]
    fn timestamps_are_formatted_in_utc() {
        assert_eq!(utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(1_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(utc_timestamp(1_794_484_800), "2026-11-12T12:00:00Z");
    }
}
