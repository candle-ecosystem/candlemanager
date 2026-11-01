mod cli;
mod github;
mod installations;
mod installer;
mod paths;
mod platform;
mod products;
mod ui;
mod uninstaller;
mod update;
#[cfg(windows)]
mod windows_path;

fn main() {
    ui::configure_colors();
    if let Err(error) = cli::run() {
        ui::error(&format!("{error:#}"));
        std::process::exit(1);
    }
}
