use std::{
    io::{self, IsTerminal},
    time::Duration,
};

use anyhow::Result;
use console::style;
use indicatif::{ProgressBar, ProgressStyle};

use crate::installer::Progress;

pub fn configure_colors() {
    if std::env::var_os("NO_COLOR").is_some() {
        console::set_colors_enabled(false);
        console::set_colors_enabled_stderr(false);
    }
}

pub fn heading(title: &str) {
    eprintln!("\n{}\n", style(title).for_stderr().yellow().bold());
}

pub fn detail(label: &str, value: impl std::fmt::Display) {
    eprintln!(
        "  {} {value}",
        style(format!("{label}:")).for_stderr().dim()
    );
}

pub fn success(message: &str) {
    eprintln!("\n{} {message}", style("✓").for_stderr().green());
}

pub fn note(message: &str) {
    eprintln!("  {} {message}", style("·").for_stderr().dim());
}

pub fn error(message: &str) {
    eprintln!("\n{} {message}", style("✗").for_stderr().red().bold());
}

pub fn stdout_is_terminal() -> bool {
    io::stdout().is_terminal()
}

pub fn version_item(product: &str, version: &str, active: bool) {
    if active {
        println!(
            "  {} {} {version} {}",
            style("-").dim(),
            style(product).yellow().bold(),
            style("active").green()
        );
    } else {
        println!(
            "  {} {} {version}",
            style("-").dim(),
            style(product).yellow().bold()
        );
    }
}

pub fn catalog_item(product: &str, repository: &str) {
    println!(
        "  {} {} {}",
        style("-").dim(),
        style(product).yellow().bold(),
        style(repository).dim()
    );
}

pub fn step<T>(label: &str, action: impl FnOnce() -> Result<T>) -> Result<T> {
    let step = Step::start(label);
    let result = action();
    if result.is_ok() {
        step.finish();
    }
    result
}

struct Step {
    label: String,
    bar: Option<ProgressBar>,
    finished: bool,
}

impl Step {
    fn start(label: &str) -> Self {
        let bar = if io::stderr().is_terminal() {
            let bar = ProgressBar::new_spinner();
            bar.set_message(format!("{} {label}", style("→").for_stderr().yellow()));
            bar.enable_steady_tick(Duration::from_millis(100));
            Some(bar)
        } else {
            eprintln!("  · {label}...");
            None
        };
        Self {
            label: label.to_owned(),
            bar,
            finished: false,
        }
    }

    fn set_download_progress(&self, downloaded: u64, total: Option<u64>) {
        let Some(bar) = &self.bar else {
            return;
        };
        if let Some(total) = total.filter(|total| *total > 0) {
            let template = "{msg}  {bytes}/{total_bytes} ({percent}%)";
            if let Ok(style) = ProgressStyle::with_template(template) {
                bar.set_style(style);
            }
            bar.set_length(total);
            bar.set_position(downloaded.min(total));
        }
    }

    fn finish(mut self) {
        if let Some(bar) = &self.bar {
            bar.finish_and_clear();
        }
        eprintln!("  {} {}", style("✓").for_stderr().green(), self.label);
        self.finished = true;
    }
}

impl Drop for Step {
    fn drop(&mut self) {
        if !self.finished
            && let Some(bar) = &self.bar
        {
            bar.finish_and_clear();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InstallStage {
    Preparing,
    Downloading,
    Verifying,
    Extracting,
    Installing,
}

impl InstallStage {
    fn label(self) -> &'static str {
        match self {
            Self::Preparing => "Prepare download and checksum",
            Self::Downloading => "Download archive",
            Self::Verifying => "Verify SHA-256",
            Self::Extracting => "Extract archive",
            Self::Installing => "Install files",
        }
    }
}

pub struct InstallProgress {
    stage: Option<InstallStage>,
    step: Option<Step>,
}

impl InstallProgress {
    pub fn new() -> Self {
        Self {
            stage: None,
            step: None,
        }
    }

    pub fn update(&mut self, progress: Progress) {
        let stage = match progress {
            Progress::Preparing => InstallStage::Preparing,
            Progress::Downloading { .. } => InstallStage::Downloading,
            Progress::Verifying => InstallStage::Verifying,
            Progress::Extracting => InstallStage::Extracting,
            Progress::Installing => InstallStage::Installing,
        };
        if self.stage != Some(stage) {
            if let Some(step) = self.step.take() {
                step.finish();
            }
            self.step = Some(Step::start(stage.label()));
            self.stage = Some(stage);
        }
        if let Progress::Downloading { downloaded, total } = progress
            && let Some(step) = &self.step
        {
            step.set_download_progress(downloaded, total);
        }
    }

    pub fn finish(mut self) {
        if let Some(step) = self.step.take() {
            step.finish();
        }
    }
}
