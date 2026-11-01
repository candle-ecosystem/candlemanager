use crate::{paths, products::Product};
use anyhow::{Context, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
pub struct Metadata {
    pub product: String,
    pub version: String,
}

#[derive(Debug)]
pub struct Installation {
    pub product: Product,
    pub version: Version,
    pub path: PathBuf,
}

pub fn discover(root: &Path) -> Result<Vec<Installation>> {
    paths::ensure_real_dirs(root)?;
    let mut found = Vec::new();
    let product_entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(error) => return Err(error).with_context(|| format!("Read {}", root.display())),
    };
    for product_entry in product_entries {
        let product_entry = product_entry?;
        if !product_entry.file_type()?.is_dir() {
            continue;
        }
        let name = product_entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Ok(product) = Product::new(name) else {
            continue;
        };
        let dir = product_entry.path();
        paths::ensure_real_dirs(&dir)?;
        let entries = fs::read_dir(&dir).with_context(|| format!("Read {}", dir.display()))?;
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let Some(version) = name.to_str().and_then(|s| Version::parse(s).ok()) else {
                continue;
            };
            if version.to_string() != name.to_string_lossy() || !version.pre.is_empty() {
                continue;
            }
            let path = entry.path();
            let metadata_path = path.join(".candle-install.json");
            let metadata = match fs::symlink_metadata(&metadata_path) {
                Ok(m) if m.file_type().is_file() => m,
                Ok(_) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            if metadata.len() > 4096 {
                continue;
            }
            let info: Metadata = match serde_json::from_slice(&fs::read(metadata_path)?) {
                Ok(info) => info,
                Err(_) => continue,
            };
            if info.product != product.id() || info.version != version.to_string() {
                continue;
            }
            if !fs::symlink_metadata(path.join(product.executable()))
                .is_ok_and(|m| m.file_type().is_file())
            {
                continue;
            }
            found.push(Installation {
                product: product.clone(),
                version,
                path,
            });
        }
    }
    found.sort_by(|a, b| {
        a.product
            .id()
            .cmp(b.product.id())
            .then(b.version.cmp(&a.version))
    });
    Ok(found)
}
