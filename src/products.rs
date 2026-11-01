use std::{fmt, io::Read};

use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::Deserialize;

const MANIFEST_URL: &str = "https://candle.nozyx.dev/products.json";
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Product {
    id: String,
}

impl Product {
    pub fn new(id: &str) -> Result<Self> {
        let reserved = [
            "bin", "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6",
            "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8",
            "lpt9",
        ];
        if id.is_empty()
            || id.len() > 64
            || !id.as_bytes()[0].is_ascii_lowercase()
            || !id.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            })
            || reserved.contains(&id)
        {
            bail!("Invalid product name: {id:?}");
        }
        Ok(Self { id: id.to_owned() })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn executable(&self) -> String {
        format!("{}{}", self.id, std::env::consts::EXE_SUFFIX)
    }
}

impl fmt::Display for Product {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.id)
    }
}

#[derive(Debug, Deserialize)]
struct Manifest {
    products: serde_json::Map<String, serde_json::Value>,
}

pub struct Catalog {
    products: Vec<(Product, String)>,
}

impl Catalog {
    pub fn fetch(client: &Client) -> Result<Self> {
        let response = client
            .get(MANIFEST_URL)
            .send()
            .context("Fetch product manifest")?
            .error_for_status()
            .context("Product manifest request failed")?;
        let mut bytes = Vec::new();
        std::io::Read::take(response, MAX_MANIFEST_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_MANIFEST_BYTES {
            bail!("Product manifest is too large");
        }
        Self::parse(&bytes)
    }

    fn parse(bytes: &[u8]) -> Result<Self> {
        let manifest: Manifest = serde_json::from_slice(bytes).context("Parse product manifest")?;
        let mut products = Vec::with_capacity(manifest.products.len());
        for (id, repository) in manifest.products {
            let product = Product::new(&id).context("Invalid product in manifest")?;
            let repository = repository
                .as_str()
                .with_context(|| format!("GitHub repository for {product} must be a string"))?;
            validate_repository(repository)?;
            products.push((product, repository.to_owned()));
        }
        Ok(Self { products })
    }

    pub fn repository(&self, product: &Product) -> Result<&str> {
        self.products
            .iter()
            .find(|(candidate, _)| candidate == product)
            .map(|(_, repository)| repository.as_str())
            .with_context(|| format!("Unknown product: {product}"))
    }

    pub fn products(&self) -> impl Iterator<Item = (&Product, &str)> {
        self.products
            .iter()
            .map(|(product, repository)| (product, repository.as_str()))
    }
}

fn validate_repository(repository: &str) -> Result<()> {
    let Some((owner, name)) = repository.split_once('/') else {
        bail!("Invalid GitHub repository: {repository:?}");
    };
    let valid = |part: &str| {
        !part.is_empty()
            && part.len() <= 100
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    };
    if !valid(owner) || !valid(name) {
        bail!("Invalid GitHub repository: {repository:?}");
    }
    Ok(())
}
