use crate::config::permissions::PermSel;
use crate::config::Config;
use crate::config::PackageConfig;
use crate::crate_index::CrateIndex;
use clap::{Parser, ValueEnum};
use fxhash::FxHashMap;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fmt::Display;

/// Counts of how many packages in the dependency tree use different permissions, how many use no
/// special permissions etc.
#[derive(serde::Serialize)]
pub(crate) struct Summary {
    packages: Vec<PackageSummary>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    /// Print output in a human-readable form.
    Human,
    /// Print output in a machine-readable form with minimal extra context.
    Json,
}

#[derive(Parser, Debug, Clone)]
pub(crate) struct SummaryOptions {
    /// Print summary by package.
    #[clap(long)]
    by_package: bool,

    /// Print summary by permission.
    #[clap(long)]
    by_permission: bool,

    /// Call out proc macros with other permissions.
    #[clap(long)]
    impure_proc_macros: bool,

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

    /// The format of the output
    #[clap(long, value_enum, action)]
    #[clap(default_value_t = OutputFormat::Human)]
    output_format: OutputFormat,
}

#[derive(serde::Serialize)]
struct PackageSummary {
    pub(crate) name: PermSel,
    pub(crate) permissions: Vec<String>,
}

impl PackageSummary {
    fn is_proc_macro_with_other_permissions(&self) -> bool {
        self.permissions.iter().any(|p| p.starts_with("proc_macro"))
            && self
                .permissions
                .iter()
                .any(|p| !p.starts_with("proc_macro") && !p.ends_with("[build]"))
    }
}

impl Summary {
    pub(crate) fn new(crate_index: &CrateIndex, config: &Config) -> Self {
        let pkg_configs: FxHashMap<&PermSel, &PackageConfig> =
            config.permissions.packages.iter().collect();
        let mut packages: Vec<PackageSummary> = crate_index
            .package_ids()
            .map(|pkg_id| {
                let mut permissions = Vec::new();
                let pkg_name = PermSel::for_primary(pkg_id.name_str());
                let build_script_name = PermSel::for_build_script(pkg_id.name_str());
                for (crate_name, suffix) in [(&pkg_name, ""), (&build_script_name, "[build]")] {
                    if let Some(pkg_config) = pkg_configs.get(&crate_name) {
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
                    name: pkg_name,
                    permissions,
                }
            })
            .collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));

        Self { packages }
    }

    pub(crate) fn print(&self, options: &SummaryOptions) {
        let options = options.with_defaults();
        let mut json_map = HashMap::new();

        if options.by_package {
            if options.output_format == OutputFormat::Human {
                if options.print_headers {
                    println!("=== Permissions by package ===");
                }
                self.print_by_crate();
            } else {
                self.json_print_by_crate(&mut json_map);
            }
        }
        if options.by_permission {
            if options.output_format == OutputFormat::Human {
                if options.print_headers {
                    println!("=== Packages by permission ===");
                }
                self.print_by_permission();
            } else {
                self.json_print_by_permission(&mut json_map);
            }
        }
        if options.impure_proc_macros {
            if options.output_format == OutputFormat::Human {
                if options.print_headers {
                    println!("=== Proc macros with other permissions ===");
                }
                self.print_impure_proc_macros();
            } else {
                self.json_print_impure_proc_macros(&mut json_map);
            }
        }
        if options.counts {
            if options.output_format == OutputFormat::Human {
                if options.print_headers {
                    println!("=== Permission counts ===");
                }
                println!("{self}");
            } else {
                self.json_print_count(&mut json_map);
            }
        }

        if !json_map.is_empty() {
            println!("{}", serde_json::to_string_pretty(&json_map).unwrap());
        }
    }

    fn print_by_crate(&self) {
        for pkg in &self.packages {
            println!("{}: {}", pkg.name, pkg.permissions.join(", "));
        }
    }

    fn json_print_by_crate(&self, json_map: &mut HashMap<&str, Value>) {
        let mut map = HashMap::new();
        for pkg in &self.packages {
            map.insert(&pkg.name.package_name, &pkg.permissions);
        }
        json_map.insert(
            "permissions_by_package",
            serde_json::to_value(&map).unwrap(),
        );
    }

    fn print_impure_proc_macros(&self) {
        for pkg in &self.packages {
            if pkg.is_proc_macro_with_other_permissions() {
                println!("{}: {}", pkg.name, pkg.permissions.join(", "));
            }
        }
    }

    fn json_print_impure_proc_macros(&self, json_map: &mut HashMap<&str, Value>) {
        let mut map = HashMap::new();
        for pkg in &self.packages {
            if pkg.is_proc_macro_with_other_permissions() {
                map.insert(&pkg.name.package_name, &pkg.permissions);
            }
        }
        json_map.insert("impure_proc_macros", serde_json::to_value(&map).unwrap());
    }

    fn print_by_permission(&self) {
        let mut by_permission: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for pkg in &self.packages {
            for perm in &pkg.permissions {
                by_permission
                    .entry(perm)
                    .or_default()
                    .push(pkg.name.to_string());
            }
        }
        for (perm, packages) in by_permission {
            println!("{perm}: {}", packages.join(", "));
        }
    }

    fn json_print_by_permission(&self, json_map: &mut HashMap<&str, Value>) {
        let mut by_permission: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for pkg in &self.packages {
            for perm in &pkg.permissions {
                by_permission
                    .entry(perm)
                    .or_default()
                    .push(pkg.name.to_string());
            }
        }
        json_map.insert(
            "impure_proc_macros",
            serde_json::to_value(&by_permission).unwrap(),
        );
    }

    fn json_print_count(&self, json_map: &mut HashMap<&str, Value>) {
        let mut map = HashMap::new();
        for pkg in &self.packages {
            map.insert(&pkg.name.package_name, &pkg.permissions);
        }
        json_map.insert("permission_count", serde_json::to_value(&map).unwrap());
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
            updated.impure_proc_macros = true;
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
        if self.impure_proc_macros {
            count += 1;
        }
        count
    }
}

impl Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "num_packages: {}", self.packages.len())?;
        writeln!(
            f,
            "no_special_permissions: {}",
            self.packages
                .iter()
                .filter(|pkg| pkg.permissions.is_empty())
                .count()
        )?;
        writeln!(
            f,
            "proc_macros_with_other_permissions: {}",
            self.packages
                .iter()
                .filter(|p| p.is_proc_macro_with_other_permissions())
                .count()
        )?;
        Ok(())
    }
}
