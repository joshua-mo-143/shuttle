use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shuttle_common::constants::API_URL_BETA;
use shuttle_common::{constants::API_URL_DEFAULT, ApiKey};
use tracing::trace;

use crate::args::ProjectArgs;

/// Helper trait for dispatching fs ops for different config files
pub trait ConfigManager: Sized {
    fn directory(&self) -> PathBuf;

    fn file(&self) -> PathBuf;

    fn path(&self) -> PathBuf {
        self.directory().join(self.file())
    }

    fn exists(&self) -> bool {
        self.path().exists()
    }

    fn create<C>(&self) -> Result<()>
    where
        C: Serialize + Default,
    {
        if self.exists() {
            return Ok(());
        }
        let config = C::default();
        self.save(&config)
    }

    fn open<C>(&self) -> Result<C>
    where
        C: for<'de> Deserialize<'de>,
    {
        let path = self.path();
        let config_string = File::open(&path)
            .and_then(|mut f| {
                let mut buf = String::new();
                f.read_to_string(&mut buf)?;
                Ok(buf)
            })
            .with_context(|| anyhow!("Unable to read configuration file: {}", path.display()))?;
        toml::from_str(config_string.as_str())
            .with_context(|| anyhow!("Invalid global configuration file: {}", path.display()))
    }

    fn save<C>(&self, config: &C) -> Result<()>
    where
        C: Serialize,
    {
        let path = self.path();
        std::fs::create_dir_all(path.parent().unwrap())?;

        let mut config_file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)?;

        let config_str = toml::to_string_pretty(config).unwrap();
        config_file.write(config_str.as_bytes()).with_context(|| {
            anyhow!(
                "Could not write the global configuration file: {}",
                path.display()
            )
        })?;
        Ok(())
    }
}

pub struct GlobalConfigManager;

impl ConfigManager for GlobalConfigManager {
    fn directory(&self) -> PathBuf {
        let shuttle_config_dir = dirs::config_dir()
            .ok_or_else(|| {
                anyhow!(
                    "Could not find a configuration directory. Your operating system may not be supported."
                )
            })
            .unwrap();
        shuttle_config_dir.join("shuttle")
    }

    fn file(&self) -> PathBuf {
        PathBuf::from("config.toml")
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ErrorLog {
    raw: String,
    datetime: DateTime<Utc>,
    error_type: String,
    error_code: Option<String>,
    error_message: String,
    file_source: Option<String>,
    file_line: Option<u16>,
    file_col: Option<u16>,
}

impl ErrorLog {
    pub fn try_new(input: Vec<String>) -> Result<Self, anyhow::Error> {
        let timestamp = match input[0].parse::<i64>() {
            Ok(timestamp) => timestamp,
            Err(e) => bail!("Expected i64-compatible string, got {e}"),
        };
        Ok(Self {
            raw: input.join("||"),
            datetime: DateTime::from_timestamp(timestamp, 0).unwrap(),
            error_type: input[1].clone(),
            error_code: if &*input[2] != "none" {
                Some(input[2].clone())
            } else {
                None
            },
            error_message: input[3].clone(),
            file_source: if &*input[4] != "none" {
                Some(input[4].clone())
            } else {
                None
            },
            file_line: if input[5].parse::<i64>().is_ok() {
                Some(input[5].parse().unwrap())
            } else {
                None
            },
            file_col: if input[6].parse::<i64>().is_ok() {
                Some(input[6].parse().unwrap())
            } else {
                None
            },
        })
    }

    pub fn rustc_error(&self) -> Option<String> {
        if let Some(error_code) = self.error_code.clone() {
            let error_code = format!("E{}", error_code);
            let rust_explain = Command::new("rustc")
                .args(["--explain", &error_code])
                .output()
                .unwrap();

            Some(String::from_utf8(rust_explain.stdout).unwrap())
        } else {
            None
        }
    }
}

pub struct ErrorLogManager;

impl ConfigManager for ErrorLogManager {
    fn directory(&self) -> PathBuf {
        let shuttle_config_dir = dirs::config_dir()
                .ok_or_else(|| {
                    anyhow!(
                        "Could not find a configuration directory. Your operating system may not be supported."
                    )
                })
                .unwrap();
        shuttle_config_dir.join("shuttle")
    }

    fn file(&self) -> PathBuf {
        PathBuf::from("logs.txt")
    }
}

impl ErrorLogManager {
    pub fn write(&self, to_add: String) {
        let logfile = self.directory().join(self.file());

        let mut file = OpenOptions::new();
        file.write(true).append(true).create(true);

        let mut file_handle = file.open(logfile).unwrap();

        file_handle.write_all(to_add.as_bytes()).unwrap();
    }

    pub fn write_generic_error(&self, to_add: String) {
        let time = Utc::now().timestamp();
        let logfile = self.directory().join(self.file());

        let mut file = OpenOptions::new();
        file.write(true).append(true).create(true);

        let mut file_handle = file.open(logfile).unwrap();

        let message = format!("{time}||error||none||{to_add}||none||none||none\n");

        file_handle.write_all(message.as_bytes()).unwrap();
    }

    pub fn fetch_last_error_from_file(&self) -> anyhow::Result<Vec<ErrorLog>> {
        let logfile = self.directory().join(self.file());

        if !logfile.is_file() {
            File::create_new(&logfile).map_err(|e| anyhow!("Could not create logfile: {e}"))?;
        }

        let mut buf = String::new();

        File::open(logfile)
            .expect("Couldn't find logfile")
            .read_to_string(&mut buf)
            .unwrap();

        if buf == String::new() {
            return Err(anyhow!("There's currently no logs that can be used with `cargo shuttle explain`. Once you have accumulated some errors from using the CLI, you'll be able to send errors from your last command invocation using `cargo shuttle explain`."));
        }

        let mut logs_by_latest = buf.lines().rev();
        let log_raw = logs_by_latest.next().unwrap().to_string();
        let log_raw_as_vec: Vec<String> = log_raw.split("||").map(ToString::to_string).collect();
        let log = ErrorLog::try_new(log_raw_as_vec).unwrap();
        let mut logs: Vec<ErrorLog> = if log.error_type == *"error" {
            vec![log.clone()]
        } else {
            vec![]
        };

        let timestamp = log.datetime.timestamp();

        for log_raw in logs_by_latest {
            let thing: Vec<String> = log_raw.split("||").map(ToString::to_string).collect();
            if thing[0].parse::<i64>().unwrap() != timestamp {
                break;
            }
            let log = ErrorLog::try_new(thing).unwrap();
            if log.error_type == *"error" {
                logs.push(log);
            }
        }

        if logs.is_empty() {
            return Err(anyhow!("There don't seem to be any errors to send."));
        }

        Ok(logs)
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ExplainStruct {
    logs: Vec<ErrorLog>,
    file_contents: Vec<FileContents>,
}

impl TryFrom<String> for ExplainStruct {
    type Error = anyhow::Error;
    fn try_from(input: String) -> Result<Self, Self::Error> {
        let mut logs_by_latest = input.lines().rev();
        let thing = logs_by_latest.next().unwrap().to_string();
        let thing: Vec<String> = thing.split("||").map(ToString::to_string).collect();
        let thing_as_str = ErrorLog::try_new(thing).unwrap();
        let mut logs: Vec<ErrorLog> = vec![thing_as_str.clone()];

        let timestamp = thing_as_str.datetime.timestamp();

        for log_raw in logs_by_latest {
            let log_raw_as_vec: Vec<String> =
                log_raw.split("||").map(ToString::to_string).collect();
            if log_raw_as_vec[0].parse::<i64>().unwrap() != timestamp {
                break;
            }

            let log = ErrorLog::try_new(log_raw_as_vec)
                .expect("Error while converting String to ErrorLog");

            if log.error_type == *"error" {
                logs.push(log);
            }
        }

        Ok(Self {
            logs,
            file_contents: Vec::new(),
        })
    }
}
impl From<Vec<ErrorLog>> for ExplainStruct {
    fn from(logs: Vec<ErrorLog>) -> Self {
        Self {
            logs,
            file_contents: Vec::new(),
        }
    }
}

impl ExplainStruct {
    pub fn fetch_file_contents_from_errlogs(mut self) -> Self {
        let mut file_contents = self
            .logs
            .clone()
            .into_iter()
            .filter(|x| x.file_source.is_some())
            .map(|x| FileContents::new(x.file_source.expect("to have an existing filesource")))
            .collect::<Vec<FileContents>>();

        file_contents.sort_by(|a, b| a.path.partial_cmp(&b.path).unwrap());
        file_contents.dedup_by_key(|key| key.path.to_owned());

        self.file_contents = file_contents;

        self
    }

    pub fn fetch_only_error_messages(&self) -> Vec<String> {
        self.logs.iter().cloned().map(|x| x.error_message).collect()
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct FileContents {
    path: String,
    contents: String,
}

impl FileContents {
    fn new(path: String) -> Self {
        let contents = std::fs::read_to_string(&path).expect("to read file");

        Self { path, contents }
    }
}

/// An impl of [`ConfigManager`] which is localised to a working directory
pub struct LocalConfigManager {
    working_directory: PathBuf,
    file_name: String,
}

impl LocalConfigManager {
    pub fn new<P: AsRef<Path>>(working_directory: P, file_name: String) -> Self {
        Self {
            working_directory: working_directory.as_ref().to_path_buf(),
            file_name,
        }
    }
}

impl ConfigManager for LocalConfigManager {
    fn directory(&self) -> PathBuf {
        self.working_directory.clone()
    }

    fn file(&self) -> PathBuf {
        PathBuf::from(&self.file_name)
    }
}

/// Global client config for things like API keys.
#[derive(Deserialize, Serialize, Default)]
pub struct GlobalConfig {
    api_key: Option<String>,
    pub api_url: Option<String>,
}

impl GlobalConfig {
    pub fn api_key(&self) -> Option<Result<ApiKey>> {
        self.api_key.as_ref().map(|key| ApiKey::parse(key))
    }

    pub fn set_api_key(&mut self, api_key: ApiKey) -> Option<String> {
        self.api_key.replace(api_key.as_ref().to_string())
    }

    pub fn clear_api_key(&mut self) {
        self.api_key = None;
    }

    pub fn api_url(&self) -> Option<String> {
        self.api_url.clone()
    }
}

/// Project-local config for things like customizing project name
#[derive(Deserialize, Serialize, Default)]
pub struct ProjectConfig {
    pub name: Option<String>,
    pub assets: Option<Vec<String>>,
}

/// A handler for configuration files. The type parameter `M` is the [`ConfigManager`] which handles
/// indirection around file location and serde. The type parameter `C` is the configuration content.
///
/// # Usage
/// ```rust,no_run
/// # use cargo_shuttle::config::{Config, GlobalConfig, GlobalConfigManager};
/// #
/// let mut config = Config::new(GlobalConfigManager);
/// config.open().unwrap();
/// let content: &GlobalConfig = config.as_ref().unwrap();
/// ```
pub struct Config<M, C> {
    pub manager: M,
    config: Option<C>,
}

impl<M, C> Config<M, C>
where
    M: ConfigManager,
    C: Serialize + for<'de> Deserialize<'de>,
{
    /// Creates a new [`Config`] instance, without opening the underlying file
    pub fn new(manager: M) -> Self {
        Self {
            manager,
            config: None,
        }
    }

    /// Opens the underlying config file, as handled by the [`ConfigManager`]
    pub fn open(&mut self) -> Result<()> {
        let config = self.manager.open()?;
        self.config = Some(config);
        Ok(())
    }

    /// Saves the current state of the config to the file managed by the [`ConfigManager`]
    pub fn save(&self) -> Result<()> {
        self.manager.save(self.config.as_ref().unwrap())
    }

    /// Check if the file managed by the [`ConfigManager`] exists
    pub fn exists(&self) -> bool {
        self.manager.exists()
    }

    /// Replace the current config state with a new value.
    ///
    /// Does not persist the change to disk. Use [`Config::save`] for that.
    pub fn replace(&mut self, config: C) -> Option<C> {
        self.config.replace(config)
    }

    /// Get a mut ref to the underlying config state. Returns `None` if the config has not been
    /// opened.
    pub fn as_mut(&mut self) -> Option<&mut C> {
        self.config.as_mut()
    }

    /// Get a ref to the underlying config state. Returns `None` if the config has not been
    /// opened.
    pub fn as_ref(&self) -> Option<&C> {
        self.config.as_ref()
    }

    /// Ask the [`ConfigManager`] to create a default config file at the location it manages.
    ///
    /// If the file already exists, is a no-op.
    pub fn create(&self) -> Result<()>
    where
        C: Default,
    {
        self.manager.create::<C>()
    }
}

/// A wrapper around our two sources of configuration and overrides:
/// - Global config
/// - Local config
pub struct RequestContext {
    global: Config<GlobalConfigManager, GlobalConfig>,
    project: Option<Config<LocalConfigManager, ProjectConfig>>,
    api_url: Option<String>,
}

impl RequestContext {
    /// Create a [`RequestContext`], only loading in the global configuration details.
    pub fn load_global() -> Result<Self> {
        let mut global = Config::new(GlobalConfigManager);
        if !global.exists() {
            global.create()?;
        }
        global
            .open()
            .context("Unable to load global configuration")?;
        Ok(Self {
            global,
            project: None,
            api_url: None,
        })
    }

    /// Load the project configuration at the given `working_directory`
    ///
    /// Ensures that if `--name` is not specified on the command-line, and either the project
    /// file does not exist, or it has not set the `name` key then the `ProjectConfig` instance
    /// has `ProjectConfig.name = Some("crate-name")`.
    pub fn load_local(&mut self, project_args: &ProjectArgs) -> Result<()> {
        // Shuttle.toml
        let project = Self::get_local_config(project_args)?;

        self.project = Some(project);

        Ok(())
    }

    pub fn get_local_config(
        project_args: &ProjectArgs,
    ) -> Result<Config<LocalConfigManager, ProjectConfig>> {
        let workspace_path = project_args
            .workspace_path()
            .unwrap_or(project_args.working_directory.clone());

        trace!("looking for Shuttle.toml in {}", workspace_path.display());
        let local_manager = LocalConfigManager::new(workspace_path, "Shuttle.toml".to_string());
        let mut project = Config::new(local_manager);

        if !project.exists() {
            trace!("no local Shuttle.toml found");
            project.replace(ProjectConfig::default());
        } else {
            trace!("found a local Shuttle.toml");
            project.open()?;
        }

        let config = project.as_mut().unwrap();

        // Project names are preferred in this order:
        // 1. Name given on command line
        // 2. Name from Shuttle.toml file
        // 3. Name from Cargo.toml package if it's a crate
        // 3. Name from the workspace directory if it's a workspace
        match (&project_args.name, &config.name) {
            // Command-line name parameter trumps everything
            (Some(name_from_args), _) => {
                trace!("using command-line project name");
                config.name = Some(name_from_args.clone());
            }
            // If key exists in config then keep it as it is
            (None, Some(_)) => {
                trace!("using Shuttle.toml project name");
            }
            // If name key is not in project config, then we infer from crate name
            (None, None) => {
                trace!("using crate name as project name");
                config.name = Some(project_args.project_name()?);
            }
        };
        Ok(project)
    }

    pub fn set_api_url(&mut self, api_url: Option<String>) {
        self.api_url = api_url;
    }

    pub fn api_url(&self, beta: bool) -> String {
        if let Some(api_url) = self.api_url.clone() {
            api_url
        } else if let Some(api_url) = self.global.as_ref().unwrap().api_url() {
            api_url
        } else if beta {
            API_URL_BETA.to_string()
        } else {
            API_URL_DEFAULT.to_string()
        }
    }

    /// Get the API key from the `SHUTTLE_API_KEY` env variable, or
    /// otherwise from the global configuration. Returns an error if
    /// an API key is not set.
    pub fn api_key(&self) -> Result<ApiKey> {
        let api_key = std::env::var("SHUTTLE_API_KEY");

        if let Ok(key) = api_key {
            ApiKey::parse(&key).context("environment variable SHUTTLE_API_KEY is invalid")
        } else {
            match self.global.as_ref().unwrap().api_key() {
                Some(key) => key,
                None => Err(anyhow!(
                    "Configuration file: `{}`",
                    self.global.manager.path().display()
                )
                .context(anyhow!(
                    "No valid API key found, try logging in first with:\n\tcargo shuttle login"
                ))),
            }
        }
    }

    /// Get the current context working directory
    ///
    /// # Panics
    /// Panics if project configuration has not been loaded.
    pub fn working_directory(&self) -> &Path {
        self.project
            .as_ref()
            .unwrap()
            .manager
            .working_directory
            .as_path()
    }

    /// Set the API key to the global configuration. Will persist the file.
    pub fn set_api_key(&mut self, api_key: ApiKey) -> Result<()> {
        self.global.as_mut().unwrap().set_api_key(api_key);
        self.global.save()
    }

    pub fn clear_api_key(&mut self) -> Result<()> {
        self.global.as_mut().unwrap().clear_api_key();
        self.global.save()
    }
    /// Get the current project name.
    ///
    /// # Panics
    /// Panics if the project configuration has not been loaded.
    pub fn project_name(&self) -> &str {
        self.project
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .name
            .as_ref()
            .unwrap()
            .as_str()
    }

    /// # Panics
    /// Panics if the project configuration has not been loaded.
    pub fn assets(&self) -> Option<&Vec<String>> {
        self.project
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .assets
            .as_ref()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::{args::ProjectArgs, config::RequestContext};

    use super::{Config, ExplainStruct, LocalConfigManager, ProjectConfig};

    fn path_from_workspace_root(path: &str) -> PathBuf {
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("..")
            .join(path)
    }

    fn unwrap_project_name(config: &Config<LocalConfigManager, ProjectConfig>) -> String {
        config.as_ref().unwrap().name.as_ref().unwrap().to_string()
    }

    #[test]
    fn get_local_config_finds_name_in_shuttle_toml() {
        let project_args = ProjectArgs {
            working_directory: path_from_workspace_root("examples/axum/hello-world/"),
            name: None,
        };

        let local_config = RequestContext::get_local_config(&project_args).unwrap();

        assert_eq!(unwrap_project_name(&local_config), "hello-world-axum-app");
    }

    #[test]
    fn get_local_config_finds_name_from_workspace_dir() {
        let project_args = ProjectArgs {
            working_directory: path_from_workspace_root("examples/rocket/workspace/hello-world/"),
            name: None,
        };

        let local_config = RequestContext::get_local_config(&project_args).unwrap();

        assert_eq!(unwrap_project_name(&local_config), "workspace");
    }

    #[test]
    fn setting_name_overrides_name_in_config() {
        let project_args = ProjectArgs {
            working_directory: path_from_workspace_root("examples/axum/hello-world/"),
            name: Some("my-fancy-project-name".to_owned()),
        };

        let local_config = RequestContext::get_local_config(&project_args).unwrap();

        assert_eq!(unwrap_project_name(&local_config), "my-fancy-project-name");
    }

    #[test]
    fn parsing_error_logs() {
        let project_args = ProjectArgs {
            working_directory: path_from_workspace_root("examples/axum/hello-world/src"),
            name: None,
        };

        let wd = project_args
            .working_directory
            .canonicalize()
            .unwrap()
            .display()
            .to_string();

        let explain_logs: ExplainStruct = format!(
            "1724950779||error||none||expected `;`, found `Ok`||{wd}/main.rs||10||60
1724950880||error||none||expected `;`, found `Ok`||{wd}/main.rs||10||60
1724950880||error||none||expected `;`||{wd}/main.rs||12||5
1724950880||warning||none||unused import: `routes`||{wd}/main.rs||1||19
1724950880||error||0601||`main` function not found in crate `hello_world`||{wd}/main.rs||13||2"
        )
        .try_into()
        .expect("Failed to parse string to explain struct");

        let explain_logs = explain_logs.fetch_file_contents_from_errlogs();

        // The logs should only grab the latest errors that all have the same timestamp (first column)
        assert_eq!(explain_logs.logs.len(), 4);

        assert_eq!(explain_logs.file_contents.len(), 1);
    }
}
