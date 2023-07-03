use crate::config::Config;
use crate::config::CrateName;
use crate::config::PackageConfig;
use crate::crate_index::CrateIndex;
use clap::Parser;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt::Display;

/// Counts of how many packages in the dependency tree use different permissions, how many use no
/// special permissions etc.
pub(crate) struct Summary {
    packages: Vec<PackageSummary>,
}

#[derive(Parser, Debug, Clone)]
pub(crate) struct SummaryOptions {
    /// Print summary by package.
    #[clap(long)]
    by_package: bool,

    /// Print summary by permission.
    #[clap(long)]
    by_permission: bool,

    /// Print counts.
    #[clap(long)]
    counts: bool,

    /// Print all summary kinds. This is the default if no options are specified.
    #[clap(long)]
    full: bool,

    /// Whether to print headers for each summary section. This is forced on if more than one
    /// summary is selected.
    #[clap(long)]
    print_headers: bool,
}

struct PackageSummary {
    name: CrateName,
    permissions: Vec<String>,
}

impl Summary {
    pub(crate) fn new(crate_index: &CrateIndex, config: &Config) -> Self {
        let pkg_configs: HashMap<&CrateName, &PackageConfig> =
            config.packages.iter().map(|(k, v)| (k, v)).collect();
        let mut packages: Vec<PackageSummary> = crate_index
            .package_names()
            .map(|name| {
                let mut permissions = Vec::new();
                for (crate_name, suffix) in [
                    (name, ""),
                    (&CrateName::for_build_script(name.as_ref()), "[build]"),
                ] {
                    if let Some(pkg_config) = pkg_configs.get(crate_name) {
                        if pkg_config.allow_proc_macro {
                            permissions.push(format!("proc_macro{suffix}"));
                        }
                        if pkg_config.allow_unsafe {
                            permissions.push(format!("unsafe{suffix}"));
                        }
                        for api in &pkg_config.allow_apis {
                            permissions.push(format!("{api}{suffix}"));
                        }
                    }
                }
                PackageSummary {
                    name: name.to_owned(),
                    permissions,
                }
            })
            .collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));

        Self { packages }
    }
}

impl Summary {
    pub(crate) fn print(&self, options: &SummaryOptions) {
        let options = options.with_defaults();
        if options.by_package {
            if options.print_headers {
                println!("=== Permissions by package ===");
            }
            self.print_by_crate();
        }
        if options.by_permission {
            if options.print_headers {
                println!("=== Packages by permission ===");
            }
            self.print_by_permission();
        }
        if options.counts {
            if options.print_headers {
                println!("=== Permission counts ===");
            }
            println!("{self}");
        }
    }

    fn print_by_crate(&self) {
        for pkg in &self.packages {
            println!("{}: {}", pkg.name, pkg.permissions.join(", "));
        }
    }

    fn print_by_permission(&self) {
        let mut by_permission: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for pkg in &self.packages {
            for perm in &pkg.permissions {
                by_permission
                    .entry(perm)
                    .or_default()
                    .push(pkg.name.as_ref());
            }
        }
        for (perm, packages) in by_permission {
            println!("{perm}: {}", packages.join(", "));
        }
    }
}

impl SummaryOptions {
    fn with_defaults(&self) -> SummaryOptions {
        let mut updated = self.clone();
        match self.num_selected() {
            0 => {
                updated.full = true;
                updated.print_headers = true;
            }
            1 => {}
            _ => updated.print_headers = true,
        }
        if updated.full {
            updated.by_package = true;
            updated.by_permission = true;
            updated.counts = true;
        }
        updated
    }

    /// Returns the number of output options that are enabled.
    fn num_selected(&self) -> u32 {
        let mut count = 0;
        if self.full {
            return 2;
        }
        if self.by_package {
            count += 1;
        }
        if self.counts {
            count += 1;
        }
        if self.by_permission {
            count += 1;
        }
        count
    }
}

impl Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "num_packages: {}", self.packages.len())?;
        let no_special_permissions = self
            .packages
            .iter()
            .filter(|pkg| pkg.permissions.is_empty())
            .count();
        writeln!(f, "no_special_permissions: {no_special_permissions}")?;
        Ok(())
    }
}
