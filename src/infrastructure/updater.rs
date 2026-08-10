use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use self_update::update::ReleaseUpdate;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize)]
pub struct UpdateCheck {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub target: String,
    pub asset: Option<String>,
    pub checksum_available: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum InstallStatus {
    UpToDate {
        version: String,
    },
    Updated {
        version: String,
        asset: String,
        sha256: String,
    },
}

pub struct GithubUpdater {
    owner: &'static str,
    repository: &'static str,
    binary: &'static str,
    current_version: &'static str,
}

impl GithubUpdater {
    #[must_use]
    pub const fn new(
        owner: &'static str,
        repository: &'static str,
        binary: &'static str,
        current_version: &'static str,
    ) -> Self {
        Self {
            owner,
            repository,
            binary,
            current_version,
        }
    }

    pub fn check(&self) -> Result<UpdateCheck> {
        let release = self.backend()?.get_latest_release()?;
        let target = self_update::get_target();
        let asset = release
            .asset_for(target, Some(self.binary))
            .map(|asset| asset.name);
        Ok(UpdateCheck {
            current_version: self.current_version.to_owned(),
            latest_version: release.version.clone(),
            update_available: self_update::version::bump_is_greater(
                self.current_version,
                &release.version,
            )?,
            target: target.to_owned(),
            asset,
            checksum_available: release
                .assets
                .iter()
                .any(|asset| asset.name == "SHA256SUMS"),
        })
    }

    pub fn install(&self) -> Result<InstallStatus> {
        let release = self.backend()?.get_latest_release()?;
        if !self_update::version::bump_is_greater(self.current_version, &release.version)? {
            return Ok(InstallStatus::UpToDate {
                version: self.current_version.to_owned(),
            });
        }
        let target = self_update::get_target();
        let archive = release
            .asset_for(target, Some(self.binary))
            .context("release does not contain an archive for the running target")?;
        let checksums = release
            .assets
            .iter()
            .find(|asset| asset.name == "SHA256SUMS")
            .context("release does not contain SHA256SUMS")?;

        let temporary = tempfile::TempDir::new().context("failed to create update directory")?;
        let archive_path = temporary.path().join(&archive.name);
        let checksum_path = temporary.path().join("SHA256SUMS");
        download(&archive.download_url, &archive_path)?;
        download(&checksums.download_url, &checksum_path)?;

        let checksum_text = std::fs::read_to_string(&checksum_path)
            .context("failed to read downloaded SHA256SUMS")?;
        let expected = checksum_for(&checksum_text, &archive.name)?;
        let actual = sha256_file(&archive_path)?;
        if !actual.eq_ignore_ascii_case(&expected) {
            bail!("downloaded archive checksum does not match SHA256SUMS");
        }

        let binary_name = if cfg!(windows) {
            format!("{}.exe", self.binary)
        } else {
            self.binary.to_owned()
        };
        self_update::Extract::from_source(&archive_path)
            .extract_file(temporary.path(), &binary_name)?;
        let replacement = temporary.path().join(binary_name);
        if !replacement.is_file() {
            bail!("release archive did not contain the expected executable");
        }
        self_update::self_replace::self_replace(&replacement)?;
        Ok(InstallStatus::Updated {
            version: release.version,
            asset: archive.name,
            sha256: actual,
        })
    }

    fn backend(&self) -> Result<Box<dyn ReleaseUpdate>> {
        let mut builder = self_update::backends::github::Update::configure();
        builder
            .repo_owner(self.owner)
            .repo_name(self.repository)
            .bin_name(self.binary)
            .current_version(self.current_version)
            .target(self_update::get_target())
            .identifier(self.binary)
            .show_download_progress(false)
            .show_output(false)
            .no_confirm(true);
        Ok(builder.build()?)
    }
}

fn download(url: &str, destination: &Path) -> Result<()> {
    let mut file = File::create(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    self_update::Download::from_url(url).download_to(&mut file)?;
    Ok(())
}

fn checksum_for(contents: &str, asset: &str) -> Result<String> {
    contents
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let checksum = fields.next()?;
            let name = fields.next()?.trim_start_matches('*');
            (name == asset).then(|| checksum.to_owned())
        })
        .find(|checksum| {
            checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .with_context(|| format!("SHA256SUMS does not contain {asset}"))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::{checksum_for, sha256_file};

    #[test]
    fn selects_only_the_exact_asset_checksum() {
        let value = checksum_for(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  other.zip\n\
             bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  wanted.zip\n",
            "wanted.zip",
        )
        .unwrap();
        assert_eq!(value, "b".repeat(64));
    }

    #[test]
    fn hashes_the_downloaded_bytes() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"bazaardb-cli").unwrap();
        assert_eq!(
            sha256_file(file.path()).unwrap(),
            "78d32990721e3a004d55386abe61912e8018609a855e4e52b701a3d91462a709"
        );
    }
}
