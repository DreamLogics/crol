//! The Zed extension for CROL (`SPEC.md` 12.7): starts `crol lsp`.

use std::fs;

use zed_extension_api::{self as zed, LanguageServerId, Result, settings::LspSettings};

const REPOSITORY: &str = "DreamLogics/crol";

struct CrolExtension {
    cached_binary: Option<String>,
}

impl CrolExtension {
    /// The `crol` binary: from the user's settings, the worktree's `PATH`,
    /// or the latest GitHub release, downloaded and cached.
    fn binary(&mut self, id: &LanguageServerId, worktree: &zed::Worktree) -> Result<String> {
        let settings = LspSettings::for_worktree("crol", worktree).ok();
        if let Some(path) = settings.and_then(|s| s.binary).and_then(|b| b.path) {
            return Ok(path);
        }
        if let Some(path) = worktree.which("crol") {
            return Ok(path);
        }
        if let Some(path) = &self.cached_binary
            && fs::metadata(path).is_ok_and(|m| m.is_file())
        {
            return Ok(path.clone());
        }

        zed::set_language_server_installation_status(
            id,
            &zed::LanguageServerInstallationStatus::CheckingForUpdate,
        );
        let release = zed::latest_github_release(
            REPOSITORY,
            zed::GithubReleaseOptions {
                require_assets: true,
                pre_release: false,
            },
        )?;
        let (os, arch) = zed::current_platform();
        let target = match (os, arch) {
            (zed::Os::Linux, zed::Architecture::X8664) => "x86_64-unknown-linux-musl",
            (zed::Os::Linux, zed::Architecture::Aarch64) => "aarch64-unknown-linux-musl",
            (zed::Os::Mac, zed::Architecture::X8664) => "x86_64-apple-darwin",
            (zed::Os::Mac, zed::Architecture::Aarch64) => "aarch64-apple-darwin",
            (zed::Os::Windows, zed::Architecture::X8664) => "x86_64-pc-windows-msvc",
            (zed::Os::Windows, zed::Architecture::Aarch64) => "aarch64-pc-windows-msvc",
            _ => {
                return Err(format!(
                    "no crol release for {os:?} on {arch:?}; set `binary.path` in the settings"
                ));
            }
        };
        let windows = matches!(os, zed::Os::Windows);
        let (extension, file_type) = if windows {
            ("zip", zed::DownloadedFileType::Zip)
        } else {
            ("tar.gz", zed::DownloadedFileType::GzipTar)
        };
        // The names the release workflow gives its assets (`SPEC.md` 14.4).
        let name = format!("crol-{}-{target}", release.version);
        let asset_name = format!("{name}.{extension}");
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == asset_name)
            .ok_or_else(|| format!("the release {} has no {asset_name}", release.version))?;
        let directory = format!("crol-{}", release.version);
        let binary = format!(
            "{directory}/{name}/crol{}",
            if windows { ".exe" } else { "" }
        );
        if !fs::metadata(&binary).is_ok_and(|m| m.is_file()) {
            zed::set_language_server_installation_status(
                id,
                &zed::LanguageServerInstallationStatus::Downloading,
            );
            zed::download_file(&asset.download_url, &directory, file_type)?;
            zed::make_file_executable(&binary)?;
            // Older versions are not needed any more.
            if let Ok(entries) = fs::read_dir(".") {
                for entry in entries.flatten() {
                    let file_name = entry.file_name();
                    let file_name = file_name.to_string_lossy();
                    if file_name.starts_with("crol-") && file_name != directory {
                        let _ = fs::remove_dir_all(entry.path());
                    }
                }
            }
        }
        self.cached_binary = Some(binary.clone());
        Ok(binary)
    }
}

impl zed::Extension for CrolExtension {
    fn new() -> Self {
        Self {
            cached_binary: None,
        }
    }

    fn language_server_command(
        &mut self,
        id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<zed::Command> {
        Ok(zed::Command {
            command: self.binary(id, worktree)?,
            args: vec!["lsp".to_owned()],
            env: Vec::new(),
        })
    }

    fn language_server_initialization_options(
        &mut self,
        _: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        Ok(LspSettings::for_worktree("crol", worktree)
            .ok()
            .and_then(|s| s.initialization_options))
    }

    fn language_server_workspace_configuration(
        &mut self,
        _: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> Result<Option<zed::serde_json::Value>> {
        Ok(LspSettings::for_worktree("crol", worktree)
            .ok()
            .and_then(|s| s.settings))
    }
}

zed::register_extension!(CrolExtension);
