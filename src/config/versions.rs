use super::RawConfig;
use super::SandboxKind;
use crate::config_editor::ConfigEditor;
use anyhow::Result;

pub(crate) const MAX_VERSION: i64 = VERSIONS.len() as i64 - 1;

#[derive(Clone)]
pub(crate) struct Version {
    pub(crate) number: i64,

    /// A description of what has changed from the previous version.
    pub(crate) change_notes: &'static str,

    /// A transformation that will be applied to the user's config at runtime if they're using an
    /// earlier version.
    apply_fn: fn(&mut RawConfig),

    /// A transformation that can be applied to edit the user's config in order to preserve the old
    /// behaviour when updating to this version. This should do semantically the same thing as
    /// apply_fn, but editing the config TOML instead of updating the runtime representation.
    update_fn: fn(&mut ConfigEditor) -> Result<()>,
}

pub(crate) const VERSIONS: &[Version] = &[
    Version {
        number: 0,
        change_notes: "",
        apply_fn: |_| {},
        update_fn: |_| Ok(()),
    },
    Version {
        number: 1,
        change_notes: "",
        apply_fn: |_| {},
        update_fn: |_| Ok(()),
    },
    Version {
        number: 2,
        change_notes: "\
            rustc.sandbox.kind now inherits from sandbox.kind. So if you have a default sandbox \
            configured, updating to version 2 or higher will mean that rustc will now be \
            sandboxed.",
        apply_fn: |config| {
            if config.rustc.sandbox.kind.is_none() {
                config.rustc.sandbox.kind = Some(SandboxKind::Disabled);
            }
        },
        update_fn: |editor| {
            let table = editor.table(["rustc", "sandbox"].into_iter())?;
            if !table.contains_key("kind") {
                table.insert("kind", toml_edit::value("Disabled"));
            }
            Ok(())
        },
    },
];

impl Version {
    pub(crate) fn apply(&self, editor: &mut ConfigEditor) -> Result<()> {
        (self.update_fn)(editor)?;
        editor.set_version(self.number)
    }
}

/// Applies whatever changes are necessary in order to bring us up to the latest version while
/// preserving the behaviour of whatever version the user specified. Note, common.version isn't
/// updated, since we need to keep that as what the user has in their config file, otherwise we
/// won't be able to detect that newer versions are available and offer them to the user.
pub(crate) fn apply_runtime_patches(config: &mut RawConfig) {
    for version in &VERSIONS[(config.common.version as usize + 1).clamp(0, VERSIONS.len())..] {
        (version.apply_fn)(config);
    }
}

#[cfg(test)]
mod tests {
    use super::VERSIONS;
    use crate::config_editor::ConfigEditor;
    use indoc::indoc;

    /// Tests that apply_fn and update_fn do semantically the same thing.
    #[test]
    fn test_edit_consistency() {
        let mut toml = indoc! {r#"
            [common]
            version = 1
        "#}
        .to_owned();
        for version in &VERSIONS[2..] {
            let mut editor = ConfigEditor::from_toml_string(&toml).unwrap();
            version.apply(&mut editor).unwrap();
            let edited_toml = editor.to_toml();

            let mut config = crate::config::parse_raw(&toml).unwrap();
            (version.apply_fn)(&mut config);
            let edited_config = crate::config::parse_raw(&edited_toml).unwrap();
            assert_eq!(config.common.version, version.number - 1);
            config.common.version = version.number;
            assert_eq!(config, edited_config);

            toml = edited_toml;
        }
    }
}
