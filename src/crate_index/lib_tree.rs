use super::PackageId;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use cargo_metadata::semver::Version;
use fxhash::FxHashMap;
use fxhash::FxHashSet;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

/// Information about what packages depend on what other packages with the current configuration.
/// i.e. it excludes dependencies that are not currently enabled.
#[derive(Default, Debug)]
pub(super) struct LibTree {
    /// Map from the lib-name to the package ID that provides it. e.g. the lib name might be
    /// "futures_util" and the package might be "futures-util" version 0.3.38. Note the hyphen vs
    /// underscore.
    pub(super) lib_name_to_pkg_id: FxHashMap<Arc<str>, PackageId>,
    pub(super) pkg_transitive_deps: FxHashMap<PackageId, FxHashSet<Arc<str>>>,
}

impl LibTree {
    pub(super) fn from_workspace(
        dir: &Path,
        pkg_name_to_ids: &FxHashMap<Arc<str>, Vec<PackageId>>,
    ) -> Result<Self> {
        let builder = LibTreeBuilder {
            stack: Vec::new(),
            tree: LibTree::default(),
            pkg_name_to_ids,
        };
        builder.build(dir)
    }
}

struct LibTreeBuilder<'a> {
    stack: Vec<StackEntry>,
    tree: LibTree,
    pkg_name_to_ids: &'a FxHashMap<Arc<str>, Vec<PackageId>>,
}

impl LibTreeBuilder<'_> {
    fn build(mut self, dir: &Path) -> Result<LibTree> {
        let output = Command::new("cargo")
            .current_dir(dir)
            .arg("tree")
            .args(["--edges", "normal,no-proc-macro"])
            .args(["--prefix", "depth"])
            .args(["--format", " {lib} {p}"])
            .output()
            .context("Failed to run cargo tree")?;

        let stdout = std::str::from_utf8(&output.stdout)
            .context("Got non-utf-8 output from `cargo tree`")?;
        for line in stdout.lines() {
            self.process_line(line)?;
        }
        self.pop_to_level(0);
        Ok(self.tree)
    }

    fn process_line(&mut self, line: &str) -> Result<()> {
        let mut parts = line.split(' ');
        let (Some(level), Some(lib_name), Some(pkg_name), Some(version_str)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Ok(());
        };
        let level = level
            .parse::<usize>()
            .context("Invalid depth in `cargo tree` output")?
            + 1;
        let Some(version_str) = version_str.strip_prefix('v') else {
            bail!("Version string `{version_str}` from `cargo tree` doesn't start with v");
        };
        let version = Version::parse(version_str)
            .context("Failed to parse package version from `cargo tree`")?;
        let packages_with_name = self.pkg_name_to_ids.get(pkg_name).with_context(|| {
            format!(
                "`cargo tree` output contained package `{}` not in `cargo metadata` output",
                pkg_name
            )
        })?;
        let package_id = packages_with_name
            .iter()
            .find(|id| id.version == version)
            .ok_or_else(|| {
                anyhow!(
                    "`cargo tree` listed `{pkg_name}` version `{version_str}` not in \
                        `cargo metadata` output"
                )
            })?;
        let lib_name: Arc<str> = if lib_name.is_empty() {
            // Bin packages don't have a lib name, so we just produce one ourselves from the package
            // name.
            Arc::from(pkg_name.replace('-', "_"))
        } else {
            Arc::from(lib_name)
        };
        self.tree
            .lib_name_to_pkg_id
            .insert(lib_name.clone(), package_id.clone());
        if level - 1 <= self.stack.len() {
            self.pop_to_level(level - 1);
        }
        // If we're already encountered this package before, then add all of its transitive
        // dependencies to the deps of all the packages that depend on it (up the stack).
        if let Some(deps) = self.tree.pkg_transitive_deps.get(package_id) {
            for entry in &mut self.stack {
                entry.deps.extend(deps.iter().cloned());
            }
        }
        // Add the current package as a dependency of all the packages on the stack.
        for entry in &mut self.stack {
            entry.deps.insert(lib_name.clone());
        }
        if self.stack.len() < level {
            self.stack.push(StackEntry {
                package_id: package_id.clone(),
                deps: Default::default(),
            });
        }
        Ok(())
    }

    fn pop_to_level(&mut self, level: usize) {
        while self.stack.len() > level {
            let entry = self.stack.pop().unwrap();
            self.tree
                .pkg_transitive_deps
                .entry(entry.package_id)
                .or_insert(entry.deps);
        }
    }
}

#[derive(Debug)]
struct StackEntry {
    package_id: PackageId,
    deps: FxHashSet<Arc<str>>,
}
