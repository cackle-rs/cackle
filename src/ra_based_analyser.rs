// Copyright 2023 The Cackle Authors
//
// Licensed under the Apache License, Version 2.0 <LICENSE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE or
// https://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use crate::built_in_perms;
use crate::checker::Checker;
use crate::checker::CrateId;
use crate::checker::Usage;
use crate::config::Config;
use crate::sandbox;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::Result;
use ra_ap_base_db::CrateGraph;
use ra_ap_base_db::CrateOrigin;
use ra_ap_base_db::FileId;
use ra_ap_base_db::SourceRoot;
use ra_ap_hir::PathResolution;
use ra_ap_ide::AnalysisHost;
use ra_ap_ide::RootDatabase;
use ra_ap_ide::Semantics;
use ra_ap_ide_db::FxHashMap;
use ra_ap_paths::AbsPath;
use ra_ap_paths::AbsPathBuf;
use ra_ap_project_model::CargoConfig;
use ra_ap_project_model::ProjectManifest;
use ra_ap_project_model::ProjectWorkspace;
use ra_ap_syntax::ast;
use ra_ap_syntax::AstNode;
use ra_ap_syntax::NodeOrToken;
use ra_ap_syntax::SyntaxKind;
use ra_ap_syntax::SyntaxNode;
use ra_ap_vfs::file_set::FileSetConfig;
use std::path::Path;
use std::sync::Arc;

pub(crate) fn analyse_crate(config: &crate::config::Config, crate_root: &Path) -> Result<Checker> {
    let mut loader = Loader::default();
    loader.apply_config(config, crate_root)?;
    let mut analysis_host = AnalysisHost::default();
    //state.load_crate_root(crate_root, false, &mut analysis_host)?;
    //let full_graph = state.crate_graph.clone();
    loader.load_crate_root(crate_root, true, &mut analysis_host)?;
    let db = analysis_host.raw_database();
    let sema = Semantics::new(db);
    let mut analyser = Analyser {
        checker: Checker::from_config(config),
        foobar: FooBar {
            db,
            sema,
            analysis: analysis_host.analysis(),
            vfs: loader.vfs,
            crate_graph: loader.crate_graph,
        },
    };
    analyser.determine_used_perms();
    Ok(analyser.checker)
}

#[derive(Default)]
struct Loader {
    crate_graph: CrateGraph,
    cargo_config: CargoConfig,
    vfs: ra_ap_vfs::Vfs,
}

struct Analyser<'a> {
    checker: Checker,
    foobar: FooBar<'a>,
}

// TODO: Come up with a name for this once we figure out what it is. Or possibly
// just get rid of Analyzer and rename this to Analyser. Note though that
// checker can't be on this struct because calls to usage_for_node inside
// closures need to borrow this while a call to Checker is in progress.
struct FooBar<'a> {
    db: &'a RootDatabase,
    sema: Semantics<'a, RootDatabase>,
    analysis: ra_ap_ide::Analysis,
    vfs: ra_ap_vfs::Vfs,
    crate_graph: CrateGraph,
}

impl Loader {
    fn apply_config(&mut self, config: &Config, crate_root: &Path) -> Result<()> {
        if let Some(mut sandbox) =
            sandbox::SandboxCommand::from_config(&config.sandbox, crate_root)?
        {
            sandbox.args(&[
                "cargo",
                "check",
                "--quiet",
                "--workspace",
                "--message-format=json",
            ]);
            println!("{}", sandbox.command_line.clone().join(" "));
            self.cargo_config.run_build_script_command = Some(sandbox.command_line);
        }
        Ok(())
    }

    fn load_crate_root(
        &mut self,
        path: &Path,
        run_build_scripts: bool,
        analysis_host: &mut AnalysisHost,
    ) -> Result<()> {
        let (message_sender, message_receiver) = std::sync::mpsc::channel();

        let vfs_source_file = AbsPathBuf::try_from(path.to_owned())
            .map_err(|p| anyhow!("Path is not absolute {}", p.display()))?;

        let mut loader = ra_ap_vfs_notify::NotifyHandle::spawn(Box::new(move |message| {
            let _ = message_sender.send(message);
        }));

        let manifest = ProjectManifest::from_manifest_file(vfs_source_file.join("Cargo.toml"))?;
        let mut workspace = ProjectWorkspace::load(manifest, &self.cargo_config, &|_| {})?;
        if run_build_scripts {
            // TODO: Check if we have permission to run build scripts/proc macros
            // before we do so.
            let build_scripts = workspace.run_build_scripts(&self.cargo_config, &|_| {})?;
            if let Some(error) = build_scripts.error() {
                bail!("Build scripts/proc macros failed to run: {}", error);
            }
            workspace.set_build_scripts(build_scripts);
        }
        let load = workspace
            .to_roots()
            .iter()
            .map(|root| {
                ra_ap_vfs::loader::Entry::Directories(ra_ap_vfs::loader::Directories {
                    extensions: vec!["rs".to_owned()],
                    include: root.include.clone(),
                    exclude: root.exclude.clone(),
                })
            })
            .collect();

        // Note, set_config is what triggers loading and calling the callback that
        // we registered when we created self.loader.
        use ra_ap_vfs::loader::Handle;
        loader.set_config(ra_ap_vfs::loader::Config {
            version: 1,
            load,
            watch: vec![],
        });

        for message in &message_receiver {
            match message {
                ra_ap_vfs::loader::Message::Progress {
                    n_total,
                    n_done,
                    config_version: _,
                } => {
                    if n_total == n_done {
                        break;
                    }
                }
                ra_ap_vfs::loader::Message::Loaded { files } => {
                    for (path, contents) in files {
                        let vfs_path: ra_ap_vfs::VfsPath = path.to_path_buf().into();
                        self.vfs
                            .set_file_contents(vfs_path.clone(), contents.clone());
                    }
                }
            }
        }

        self.crate_graph = workspace.to_crate_graph(
            &mut |_, _| Ok(Vec::new()),
            &mut |path: &AbsPath| self.vfs.file_id(&path.to_path_buf().into()),
            &FxHashMap::default(),
        );

        let mut change = ra_ap_ide::Change::new();
        for changed_file in self.vfs.take_changes() {
            let new_contents = if changed_file.exists() {
                String::from_utf8(self.vfs.file_contents(changed_file.file_id).to_owned())
                    .ok()
                    .map(Arc::new)
            } else {
                None
            };
            change.change_file(changed_file.file_id, new_contents);
        }
        change.set_roots(
            FileSetConfig::default()
                .partition(&self.vfs)
                .into_iter()
                .map(SourceRoot::new_local)
                .collect(),
        );
        change.set_crate_graph(self.crate_graph.clone());
        analysis_host.apply_change(change);
        Ok(())
    }
}

impl<'a> Analyser<'a> {
    fn determine_used_perms(&mut self) {
        for ra_crate_id in self.foobar.crate_graph.crates_in_topological_order() {
            let krate = &self.foobar.crate_graph[ra_crate_id];
            if matches!(krate.origin, CrateOrigin::Lang(_)) {
                continue;
            }
            if let Some(display_name) = &krate.display_name {
                let mut krate_name = display_name.canonical_name().to_owned();
                if krate_name == "build-script-build" {
                    if let Some(true_crate_name) = krate.env.get("CARGO_CRATE_NAME") {
                        krate_name = format!("{true_crate_name}.build");
                    }
                }
                let crate_id = self.checker.crate_id_from_name(&krate_name);
                self.checker.report_crate_used(crate_id);
                let mut file_ids = Vec::new();
                for module in self.foobar.sema.to_module_defs(krate.root_file_id) {
                    self.collect_file_ids(&module, &mut file_ids);
                }
                for file_id in file_ids {
                    let source_file = self.foobar.sema.parse(file_id);
                    self.collect_usage(source_file.syntax(), crate_id);
                }
            }
        }
    }

    fn collect_file_ids(&mut self, module: &ra_ap_hir::Module, file_ids: &mut Vec<FileId>) {
        file_ids.push(
            module
                .definition_source(self.foobar.db)
                .file_id
                .original_file(self.foobar.db),
        );
        for child in module.children(self.foobar.db) {
            self.collect_file_ids(&child, file_ids);
        }
    }

    fn collect_usage(&mut self, node: &SyntaxNode, crate_id: CrateId) {
        if let Some(path) = ast::Path::cast(node.clone()) {
            self.process_path(path, crate_id);
            return;
        }
        if let Some(macro_call) = ast::MacroCall::cast(node.clone()) {
            if let Some(expansion) = self.foobar.sema.expand(&macro_call) {
                self.collect_usage(&expansion, crate_id);
                return;
            }
        }
        if node.kind() == SyntaxKind::ERROR {
            self.checker
                .permission_used(crate_id, &built_in_perms::ERROR, &mut || {
                    self.foobar.usage_for_node(node)
                });
            return;
        }
        if node.kind() == SyntaxKind::USE {
            // Note, we don't recurse into use statements. We care about actual
            // usage, which may be more specific. For example, suppose we have
            // an include on std::process, but an exclude on std::process:exit,
            // then if there's a `use std::process` we wouldn't want to treat
            // that as matching if the only reference in the file is
            // `process:exit`.
            return;
        }
        for node_or_token in node.children_with_tokens() {
            match node_or_token {
                NodeOrToken::Node(node) => {
                    self.collect_usage(&node, crate_id);
                }
                NodeOrToken::Token(token) => {
                    if token.kind() == SyntaxKind::UNSAFE_KW {
                        self.checker.permission_used(
                            crate_id,
                            &built_in_perms::UNSAFE,
                            &mut || self.foobar.usage_for_node(node),
                        );
                    }
                }
            }
        }
    }

    fn process_path(&mut self, path: ast::Path, crate_id: CrateId) {
        if let Some(PathResolution::Def(definition)) = self.foobar.sema.resolve_path(&path) {
            if let Some(module) = definition.module(self.foobar.db) {
                if let Some(display_name) = module.krate().display_name(self.foobar.db) {
                    // Build up the fully qualified name of what the path
                    // referenced, one bit at a time, checking each time if
                    // the name matches an inclusion or exclusion rule.
                    let mut name_parts = vec![display_name.to_string()];
                    name_parts.extend(
                        module
                            .path_to_root(self.foobar.db)
                            .iter()
                            .rev()
                            .filter_map(|p| p.name(self.foobar.db))
                            .map(|n| n.to_string()),
                    );
                    if let Some(def_name) = definition.name(self.foobar.db) {
                        name_parts.push(def_name.to_string());
                    }
                    self.checker.path_used(crate_id, &name_parts, &mut || {
                        self.foobar.usage_for_node(path.syntax())
                    });
                }
            }
        }
    }
}

impl<'a> FooBar<'a> {
    fn usage_for_node(&self, node: &SyntaxNode) -> Usage {
        let range = self.sema.original_range(node);
        let filename = self.vfs.file_path(range.file_id).to_string();
        let line_index = self.analysis.file_line_index(range.file_id).unwrap();
        let line_col = line_index.line_col(range.range.start());
        Usage {
            filename,
            line_number: line_col.line,
        }
    }
}
