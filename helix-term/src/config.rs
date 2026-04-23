use crate::keymap;
use crate::keymap::{merge_keys, KeyTrie};
use helix_loader::merge_toml_values;
use helix_loader::workspace_trust::{TrustQuery, WorkspaceTrust};
use helix_view::{document::Mode, theme};
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt::Display;
use std::io::Error as IOError;
use std::path::Path;
use std::rc::Rc;
use toml::de::Error as TomlError;

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub theme: Option<theme::Config>,
    pub keys: HashMap<Mode, KeyTrie>,
    pub editor: helix_view::editor::Config,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigRaw {
    pub theme: Option<theme::Config>,
    pub keys: Option<HashMap<Mode, KeyTrie>>,
    pub editor: Option<toml::Value>,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            theme: None,
            keys: keymap::default(),
            editor: helix_view::editor::Config::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum ConfigLoadError {
    BadConfig(TomlError),
    Error(Rc<IOError>),
}

impl Default for ConfigLoadError {
    fn default() -> Self {
        IOError::new(std::io::ErrorKind::NotFound, "place holder").into()
    }
}

impl Display for ConfigLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigLoadError::BadConfig(err) => err.fmt(f),
            ConfigLoadError::Error(err) => err.fmt(f),
        }
    }
}

impl From<IOError> for ConfigLoadError {
    fn from(value: IOError) -> Self {
        Self::Error(Rc::new(value))
    }
}

impl Config {
    fn load(
        configs: impl IntoIterator<Item = Result<String, ConfigLoadError>>,
    ) -> Result<Config, ConfigLoadError> {
        let merged = configs
            .into_iter()
            .map(|res| {
                res.and_then(|file| toml::from_str(&file).map_err(ConfigLoadError::BadConfig))
            })
            .reduce(Config::merge)
            .expect("Should always be at least one config")?;

        let mut keys = keymap::default();
        if let Some(cfg_keys) = merged.keys {
            merge_keys(&mut keys, cfg_keys);
        }

        let editor = match merged.editor {
            None => helix_view::editor::Config::default(),
            Some(toml) => toml.try_into().map_err(ConfigLoadError::BadConfig)?,
        };

        Ok(Config {
            theme: merged.theme,
            keys,
            editor,
        })
    }
    pub fn merge(
        current: Result<ConfigRaw, ConfigLoadError>,
        merge: Result<ConfigRaw, ConfigLoadError>,
    ) -> Result<ConfigRaw, ConfigLoadError> {
        let res = match (current, merge) {
            (Ok(current), Ok(merge)) => {
                let keys = match (current.keys, merge.keys) {
                    (None, None) => None,
                    (Some(val), None) | (None, Some(val)) => Some(val),
                    (Some(mut current), Some(merge)) => {
                        merge_keys(&mut current, merge);
                        Some(current)
                    }
                };

                let editor = match (current.editor, merge.editor) {
                    (None, None) => None,
                    (None, Some(val)) | (Some(val), None) => Some(val),
                    (Some(current), Some(merge)) => Some(merge_toml_values(current, merge, 5)),
                };

                ConfigRaw {
                    theme: merge.theme.or(current.theme),
                    keys,
                    editor,
                }
            }
            // if any configs are invalid return that first
            (_, Err(ConfigLoadError::BadConfig(err)))
            | (Err(ConfigLoadError::BadConfig(err)), _) => {
                return Err(ConfigLoadError::BadConfig(err))
            }
            (Ok(config), Err(_)) | (Err(_), Ok(config)) => config,
            // these are just two io errors return the current one
            (Err(err), Err(_)) => return Err(err),
        };

        Ok(res)
    }

    pub fn load_default() -> Result<Config, ConfigLoadError> {
        let load_file = |path: &Path| Ok(std::fs::read_to_string(path)?);
        let global_config = helix_loader::config_file();
        let extra_configs = helix_loader::extra_config_files();
        let global_files = [&global_config]
            .into_iter()
            .chain(&extra_configs)
            .map(|p| load_file(p));
        let global_parsed = Config::load(global_files.clone())?;

        // We need to build a transient `WorkspaceTrust` just to ask whether the workspace is
        // trusted enough to load its `.helix/config.toml`. The persisted-trust file on disk is the
        // source of truth either way; this transient instance has an empty cache and is dropped
        // after the check.
        let trust = WorkspaceTrust::new((&global_parsed.editor.workspace_trust).into());
        if !trust.query_current(TrustQuery::LocalConfig).is_trusted() {
            return Ok(global_parsed);
        }

        let local_config = load_file(&helix_loader::workspace_config_file());

        // editor.workspace-trust is global/user-scope only. Without this override, a
        // workspace's `.helix/config.toml` could set `level = "insecure"`; once the user trusted
        // *that* workspace, refresh_config would re-load with the override merged in and from
        // then on every subsequent workspace in the session would be implicitly trusted. Pin
        // the gate's own configuration to the global file.
        let mut merged = Config::load(global_files.into_iter().chain([local_config]))?;
        merged.editor.workspace_trust = global_parsed.editor.workspace_trust;
        Ok(merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Config {
        fn load_test(config: &str) -> Config {
            Config::load([Ok(config.to_owned()), Err(ConfigLoadError::default())]).unwrap()
        }
    }

    #[test]
    fn parsing_keymaps_config_file() {
        use crate::keymap;
        use helix_core::hashmap;
        use helix_view::document::Mode;

        let sample_keymaps = r#"
            [keys.insert]
            y = "move_line_down"
            S-C-a = "delete_selection"

            [keys.normal]
            A-F12 = "move_next_word_end"
        "#;

        let mut keys = keymap::default();
        merge_keys(
            &mut keys,
            hashmap! {
                Mode::Insert => keymap!({ "Insert mode"
                    "y" => move_line_down,
                    "S-C-a" => delete_selection,
                }),
                Mode::Normal => keymap!({ "Normal mode"
                    "A-F12" => move_next_word_end,
                }),
            },
        );

        assert_eq!(
            Config::load_test(sample_keymaps),
            Config {
                keys,
                ..Default::default()
            }
        );
    }

    #[test]
    fn keys_resolve_to_correct_defaults() {
        // From serde default
        let default_keys = Config::load_test("").keys;
        assert_eq!(default_keys, keymap::default());

        // From the Default trait
        let default_keys = Config::default().keys;
        assert_eq!(default_keys, keymap::default());
    }
}
