use anyhow::{Context, Result, bail};
use std::{fs, io, path::Path};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
};
use winreg::{
    RegKey, RegValue,
    enums::{HKEY_LOCAL_MACHINE, KEY_READ, KEY_SET_VALUE, REG_EXPAND_SZ, REG_SZ},
};

const ENVIRONMENT: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const MARKER: &str = ".candle-path-managed";

fn environment(access: u32) -> Result<RegKey> {
    RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(ENVIRONMENT, access)
        .context("Open the system environment registry key")
}

pub fn require_admin() -> Result<()> {
    environment(KEY_READ | KEY_SET_VALUE).context(
        "Administrator privileges are required. Run in an elevated administrator terminal.",
    )?;
    Ok(())
}

fn read_path(key: &RegKey) -> Result<RegValue<'_>> {
    let value = key.get_raw_value("Path").context("Read the system PATH")?;
    if value.vtype != REG_SZ && value.vtype != REG_EXPAND_SZ {
        bail!("The system PATH has an unsupported registry type");
    }
    Ok(value)
}

fn decode(value: &RegValue) -> Result<String> {
    if !value.bytes.len().is_multiple_of(2) {
        bail!("Invalid system PATH encoding");
    }
    let words: Vec<u16> = value
        .bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| u16::from_le_bytes(*bytes))
        .collect();
    let end = words
        .iter()
        .position(|word| *word == 0)
        .unwrap_or(words.len());
    if words[end..].iter().any(|word| *word != 0) {
        bail!("Invalid system PATH contents");
    }
    String::from_utf16(&words[..end]).context("Invalid system PATH encoding")
}

fn encode(text: &str, original: &RegValue) -> RegValue<'static> {
    let bytes: Vec<u8> = text
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    RegValue {
        vtype: original.vtype.clone(),
        bytes: bytes.into(),
    }
}

fn normalize(entry: &str) -> String {
    entry
        .trim()
        .trim_matches('"')
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_lowercase()
}

fn expand_environment(entry: &str) -> Result<String> {
    use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
    let input: Vec<u16> = entry.encode_utf16().chain(std::iter::once(0)).collect();

    let needed = unsafe { ExpandEnvironmentStringsW(input.as_ptr(), std::ptr::null_mut(), 0) };
    if needed == 0 {
        return Err(io::Error::last_os_error()).context("Expand a system PATH entry");
    }
    let mut output = vec![0u16; needed as usize];

    let written = unsafe { ExpandEnvironmentStringsW(input.as_ptr(), output.as_mut_ptr(), needed) };
    if written == 0 || written > needed {
        bail!("Could not expand a system PATH entry");
    }
    String::from_utf16(&output[..written as usize - 1]).context("Invalid expanded PATH entry")
}

fn contains(path: &str, bin: &Path) -> Result<bool> {
    let target = normalize(bin.to_str().context("Candle bin path is not Unicode")?);
    for entry in path.split(';') {
        if normalize(&expand_environment(entry)?) == target {
            return Ok(true);
        }
    }
    Ok(false)
}

fn notify_environment() {
    let environment: Vec<u16> = "Environment\0".encode_utf16().collect();

    unsafe {
        SendMessageTimeoutW(
            windows_sys::Win32::UI::WindowsAndMessaging::HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            environment.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5000,
            std::ptr::null_mut(),
        );
    }
}

pub fn is_configured(root: &Path) -> Result<bool> {
    let key = environment(KEY_READ)?;
    let value = read_path(&key)?;
    contains(&decode(&value)?, &root.join("bin"))
}

pub fn add(root: &Path) -> Result<bool> {
    let key = environment(KEY_READ | KEY_SET_VALUE)?;
    let original = read_path(&key)?;
    let existing = decode(&original)?;
    let bin = root.join("bin");
    if contains(&existing, &bin)? {
        return Ok(false);
    }
    let entry = bin.to_str().context("Candle bin path is not Unicode")?;
    if entry.contains(';') {
        bail!("Candle bin path contains a PATH separator");
    }
    let marker = root.join(MARKER);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    let marker_file = options
        .open(&marker)
        .context("Create PATH ownership marker")?;
    drop(marker_file);
    let new_path = if existing.is_empty() {
        entry.to_owned()
    } else {
        format!("{existing};{entry}")
    };
    if let Err(error) = key.set_raw_value("Path", &encode(&new_path, &original)) {
        let _ = fs::remove_file(&marker);
        return Err(error).context("Update system PATH");
    }
    notify_environment();
    Ok(true)
}

pub fn remove(root: &Path) -> Result<()> {
    let marker = root.join(MARKER);
    match fs::symlink_metadata(&marker) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("Inspect PATH ownership marker"),
        Ok(metadata) if !metadata.file_type().is_file() => bail!("Invalid PATH ownership marker"),
        Ok(_) => (),
    }
    let key = environment(KEY_READ | KEY_SET_VALUE)?;
    let original = read_path(&key)?;
    let existing = decode(&original)?;
    let bin = root.join("bin");
    let target = normalize(bin.to_str().context("Candle bin path is not Unicode")?);
    let parts: Vec<_> = existing.split(';').collect();
    let matches: Vec<_> = parts
        .iter()
        .enumerate()
        .filter(|(_, part)| normalize(part) == target)
        .map(|(index, _)| index)
        .collect();
    if matches.len() > 1 {
        bail!("Multiple Candle PATH entries found; refusing to remove an ambiguous entry");
    }
    if let Some(index) = matches.first() {
        let updated = parts
            .iter()
            .enumerate()
            .filter(|(i, _)| i != index)
            .map(|(_, part)| *part)
            .collect::<Vec<_>>()
            .join(";");
        key.set_raw_value("Path", &encode(&updated, &original))
            .context("Remove managed PATH entry")?;
        notify_environment();
    }
    if let Err(error) = fs::remove_file(marker) {
        if !matches.is_empty() {
            key.set_raw_value("Path", &original)
                .context("Restore PATH after marker cleanup failed")?;
            notify_environment();
        }
        return Err(error).context("Remove PATH ownership marker");
    }
    Ok(())
}
