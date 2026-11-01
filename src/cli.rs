use std::{
    io::{self, IsTerminal, Write},
    path::Path,
    str::FromStr,
};

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use semver::Version;

use crate::{
    github,
    installations::{self, Installation},
    installer, paths, platform,
    products::{Catalog, Product},
    ui, uninstaller, update,
};

#[derive(Debug, Parser)]
#[command(
    name = "Candle Manager",
    bin_name = "candlemanager",
    about = concat!("Candle Ecosystem Manager v", env!("CARGO_PKG_VERSION")),
    version = env!("CARGO_PKG_VERSION")
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Install the latest stable version, optionally within an ecosystem generation.
    Install {
        product: String,
        /// Ecosystem generation (e.g. 1), or an exact version (e.g. 1.2.3).
        ecosystem_version: Option<VersionSelector>,
    },
    /// Check for updates to installed products and ecosystem generations.
    Update {
        /// Limit the check to one installed product.
        product: Option<String>,
        /// Limit the check to one ecosystem generation (e.g. 1).
        #[arg(requires = "product")]
        ecosystem_version: Option<u64>,
    },
    /// Remove the newest installed version of a generation, or an exact version.
    Remove {
        product: String,
        ecosystem_version: VersionSelector,
    },
    /// Activate the newest installed version of a generation, or an exact version.
    Activate {
        product: String,
        ecosystem_version: VersionSelector,
    },
    /// Deactivate the active version if it belongs to the specified generation.
    Deactivate {
        product: String,
        ecosystem_version: VersionSelector,
    },
    /// List installed products and versions.
    List { product: Option<String> },
    /// List available products, or the latest stable release of each generation.
    Available { product: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum VersionSelector {
    Generation(u64),
    Exact(Version),
}

impl VersionSelector {
    fn matches(&self, version: &Version) -> bool {
        match self {
            Self::Generation(generation) => version.major == *generation,
            Self::Exact(exact) => version == exact,
        }
    }
}

impl FromStr for VersionSelector {
    type Err = String;

    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        if let Ok(generation) = input.parse::<u64>() {
            return Ok(Self::Generation(generation));
        }
        let version = Version::parse(input.strip_prefix('v').unwrap_or(input))
            .map_err(|_| "expected a generation number or a complete SemVer version".to_owned())?;
        if !version.pre.is_empty() {
            return Err("prerelease versions are not supported".to_owned());
        }
        Ok(Self::Exact(version))
    }
}

pub fn run() -> Result<()> {
    let args = Cli::parse();
    let Some(command) = args.command else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };

    match command {
        Command::Install {
            product,
            ecosystem_version,
        } => install(&Product::new(&product)?, ecosystem_version.as_ref()),
        Command::Update {
            product,
            ecosystem_version,
        } => update::run(product.as_deref(), ecosystem_version),
        Command::Remove {
            product,
            ecosystem_version,
        } => remove(&Product::new(&product)?, &ecosystem_version),
        Command::Activate {
            product,
            ecosystem_version,
        } => activate(&Product::new(&product)?, &ecosystem_version),
        Command::Deactivate {
            product,
            ecosystem_version,
        } => deactivate(&Product::new(&product)?, &ecosystem_version),
        Command::List { product } => list(product.as_deref()),
        Command::Available { product } => available(product.as_deref()),
    }
}

fn resolve_release<'a>(
    releases: &'a [github::Release],
    selector: Option<&VersionSelector>,
) -> Result<(&'a github::Release, Version)> {
    releases
        .iter()
        .filter_map(|release| release.version().map(|version| (release, version)))
        .filter(|(_, version)| selector.is_none_or(|selector| selector.matches(version)))
        .max_by(|left, right| left.1.cmp(&right.1))
        .context("No matching stable release was found")
}

fn select_installation<'a>(
    items: &'a [Installation],
    product: &Product,
    selector: &VersionSelector,
) -> Result<&'a Installation> {
    items
        .iter()
        .filter(|item| &item.product == product && selector.matches(&item.version))
        .max_by(|left, right| left.version.cmp(&right.version))
        .with_context(|| format!("No matching installation found for {product}"))
}

fn same_generation<'a>(
    items: &'a [Installation],
    product: &Product,
    version: &Version,
) -> Vec<&'a Installation> {
    items
        .iter()
        .filter(|item| {
            &item.product == product
                && item.version.major == version.major
                && item.version != *version
        })
        .collect()
}

fn parse_replacement_answer(answer: &str) -> Option<bool> {
    match answer.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "" | "n" | "no" => Some(false),
        _ => None,
    }
}

fn confirm_replacement(
    product: &Product,
    version: &Version,
    existing: &[&Installation],
) -> Result<bool> {
    if existing.is_empty() {
        return Ok(true);
    }

    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        bail!(
            "Replacing an installed version requires an interactive terminal; no changes were made"
        );
    }

    let versions = existing
        .iter()
        .map(|item| generation_label(&item.version))
        .collect::<Vec<_>>()
        .join(", ");
    ui::note(&format!(
        "{product} already has these versions installed: {versions}."
    ));
    let target = product_version_label(product, version);
    ui::note(&format!(
        "Replacing them will remove the listed version(s) after {target} is installed."
    ));
    loop {
        eprint!("  ? Replace the existing version(s) with {target}? [y/N] ");
        io::stderr().flush()?;
        let mut answer = String::new();
        if io::stdin().read_line(&mut answer)? == 0 {
            return Ok(false);
        }
        if let Some(answer) = parse_replacement_answer(&answer) {
            return Ok(answer);
        }
        ui::note("Please answer yes or no.");
    }
}

fn replacement_needs_activation(previous: Option<&Version>, existing: &[&Installation]) -> bool {
    previous.is_none_or(|active| existing.iter().any(|item| &item.version == active))
}

fn remove_previous_versions(
    existing: &[&Installation],
    mut remove: impl FnMut(&Installation) -> Result<()>,
) -> Result<()> {
    for item in existing {
        remove(item).with_context(|| {
            format!(
                "Failed to remove previous {}",
                product_version_label(&item.product, &item.version)
            )
        })?;
    }
    Ok(())
}

fn install(product: &Product, selector: Option<&VersionSelector>) -> Result<()> {
    ui::heading(&format!("Install {product}"));

    let client = github::client()?;
    let catalog = ui::step("Load the product catalog", || Catalog::fetch(&client))?;
    let repository = catalog.repository(product)?;
    let releases = ui::step("Find available releases", || {
        github::fetch(&client, repository)
    })?;
    let (release, version) = resolve_release(&releases, selector)?;
    let root = paths::root()?;
    install_release(&client, &root, product, release, &version)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstallOutcome {
    Installed,
    Cancelled,
}

pub(crate) fn install_release(
    client: &reqwest::blocking::Client,
    root: &Path,
    product: &Product,
    release: &github::Release,
    version: &Version,
) -> Result<InstallOutcome> {
    let asset = release.asset(product, &platform::target()?)?;
    let destination = paths::version_dir(root, product, version);

    let installations = installations::discover(root)?;
    let existing = same_generation(&installations, product, version);
    let previous = paths::active(root, product)?;
    ui::detail("Version", generation_label(version));
    ui::detail("Archive", &asset.name);
    ui::detail("Destination", destination.display());

    if destination.try_exists()? {
        bail!(
            "{} is already installed at {}",
            product_version_label(product, version),
            destination.display()
        );
    }
    eprintln!();

    if !confirm_replacement(product, version, &existing)? {
        ui::note("Installation cancelled. Existing version was not changed.");
        return Ok(InstallOutcome::Cancelled);
    }

    ui::step("Check administrator privileges", paths::require_admin)?;
    let mut progress = ui::InstallProgress::new();
    installer::install(client, root, product, version, release, asset, |event| {
        progress.update(event);
    })?;
    progress.finish();

    let activate = replacement_needs_activation(previous.as_ref(), &existing);
    if activate {
        let activation = ui::step("Activate the installed version", || {
            paths::activate(root, Path::new("/usr/local/bin"), product, version)
        });
        if let Err(error) = activation {
            let item = Installation {
                product: product.clone(),
                version: version.clone(),
                path: destination,
            };
            if let Err(rollback) =
                uninstaller::uninstall(root, Path::new("/usr/local/bin"), &item, None)
            {
                bail!("Activation failed: {error:#}; installation rollback failed: {rollback:#}");
            }
            return Err(error).context("Activation failed; the new installation was rolled back");
        }
    }

    remove_previous_versions(&existing, |item| {
        if paths::active(root, product)?.as_ref() == Some(&item.version) {
            bail!("The previous version became active; refusing to remove it");
        }
        let label = format!("Remove previous version {}", generation_label(&item.version));
        ui::step(&label, || {
            uninstaller::uninstall(root, Path::new("/usr/local/bin"), item, None)
        })
    })
    .with_context(|| {
        format!(
            "{} was installed, but replacement was not completed; some previous versions may remain",
            product_version_label(product, version)
        )
    })?;

    if activate {
        ui::success(&format!(
            "{} is installed and active.",
            product_version_label(product, version)
        ));
        if cfg!(windows) {
            ui::note("Restart your terminal to pick up the updated system PATH.");
        } else {
            ui::note("Make sure /usr/local/bin is in your PATH.");
        }
    } else {
        ui::success(&format!(
            "{} is installed.",
            product_version_label(product, version)
        ));
        if let Some(current) = previous {
            ui::note(&format!(
                "{} remains active. Run `candlemanager activate {product} {version}` to switch.",
                product_version_label(product, &current)
            ));
        }
    }
    Ok(InstallOutcome::Installed)
}

fn remove(product: &Product, selector: &VersionSelector) -> Result<()> {
    ui::heading(&format!("Remove {product}"));
    let root = paths::root()?;
    let items = ui::step("Find installed versions", || installations::discover(&root))?;
    let selected = select_installation(&items, product, selector)?;
    let active = paths::active(&root, product)?;
    let replacement = if active.as_ref() == Some(&selected.version) {
        items
            .iter()
            .filter(|item| &item.product == product && item.path != selected.path)
            .max_by(|left, right| left.version.cmp(&right.version))
    } else {
        None
    };

    ui::detail("Version", generation_label(&selected.version));
    ui::detail("Location", selected.path.display());
    if let Some(replacement) = replacement {
        ui::detail(
            "Next active version",
            generation_label(&replacement.version),
        );
    }
    eprintln!();

    let removal_step = if active.as_ref() == Some(&selected.version) {
        if replacement.is_some() {
            "Activate the replacement and remove files"
        } else {
            "Deactivate and remove the installation"
        }
    } else {
        "Remove the installation"
    };
    ui::step(removal_step, || {
        uninstaller::uninstall(&root, Path::new("/usr/local/bin"), selected, replacement)
    })?;
    ui::success(&format!(
        "Removed {}.",
        product_version_label(product, &selected.version)
    ));
    if let Some(replacement) = replacement {
        ui::note(&format!(
            "{} is now active.",
            product_version_label(product, &replacement.version)
        ));
    }
    Ok(())
}

fn activate(product: &Product, selector: &VersionSelector) -> Result<()> {
    ui::heading(&format!("Activate {product}"));
    let root = paths::root()?;
    let items = ui::step("Find installed versions", || installations::discover(&root))?;
    let selected = select_installation(&items, product, selector)?;
    ui::detail("Version", generation_label(&selected.version));
    if paths::active(&root, product)?.as_ref() == Some(&selected.version)
        && paths::is_exposed(&root, Path::new("/usr/local/bin"), product)?
    {
        ui::success(&format!(
            "{} is already active.",
            product_version_label(product, &selected.version)
        ));
        return Ok(());
    }
    eprintln!();
    ui::step("Check administrator privileges", paths::require_admin)?;
    ui::step("Activate version", || {
        paths::activate(
            &root,
            Path::new("/usr/local/bin"),
            product,
            &selected.version,
        )
    })?;
    ui::success(&format!(
        "{} is now active.",
        product_version_label(product, &selected.version)
    ));
    Ok(())
}

fn deactivate(product: &Product, selector: &VersionSelector) -> Result<()> {
    ui::heading(&format!("Deactivate {product}"));
    let root = paths::root()?;
    let active = ui::step("Check the active version", || paths::active(&root, product))?;
    let Some(version) = active else {
        bail!("{product} has no active version");
    };
    if !selector.matches(&version) {
        bail!(
            "The active version is {}, which does not match the requested version",
            product_version_label(product, &version)
        );
    }
    ui::detail("Version", generation_label(&version));
    eprintln!();
    ui::step("Check administrator privileges", paths::require_admin)?;
    ui::step("Deactivate version", || {
        paths::deactivate(&root, Path::new("/usr/local/bin"), product)
    })?;
    ui::success(&format!(
        "{} is now inactive.",
        product_version_label(product, &version)
    ));
    Ok(())
}

pub(crate) fn generation_label(version: &Version) -> String {
    format!("{} (v{version})", version.major)
}

pub(crate) fn product_version_label(product: &Product, version: &Version) -> String {
    format!("{product} {}", generation_label(version))
}

fn plain_version_item(product: &Product, version: &Version, active: bool) -> String {
    let marker = if active { " (active)" } else { "" };
    format!("{}{marker}", product_version_label(product, version))
}

fn list(filter: Option<&str>) -> Result<()> {
    let product = filter.map(Product::new).transpose()?;
    let root = paths::root()?;
    let installations = installations::discover(&root)?;
    let interactive = ui::stdout_is_terminal();
    if interactive {
        ui::heading("Installed products");
    }

    let mut found = false;
    for item in installations.iter().filter(|item| {
        product
            .as_ref()
            .is_none_or(|product| product == &item.product)
    }) {
        let active = paths::active(&root, &item.product)?.as_ref() == Some(&item.version);
        if interactive {
            ui::version_item(item.product.id(), &generation_label(&item.version), active);
        } else {
            println!(
                "{}",
                plain_version_item(&item.product, &item.version, active)
            );
        }
        found = true;
    }

    if !found {
        println!("No installations found.");
        if interactive {
            ui::note("Run `candlemanager available` to browse products.");
        }
    }
    Ok(())
}

fn available(filter: Option<&str>) -> Result<()> {
    let product = filter.map(Product::new).transpose()?;
    let interactive = ui::stdout_is_terminal();
    if interactive {
        ui::heading("Available products");
    }

    let client = github::client()?;
    let catalog = ui::step("Load the product catalog", || Catalog::fetch(&client))?;
    match product {
        None => {
            if interactive {
                println!();
            }
            if catalog.products().next().is_none() {
                println!("No products are currently available.");
            }
            for (product, repository) in catalog.products() {
                if interactive {
                    ui::catalog_item(product.id(), repository);
                } else {
                    println!("{product} {repository}");
                }
            }
            if interactive {
                println!();
                ui::note("Run `candlemanager available <product>` to see its versions.");
            }
        }
        Some(product) => {
            let repository = catalog.repository(&product)?;
            let releases = ui::step("Find available releases", || {
                github::fetch(&client, repository)
            })?;
            let generations = github::generations(&releases);
            if interactive {
                println!();
            }
            if generations.is_empty() {
                println!("No stable releases found for {product}.");
            }
            for (_, (_, version)) in generations.iter().rev() {
                if interactive {
                    ui::version_item(product.id(), &generation_label(version), false);
                } else {
                    println!("{}", plain_version_item(&product, version, false));
                }
            }
            if interactive {
                println!();
                ui::note(&format!(
                    "Run `candlemanager install {product} [generation]` to install a version."
                ));
            }
        }
    }
    Ok(())
}
