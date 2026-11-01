use crate::products::Product;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

pub fn root() -> Result<PathBuf> {
    match std::env::consts::OS {
        "linux" => Ok(PathBuf::from("/opt/candle")),
        "macos" => Ok(PathBuf::from("/Library/Application Support/Candle")),
        "windows" => Ok(PathBuf::from(
            std::env::var_os("ProgramW6432")
                .or_else(|| std::env::var_os("ProgramFiles"))
                .context("Program Files is unavailable")?,
        )
        .join("Candle")),
        other => bail!("Unsupported operating system: {other}"),
    }
}

pub fn require_admin() -> Result<()> {
    #[cfg(unix)]
    {
        let output = std::process::Command::new("id")
            .arg("-u")
            .output()
            .context("Check administrator privileges")?;
        if !output.status.success() || output.stdout.trim_ascii() != b"0" {
            bail!("Administrator privileges are required. Run this command again with sudo.");
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        crate::windows_path::require_admin()
    }
    #[cfg(not(any(unix, windows)))]
    {
        bail!("Unsupported operating system")
    }
}

pub fn ensure_real_dirs(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if !metadata.is_dir() || is_link_or_reparse(&metadata) => {
                bail!("Unsafe directory: {}", current.display())
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => {
                return Err(error).with_context(|| format!("Inspect {}", current.display()));
            }
        }
    }
    Ok(())
}

fn is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

pub fn version_dir(root: &Path, product: &Product, version: &semver::Version) -> PathBuf {
    root.join(product.id()).join(version.to_string())
}

#[cfg(any(unix, windows))]
pub fn active(root: &Path, product: &Product) -> Result<Option<semver::Version>> {
    ensure_real_dirs(&root.join("bin"))?;
    let link = root.join("bin").join(product.executable());
    let target = match std::fs::read_link(&link) {
        Ok(target) => target,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("Inspect active link {}", link.display()));
        }
    };
    let prefix = root.join(product.id());
    let relative = target
        .strip_prefix(&prefix)
        .context("Active link is not managed by Candle Installer")?;
    let mut components = relative.components();
    let version = components
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .and_then(|s| semver::Version::parse(s).ok())
        .context("Invalid active link target")?;
    if components.next().map(|c| c.as_os_str()) != Some(std::ffi::OsStr::new(&product.executable()))
        || components.next().is_some()
    {
        bail!("Invalid active link target");
    }
    Ok(Some(version))
}

#[cfg(not(any(unix, windows)))]
pub fn active(_root: &Path, _product: &Product) -> Result<Option<semver::Version>> {
    Ok(None)
}

#[cfg(unix)]
pub fn is_exposed(root: &Path, external_bin: &Path, product: &Product) -> Result<bool> {
    ensure_real_dirs(external_bin)?;
    let link = external_bin.join(product.executable());
    let metadata = match std::fs::symlink_metadata(&link) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("Inspect {}", link.display())),
    };

    let expected = root.join("bin").join(product.executable());
    if !metadata.file_type().is_symlink() || std::fs::read_link(&link)? != expected {
        bail!(
            "Command already exists and is not managed by Candle Manager: {}",
            link.display()
        );
    }

    Ok(true)
}

#[cfg(windows)]
pub fn is_exposed(root: &Path, _external_bin: &Path, _product: &Product) -> Result<bool> {
    crate::windows_path::is_configured(root)
}

#[cfg(not(any(unix, windows)))]
pub fn is_exposed(_root: &Path, _external_bin: &Path, _product: &Product) -> Result<bool> {
    bail!("Unsupported operating system")
}

#[cfg(unix)]
pub fn activate(
    root: &Path,
    external_bin: &Path,
    product: &Product,
    version: &semver::Version,
) -> Result<()> {
    use std::os::unix::fs::symlink;
    let executable = version_dir(root, product, version).join(product.executable());
    ensure_real_dirs(&version_dir(root, product, version))?;
    if !std::fs::symlink_metadata(&executable)
        .context("Inspect executable")?
        .file_type()
        .is_file()
    {
        bail!("Executable is not a regular file");
    }
    let bin = root.join("bin");
    ensure_real_dirs(&bin)?;
    ensure_real_dirs(external_bin)?;
    std::fs::create_dir_all(&bin)?;
    std::fs::create_dir_all(external_bin)?;
    let central = bin.join(product.executable());
    let external = external_bin.join(product.executable());
    let _ = active(root, product)?;
    let temp = tempfile::Builder::new()
        .prefix(".active-")
        .tempfile_in(&bin)?;
    let temp_path = temp.into_temp_path();
    std::fs::remove_file(&temp_path)?;
    symlink(&executable, &temp_path)?;
    let created_external = match std::fs::symlink_metadata(&external) {
        Ok(_) if std::fs::read_link(&external).ok().as_deref() == Some(central.as_path()) => false,
        Ok(_) => bail!(
            "Command already exists and is not managed by Candle Installer: {}",
            external.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            symlink(&central, &external).context("Create system command link")?;
            true
        }
        Err(e) => return Err(e.into()),
    };
    if let Err(error) = std::fs::rename(&temp_path, &central) {
        if created_external {
            std::fs::remove_file(&external)
                .context("Restore system command after activation failure")?;
        }
        return Err(error).context("Switch active version");
    }
    Ok(())
}

#[cfg(windows)]
pub fn activate(
    root: &Path,
    _external_bin: &Path,
    product: &Product,
    version: &semver::Version,
) -> Result<()> {
    use std::os::windows::fs::symlink_file;
    let executable = version_dir(root, product, version).join(product.executable());
    ensure_real_dirs(&version_dir(root, product, version))?;
    if !std::fs::symlink_metadata(&executable)?
        .file_type()
        .is_file()
    {
        bail!("Executable is not a regular file");
    }
    let bin = root.join("bin");
    ensure_real_dirs(&bin)?;
    std::fs::create_dir_all(&bin)?;
    let central = bin.join(product.executable());
    let _ = active(root, product)?;
    let temporary = tempfile::Builder::new()
        .prefix(".active-")
        .tempfile_in(&bin)?;
    let temporary = temporary.into_temp_path();
    std::fs::remove_file(&temporary)?;
    symlink_file(&executable, &temporary)
        .context("Create active command link; an elevated administrator terminal is required")?;
    let added = crate::windows_path::add(root)?;
    if let Err(error) = std::fs::rename(&temporary, &central) {
        if added {
            crate::windows_path::remove(root).context("Restore PATH after activation failure")?;
        }
        return Err(error).context("Switch active version");
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub fn activate(
    _root: &Path,
    _external_bin: &Path,
    _product: &Product,
    _version: &semver::Version,
) -> Result<()> {
    bail!("Unsupported operating system")
}

#[cfg(unix)]
pub fn deactivate(root: &Path, external_bin: &Path, product: &Product) -> Result<()> {
    ensure_real_dirs(external_bin)?;
    let central = root.join("bin").join(product.executable());
    let external = external_bin.join(product.executable());
    let owned_external = match std::fs::symlink_metadata(&external) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            std::fs::read_link(&external)? == central
        }
        Ok(_) => false,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e).context("Inspect system command link"),
    };
    if active(root, product)?.is_some() {
        std::fs::remove_file(&central)?;
    }
    if owned_external {
        std::fs::remove_file(&external)?;
    }
    Ok(())
}
#[cfg(windows)]
pub fn deactivate(root: &Path, _external_bin: &Path, product: &Product) -> Result<()> {
    use std::os::windows::fs::symlink_file;
    let central = root.join("bin").join(product.executable());
    let previous = active(root, product)?;
    if previous.is_some() {
        std::fs::remove_file(&central)?;
    }
    let cleanup = (|| -> Result<()> {
        let mut any_active = false;
        for entry in std::fs::read_dir(root.join("bin"))? {
            let entry = entry?;
            if !entry.file_type()?.is_symlink() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str().and_then(|name| name.strip_suffix(".exe")) else {
                continue;
            };
            let Ok(other) = Product::new(name) else {
                continue;
            };
            if active(root, &other)?.is_some() {
                any_active = true;
                break;
            }
        }
        if !any_active {
            crate::windows_path::remove(root)?;
        }
        Ok(())
    })();
    if let Err(error) = cleanup {
        if let Some(version) = previous {
            let target = version_dir(root, product, &version).join(product.executable());
            if let Err(restore) = symlink_file(target, &central) {
                bail!(
                    "Could not clean system PATH: {error:#}; could not restore active link: {restore}"
                );
            }
        }
        return Err(error);
    }
    Ok(())
}
#[cfg(not(any(unix, windows)))]
pub fn deactivate(_root: &Path, _external_bin: &Path, _product: &Product) -> Result<()> {
    bail!("Unsupported operating system")
}
