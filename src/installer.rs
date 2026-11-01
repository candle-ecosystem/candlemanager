use crate::{
    github::{Asset, Release},
    installations::Metadata,
    paths,
    products::Product,
};
use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use semver::Version;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::Path,
};

fn expected_digest(client: &Client, release: &Release, asset: &Asset) -> Result<String> {
    if let Some(digest) = &asset.digest {
        let hash = digest
            .strip_prefix("sha256:")
            .context("Unsupported GitHub asset digest")?;
        validate_hash(hash)?;
        return Ok(hash.to_ascii_lowercase());
    }
    let sums = release
        .assets
        .iter()
        .find(|a| a.name == "SHA256SUMS.txt")
        .context("No trusted SHA-256 checksum is available")?;
    let mut response = client
        .get(&sums.browser_download_url)
        .send()?
        .error_for_status()?;
    let mut text = String::new();
    response
        .by_ref()
        .take(1024 * 1024)
        .read_to_string(&mut text)?;
    let mut matching = text.lines().filter_map(|line| {
        let (hash, name) = line.split_once(char::is_whitespace)?;
        (name.trim().trim_start_matches('*') == asset.name).then_some(hash)
    });
    let hash = matching
        .next()
        .context("Archive missing from checksum file")?;
    if matching.next().is_some() {
        bail!("Duplicate checksum entries");
    }
    validate_hash(hash)?;
    Ok(hash.to_ascii_lowercase())
}

fn validate_hash(hash: &str) -> Result<()> {
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("Invalid SHA-256 checksum");
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub enum Progress {
    Preparing,
    Downloading { downloaded: u64, total: Option<u64> },
    Verifying,
    Extracting,
    Installing,
}

pub fn install(
    client: &Client,
    root: &Path,
    product: &Product,
    version: &Version,
    release: &Release,
    asset: &Asset,
    mut report: impl FnMut(Progress),
) -> Result<()> {
    paths::require_admin()?;
    install_with_progress(client, root, product, version, release, asset, &mut report)
}

fn install_with_progress(
    client: &Client,
    root: &Path,
    product: &Product,
    version: &Version,
    release: &Release,
    asset: &Asset,
    report: &mut impl FnMut(Progress),
) -> Result<()> {
    report(Progress::Preparing);
    paths::ensure_real_dirs(root)?;
    let parent = root.join(product.id());
    paths::ensure_real_dirs(&parent)?;
    fs::create_dir_all(&parent).context("Create product directory")?;
    let destination = paths::version_dir(root, product, version);
    if destination.try_exists()? {
        bail!("This version already exists: {}", destination.display());
    }
    let expected = expected_digest(client, release, asset)?;
    let staging = tempfile::Builder::new()
        .prefix(".install-")
        .tempdir_in(&parent)?;
    let archive_path = staging.path().join("download.zip");
    let mut response = client
        .get(&asset.browser_download_url)
        .send()
        .context("Download archive")?
        .error_for_status()?;
    let content_length = response.content_length().filter(|length| *length > 0);
    report(Progress::Downloading {
        downloaded: 0,
        total: content_length,
    });
    let mut last_report = std::time::Instant::now();
    let mut archive = File::create(&archive_path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut total = 0u64;
    loop {
        let count = response.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > 1024 * 1024 * 1024 {
            bail!("Archive exceeds size limit");
        }
        hasher.update(&buffer[..count]);
        archive.write_all(&buffer[..count])?;
        if last_report.elapsed() >= std::time::Duration::from_millis(100) {
            report(Progress::Downloading {
                downloaded: total,
                total: content_length,
            });
            last_report = std::time::Instant::now();
        }
    }
    report(Progress::Downloading {
        downloaded: total,
        total: content_length,
    });
    report(Progress::Verifying);
    if format!("{:x}", hasher.finalize()) != expected {
        bail!("Archive SHA-256 verification failed");
    }
    drop(archive);
    report(Progress::Extracting);
    let extracted = staging.path().join("files");
    fs::create_dir(&extracted)?;
    extract(&archive_path, &extracted, product)?;
    let metadata = Metadata {
        product: product.id().into(),
        version: version.to_string(),
    };
    fs::write(
        extracted.join(".candle-install.json"),
        serde_json::to_vec_pretty(&metadata)?,
    )?;
    report(Progress::Installing);
    fs::create_dir(&destination).context("Reserve version directory; it may already exist")?;
    let result = (|| -> Result<()> {
        for entry in fs::read_dir(&extracted)? {
            let entry = entry?;
            fs::rename(entry.path(), destination.join(entry.file_name()))?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let cleanup = fs::remove_dir_all(&destination);
        if let Err(cleanup_error) = cleanup {
            bail!("Installation failed: {error:#}; cleanup failed: {cleanup_error}");
        }
        return Err(error).context("Finalize installation");
    }
    Ok(())
}

fn extract(archive_path: &Path, destination: &Path, product: &Product) -> Result<()> {
    let mut archive =
        zip::ZipArchive::new(File::open(archive_path)?).context("Open ZIP archive")?;
    if archive.len() > 100 {
        bail!("Archive contains too many files");
    }
    let mut total = 0u64;
    let mut executable_found = false;
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let name = file.enclosed_name().context("Unsafe archive path")?;
        if name.components().count() != 1 || file.is_dir() {
            bail!("Unexpected archive layout");
        }
        if file
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 != 0 && mode & 0o170000 != 0o100000)
        {
            bail!("Archive contains a non-regular file");
        }
        let filename = name.to_str().context("Non-UTF-8 archive name")?;
        if filename != product.executable() && filename != "LICENSE" && filename != "README.md" {
            bail!("Unexpected archive file: {filename}");
        }
        total = total
            .checked_add(file.size())
            .context("Archive size overflow")?;
        if total > 1024 * 1024 * 1024 {
            bail!("Extracted files exceed size limit");
        }
        let path = destination.join(&name);
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let written = io::copy(&mut file, &mut output)?;
        if written != file.size() {
            bail!("Unexpected extracted file size");
        }
        if filename == product.executable() {
            executable_found = true;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
            }
        }
    }
    if !executable_found {
        bail!("Archive does not contain the expected executable");
    }
    Ok(())
}
