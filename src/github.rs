use crate::products::Product;
use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use semver::Version;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    pub digest: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub draft: bool,
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn version(&self) -> Option<Version> {
        let version =
            Version::parse(self.tag_name.strip_prefix('v').unwrap_or(&self.tag_name)).ok()?;
        (!self.draft && !self.prerelease && version.pre.is_empty()).then_some(version)
    }

    pub fn asset(&self, product: &Product, target: &str) -> Result<&Asset> {
        let name = format!("{}-{}-{target}.zip", product.id(), self.tag_name);
        let mut matching = self.assets.iter().filter(|asset| asset.name == name);
        let asset = matching
            .next()
            .context("No archive is available for this platform")?;
        if matching.next().is_some() {
            bail!("Duplicate matching release assets");
        }
        Ok(asset)
    }
}

pub fn client() -> Result<Client> {
    Client::builder()
        .user_agent(concat!("candlemanager/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .context("Create HTTP client")
}

pub fn fetch(client: &Client, repository: &str) -> Result<Vec<Release>> {
    let mut releases = Vec::new();
    for page in 1..=1000 {
        let url =
            format!("https://api.github.com/repos/{repository}/releases?per_page=100&page={page}");
        let batch: Vec<Release> = client
            .get(url)
            .send()
            .context("Fetch GitHub releases")?
            .error_for_status()
            .context("GitHub rejected the releases request")?
            .json()
            .context("Parse GitHub releases")?;
        let count = batch.len();
        releases.extend(batch);
        if count < 100 {
            return Ok(releases);
        }
    }
    bail!("Too many release pages")
}

pub fn generations(releases: &[Release]) -> BTreeMap<u64, (&Release, Version)> {
    let mut latest = BTreeMap::new();
    for release in releases {
        if let Some(version) = release.version() {
            let entry = latest
                .entry(version.major)
                .or_insert_with(|| (release, version.clone()));
            if version > entry.1 {
                *entry = (release, version);
            }
        }
    }
    latest
}
