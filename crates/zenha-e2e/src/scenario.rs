//! Recording of each scenario's results: steps, measurements, failure and log excerpts.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Fail,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Number(f64),
    Bool(bool),
    Text(String),
}

impl Value {
    pub fn display(&self) -> String {
        match self {
            Self::Number(number) if number.fract() == 0.0 && number.abs() < 1e12 => {
                format!("{number:.0}")
            }
            Self::Number(number) => format!("{number:.4}"),
            Self::Bool(flag) => if *flag { "sim" } else { "não" }.to_owned(),
            Self::Text(text) => text.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    pub name: String,
    pub value: Value,
    pub unit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotLog {
    pub bot: String,
    pub tail: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub name: String,
    pub title: String,
    pub status: Status,
    pub duration_ms: u64,
    pub steps: Vec<String>,
    pub measurements: Vec<Measurement>,
    pub failure: Option<String>,
    pub bot_logs: Vec<BotLog>,
}

/// Collects what a scenario does and measures while it runs.
pub struct Recorder {
    name: String,
    title: String,
    started: Instant,
    steps: Vec<String>,
    measurements: Vec<Measurement>,
}

impl Recorder {
    pub fn new(name: &str, title: &str) -> Self {
        Self {
            name: name.to_owned(),
            title: title.to_owned(),
            started: Instant::now(),
            steps: Vec::new(),
            measurements: Vec::new(),
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn step(&mut self, text: impl Into<String>) {
        let text = text.into();
        tracing::info!(scenario = %self.name, "{text}");
        self.steps.push(format!(
            "[+{:.1}s] {text}",
            self.started.elapsed().as_secs_f64()
        ));
    }

    pub fn number(&mut self, name: &str, value: f64, unit: &str) {
        self.push(name, Value::Number(value), unit);
    }

    pub fn text(&mut self, name: &str, value: impl Into<String>) {
        self.push(name, Value::Text(value.into()), "");
    }

    pub fn flag(&mut self, name: &str, value: bool) {
        self.push(name, Value::Bool(value), "");
    }

    fn push(&mut self, name: &str, value: Value, unit: &str) {
        self.measurements.push(Measurement {
            name: name.to_owned(),
            value,
            unit: unit.to_owned(),
        });
    }

    pub fn finish(self, outcome: anyhow::Result<()>, bot_logs: Vec<BotLog>) -> ScenarioResult {
        let duration_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let (status, failure, bot_logs) = match outcome {
            Ok(()) => (Status::Pass, None, Vec::new()),
            Err(err) => (Status::Fail, Some(format!("{err:#}")), bot_logs),
        };
        ScenarioResult {
            name: self.name,
            title: self.title,
            status,
            duration_ms,
            steps: self.steps,
            measurements: self.measurements,
            failure,
            bot_logs,
        }
    }

    pub fn skipped(self, reason: &str) -> ScenarioResult {
        ScenarioResult {
            name: self.name,
            title: self.title,
            status: Status::Skipped,
            duration_ms: 0,
            steps: vec![reason.to_owned()],
            measurements: self.measurements,
            failure: None,
            bot_logs: Vec::new(),
        }
    }
}

/// Last `count` lines of a log text.
pub fn tail_lines(text: &str, count: usize) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(count);
    lines[start..]
        .iter()
        .map(|line| (*line).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    #[test]
    fn passing_scenario_keeps_measurements_and_drops_logs() {
        let mut recorder = Recorder::new("chat", "Chat");
        recorder.step("sent");
        recorder.number("latency", 120.0, "ms");
        recorder.flag("received", true);
        let logs = vec![BotLog {
            bot: "a".into(),
            tail: vec!["x".into()],
        }];
        let result = recorder.finish(Ok(()), logs);
        assert_eq!(result.status, Status::Pass);
        assert!(result.bot_logs.is_empty());
        assert_eq!(result.measurements.len(), 2);
        assert!(result.steps[0].contains("sent"));
        assert!(result.failure.is_none());
    }

    #[test]
    fn failing_scenario_keeps_the_error_chain_and_logs() {
        let recorder = Recorder::new("mute", "Mute");
        let error = anyhow!("inner").context("outer");
        let logs = vec![BotLog {
            bot: "t".into(),
            tail: vec!["line".into()],
        }];
        let result = recorder.finish(Err(error), logs);
        assert_eq!(result.status, Status::Fail);
        assert_eq!(result.failure.as_deref(), Some("outer: inner"));
        assert_eq!(result.bot_logs.len(), 1);
    }

    #[test]
    fn skipped_scenario_records_the_reason() {
        let result = Recorder::new("share", "Share").skipped("no probe");
        assert_eq!(result.status, Status::Skipped);
        assert_eq!(result.steps, vec!["no probe".to_owned()]);
    }

    #[test]
    fn tail_keeps_only_the_last_lines() {
        let text = (1..=50)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let tail = tail_lines(&text, 30);
        assert_eq!(tail.len(), 30);
        assert_eq!(tail[0], "21");
        assert_eq!(tail[29], "50");
        assert_eq!(tail_lines("a\nb", 30), vec!["a", "b"]);
    }

    #[test]
    fn values_display_without_noise() {
        assert_eq!(Value::Number(3.0).display(), "3");
        assert_eq!(Value::Number(0.0212345).display(), "0.0212");
        assert_eq!(Value::Bool(true).display(), "sim");
        assert_eq!(Value::Text("x".into()).display(), "x");
    }
}
