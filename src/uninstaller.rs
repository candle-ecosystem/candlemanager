use crate::{
    installations::{self, Installation},
    paths,
};
use anyhow::{Context, Result, bail};
use std::path::Path;

pub fn uninstall(
    root: &Path,
    external_bin: &Path,
    installation: &Installation,
    replacement: Option<&Installation>,
) -> Result<()> {
    paths::require_admin()?;
    uninstall_in(root, external_bin, installation, replacement)
}

pub(crate) fn uninstall_in(
    root: &Path,
    external_bin: &Path,
    installation: &Installation,
    replacement: Option<&Installation>,
) -> Result<()> {
    paths::ensure_real_dirs(root)?;
    let expected = paths::version_dir(root, &installation.product, &installation.version);
    if installation.path != expected {
        bail!("Installation path is outside the expected directory");
    }
    paths::ensure_real_dirs(&expected)?;
    let still_valid = installations::discover(root)?
        .into_iter()
        .any(|item| item.path == expected);
    if !still_valid {
        bail!("Installation is no longer valid");
    }
    let is_active =
        paths::active(root, &installation.product)?.as_ref() == Some(&installation.version);
    if is_active {
        if let Some(other) = replacement {
            if other.product != installation.product
                || other.path == installation.path
                || !installations::discover(root)?
                    .iter()
                    .any(|item| item.path == other.path)
            {
                bail!("Invalid replacement installation");
            }
            paths::activate(root, external_bin, &other.product, &other.version)?;
        } else {
            paths::deactivate(root, external_bin, &installation.product)?;
        }
    }
    if let Err(error) = std::fs::remove_dir_all(&expected) {
        if is_active
            && std::fs::symlink_metadata(expected.join(installation.product.executable()))
                .is_ok_and(|metadata| metadata.file_type().is_file())
            && let Err(restore) = paths::activate(
                root,
                external_bin,
                &installation.product,
                &installation.version,
            )
        {
            bail!(
                "Failed to remove {}: {error}; failed to restore active version: {restore:#}",
                expected.display()
            );
        }
        return Err(error).with_context(|| format!("Remove {}", expected.display()));
    }
    Ok(())
}
