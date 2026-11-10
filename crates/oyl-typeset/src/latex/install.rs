//! Fetching tectonic from its official GitHub releases into
//! `<data_dir>/bin/`, so LaTeX works on a machine without a TeX installation.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use super::run::{RunError, run};
use crate::download::Http;

/// The releases listing of the tectonic repository.
const RELEASES_API: &str =
    "https://api.github.com/repos/tectonic-typesetting/tectonic/releases?per_page=50";
/// The release used when the GitHub API cannot be reached (rate limits).
const FALLBACK_VERSION: &str = "0.17.0";
/// Refuse archives larger than this.
const MAX_ARCHIVE_BYTES: usize = 200 * 1024 * 1024;

/// The release target triple for this machine, if tectonic ships one.
pub(crate) fn target_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        _ => None,
    }
}

/// One downloadable build.
struct Release {
    version: String,
    url: String,
}

impl Release {
    fn fallback(triple: &str) -> Self {
        let version = FALLBACK_VERSION;
        Self {
            version: version.to_owned(),
            url: format!(
                "https://github.com/tectonic-typesetting/tectonic/releases/download/\
                 tectonic%40{version}/tectonic-{version}-{triple}.tar.gz"
            ),
        }
    }
}

/// The newest stable `tectonic@x.y.z` release with a build for `triple`.
/// (The repository also tags releases of its internal crates.)
fn latest_release(http: &Http, triple: &str) -> Result<Release> {
    let (_, mut body) = http.get(RELEASES_API, Some("application/vnd.github+json"))?;
    let mut text = String::new();
    body.read_to_string(&mut text)?;
    let releases: serde_json::Value =
        serde_json::from_str(&text).context("the release list is not JSON")?;
    let releases = releases
        .as_array()
        .ok_or_else(|| anyhow!("unexpected release list"))?;
    for release in releases {
        let flag = |name: &str| {
            release
                .get(name)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        };
        if flag("draft") || flag("prerelease") {
            continue;
        }
        let Some(version) = release
            .get("tag_name")
            .and_then(serde_json::Value::as_str)
            .and_then(|tag| tag.strip_prefix("tectonic@"))
        else {
            continue;
        };
        let wanted = format!("tectonic-{version}-{triple}.tar.gz");
        let assets = release.get("assets").and_then(serde_json::Value::as_array);
        let url = assets.into_iter().flatten().find_map(|asset| {
            let name = asset.get("name").and_then(serde_json::Value::as_str)?;
            (name == wanted)
                .then(|| {
                    asset
                        .get("browser_download_url")
                        .and_then(serde_json::Value::as_str)
                })
                .flatten()
        });
        if let Some(url) = url {
            return Ok(Release {
                version: version.to_owned(),
                url: url.to_owned(),
            });
        }
    }
    bail!("no tectonic release for {triple} was found")
}

fn megabytes(bytes: usize) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Downloads `url`, reporting progress about every 2 MB.
fn download(http: &Http, url: &str, label: &str, progress: &dyn Fn(&str)) -> Result<Vec<u8>> {
    let (length, mut body) = http
        .get(url, None)
        .with_context(|| format!("downloading {url}"))?;
    let mut data = Vec::with_capacity(length.unwrap_or(0).min(MAX_ARCHIVE_BYTES));
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut reported = 0;
    loop {
        let read = body
            .read(&mut buffer)
            .context("the download was interrupted")?;
        if read == 0 {
            break;
        }
        data.extend_from_slice(&buffer[..read]);
        if data.len() > MAX_ARCHIVE_BYTES {
            bail!("the download is larger than expected");
        }
        if data.len() - reported >= 2 * 1024 * 1024 {
            reported = data.len();
            progress(&match length {
                Some(total) => format!(
                    "{label} {:.1} of {:.1} MB",
                    megabytes(data.len()),
                    megabytes(total)
                ),
                None => format!("{label} {:.1} MB", megabytes(data.len())),
            });
        }
    }
    if let Some(total) = length
        && data.len() != total
    {
        bail!("the download stopped after {} of {total} bytes", data.len());
    }
    Ok(data)
}

/// Extracts the `tectonic` executable from a release archive into `dir`.
fn unpack(archive: &[u8], dir: &Path) -> Result<PathBuf> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let target = dir.join("tectonic");
    for entry in archive
        .entries()
        .context("the archive is not a tar.gz file")?
    {
        let mut entry = entry.context("the archive is damaged")?;
        let path = entry.path().context("the archive is damaged")?;
        let is_binary = path.file_name().and_then(|name| name.to_str()) == Some("tectonic");
        if is_binary && entry.header().entry_type().is_file() {
            entry
                .unpack(&target)
                .context("could not unpack the tectonic executable")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&target, fs::Permissions::from_mode(0o755))?;
            }
            return Ok(target);
        }
    }
    bail!("the archive does not contain a tectonic executable")
}

/// Runs `tectonic --version` and returns its first line.
fn check(binary: &Path) -> Result<String> {
    let mut command = Command::new(binary);
    command.arg("--version");
    let finished = match run(command, Instant::now() + Duration::from_secs(30)) {
        Ok(finished) => finished,
        Err(RunError::Spawn(err)) => {
            return Err(anyhow!(err).context("the downloaded tectonic does not start"));
        }
        Err(RunError::TimedOut) => bail!("the downloaded tectonic did not answer"),
    };
    let first = finished
        .stdout
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    if !finished.status.success() || !first.to_ascii_lowercase().starts_with("tectonic") {
        bail!(
            "the downloaded tectonic does not work: {}",
            if first.is_empty() {
                finished.stderr.trim()
            } else {
                &first
            }
        );
    }
    Ok(first)
}

/// Downloads, verifies and installs tectonic as `<data_dir>/bin/tectonic`.
pub(crate) fn install_tectonic(data_dir: &Path, progress: &dyn Fn(&str)) -> Result<PathBuf> {
    let triple =
        target_triple().ok_or_else(|| anyhow!("tectonic has no build for this computer"))?;
    let http = Http::new();

    progress("Looking up the latest tectonic release…");
    let release = latest_release(&http, triple).unwrap_or_else(|err| {
        log::warn!("tectonic release lookup failed ({err:#}); using {FALLBACK_VERSION}");
        Release::fallback(triple)
    });

    let label = format!("Downloading tectonic {}…", release.version);
    progress(&label);
    let archive = download(&http, &release.url, &label, progress)?;

    progress("Unpacking tectonic…");
    let bin = data_dir.join("bin");
    fs::create_dir_all(&bin).with_context(|| format!("creating {}", bin.display()))?;
    // Unpack next to the destination so the final step is an atomic rename.
    let staging = tempfile::Builder::new()
        .prefix(".tectonic-")
        .tempdir_in(&bin)
        .context("creating a staging folder")?;
    let unpacked = unpack(&archive, staging.path())?;

    progress("Checking tectonic…");
    let version = check(&unpacked)?;

    let destination = bin.join("tectonic");
    fs::rename(&unpacked, &destination)
        .with_context(|| format!("installing {}", destination.display()))?;

    // The first compile downloads the LaTeX format and base packages; do it
    // now, while the user expects a download, rather than in a compile that
    // has a deadline.
    progress("Fetching the LaTeX basics (about 40 MB, once)…");
    match warm_up(&destination, data_dir) {
        Ok(()) => progress(&format!("Installed {version}.")),
        Err(err) => {
            log::warn!("tectonic warm-up failed: {err:#}");
            progress(&format!(
                "Installed {version}. It fetches the LaTeX basics on the first compile."
            ));
        }
    }
    Ok(destination)
}

/// Compiles a one-line document so tectonic fills its cache.
fn warm_up(tectonic: &Path, data_dir: &Path) -> Result<()> {
    let dir = tempfile::Builder::new()
        .prefix("oyl-tectonic-")
        .tempdir()
        .context("creating a temporary folder")?;
    let source = dir.path().join("warmup.tex");
    fs::write(
        &source,
        "\\documentclass{article}\n\\begin{document}\nOyl.\n\\end{document}\n",
    )?;
    let mut command = Command::new(tectonic);
    command.current_dir(dir.path());
    super::tectonic_args(&mut command, data_dir, tectonic, dir.path());
    command.arg(&source);
    match run(command, Instant::now() + Duration::from_secs(15 * 60)) {
        Ok(finished) if finished.status.success() && dir.path().join("warmup.pdf").is_file() => {
            Ok(())
        }
        Ok(finished) => bail!("tectonic failed: {}", finished.stderr.trim()),
        Err(RunError::TimedOut) => bail!("tectonic took too long"),
        Err(RunError::Spawn(err)) => Err(anyhow!(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive_with(name: &str, contents: &[u8]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, name, contents).unwrap();
        let tar = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn unpacks_only_the_binary() {
        let dir = tempfile::tempdir().unwrap();
        let archive = archive_with("tectonic", b"#!/bin/sh\necho 'Tectonic 9.9.9'\n");
        let binary = unpack(&archive, dir.path()).unwrap();
        assert_eq!(binary, dir.path().join("tectonic"));
        #[cfg(unix)]
        assert_eq!(check(&binary).unwrap(), "Tectonic 9.9.9");

        let other = archive_with("README.md", b"hi");
        assert!(unpack(&other, dir.path()).is_err());
        assert!(unpack(b"garbage", dir.path()).is_err());
    }

    #[test]
    fn fallback_url_names_the_release_asset() {
        let release = Release::fallback("aarch64-apple-darwin");
        assert_eq!(
            release.url,
            "https://github.com/tectonic-typesetting/tectonic/releases/download/\
             tectonic%400.17.0/tectonic-0.17.0-aarch64-apple-darwin.tar.gz"
        );
    }
}
