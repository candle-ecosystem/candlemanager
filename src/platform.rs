use anyhow::{Result, bail};

pub fn target() -> Result<String> {
    let os = match std::env::consts::OS {
        "windows" => "windows",
        "macos" => "macos",
        "linux" => "linux",
        other => bail!("Unsupported operating system: {other}"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => bail!("Unsupported architecture: {other}"),
    };
    let suffix = match (os, cfg!(target_env = "gnu")) {
        ("linux", true) => "-glibc",
        ("linux", false) => "-musl",
        _ => "",
    };
    Ok(format!("{os}-{arch}{suffix}"))
}
