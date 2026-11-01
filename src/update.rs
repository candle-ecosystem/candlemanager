use std::{collections::BTreeMap, path::Path};

use anyhow::{Result, bail};
use reqwest::blocking::Client;
use semver::Version;

use crate::{
    cli::{self, InstallOutcome},
    github::{self, Release},
    installations::{self, Installation},
    paths,
    products::{Catalog, Product},
    ui,
};

type InstalledGenerations = BTreeMap<Product, BTreeMap<u64, Version>>;

fn installed_generations(
    installations: &[Installation],
    product_filter: Option<&Product>,
    generation_filter: Option<u64>,
) -> InstalledGenerations {
    let mut products = InstalledGenerations::new();

    for item in installations {
        if product_filter.is_some_and(|product| product != &item.product)
            || generation_filter.is_some_and(|generation| generation != item.version.major)
        {
            continue;
        }

        let versions = products.entry(item.product.clone()).or_default();
        let latest = versions
            .entry(item.version.major)
            .or_insert_with(|| item.version.clone());
        if item.version > *latest {
            *latest = item.version.clone();
        }
    }

    products
}

fn newer_release<'a>(
    releases: &'a BTreeMap<u64, (&'a Release, Version)>,
    installed: &Version,
) -> Option<(&'a Release, &'a Version)> {
    let (release, version) = releases.get(&installed.major)?;
    (version > installed).then_some((*release, version))
}

pub fn run(product: Option<&str>, generation: Option<u64>) -> Result<()> {
    if product.is_none() && generation.is_some() {
        bail!("An ecosystem generation requires a product");
    }

    let product = product.map(Product::new).transpose()?;
    let root = paths::root()?;
    ui::heading("Check for updates");

    let installations = ui::step("Find installed versions", || installations::discover(&root))?;
    let installed = installed_generations(&installations, product.as_ref(), generation);
    if installed.is_empty() {
        match (product.as_ref(), generation) {
            (Some(product), Some(generation)) => {
                ui::note(&format!(
                    "No {product} installation found for Candle {generation}."
                ));
            }
            (Some(product), None) => ui::note(&format!("No {product} installations found.")),
            _ => ui::note("No installations found."),
        }
        return Ok(());
    }

    let client = github::client()?;
    let catalog = ui::step("Load the product catalog", || Catalog::fetch(&client))?;
    let mut summary = UpdateSummary::default();

    for (product, versions) in installed {
        if let Err(error) =
            check_product(&client, &catalog, &root, &product, &versions, &mut summary)
        {
            ui::error(&format!("Could not check {product}: {error:#}"));
            summary.failed += 1;
        }
    }

    if summary.failed > 0 {
        bail!(
            "{} update check(s) or installation(s) failed",
            summary.failed
        );
    }
    if summary.installed > 0 {
        ui::success(&format!("Installed {} update(s).", summary.installed));
    } else if summary.available > 0 {
        ui::note("No updates were installed.");
    } else {
        ui::success("No newer versions are available.");
    }
    Ok(())
}

#[derive(Default)]
struct UpdateSummary {
    available: usize,
    installed: usize,
    failed: usize,
}

fn check_product(
    client: &Client,
    catalog: &Catalog,
    root: &Path,
    product: &Product,
    installed: &BTreeMap<u64, Version>,
    summary: &mut UpdateSummary,
) -> Result<()> {
    let repository = catalog.repository(product)?;
    let releases = ui::step(&format!("Find releases for {product}"), || {
        github::fetch(client, repository)
    })?;
    let generations = github::generations(&releases);

    for (generation, current) in installed.iter().rev() {
        if !generations.contains_key(generation) {
            ui::error(&format!(
                "No stable release found for {product} generation {generation}; it could not be checked."
            ));
            summary.failed += 1;
            continue;
        }

        let Some((release, latest)) = newer_release(&generations, current) else {
            ui::note(&format!(
                "{}: no newer release.",
                cli::product_version_label(product, current)
            ));
            continue;
        };

        summary.available += 1;
        ui::note(&format!(
            "Update available: {} → {}.",
            cli::product_version_label(product, current),
            cli::generation_label(latest)
        ));

        match cli::install_release(client, root, product, release, latest) {
            Ok(InstallOutcome::Installed) => summary.installed += 1,
            Ok(InstallOutcome::Cancelled) => (),
            Err(error) => {
                ui::error(&format!(
                    "Could not update {product} generation {generation}: {error:#}"
                ));
                summary.failed += 1;
            }
        }
    }

    Ok(())
}
