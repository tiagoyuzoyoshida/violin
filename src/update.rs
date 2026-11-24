use std::{
    fmt, fs,
    io::ErrorKind,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use flate2::read::GzDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tar::Archive;
use thiserror::Error;

use crate::config::{AppPaths, ConfigError};

const RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/tiagoyuzoyoshida/violin/releases/latest";
const SHA256SUMS_ASSET_NAME: &str = "SHA256SUMS.txt";
const EXECUTABLE_MODE: u32 = 0o755;
const LIBRARY_MODE: u32 = 0o644;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("invalid version string '{0}'")]
    InvalidVersion(String),
    #[error("unsupported architecture '{0}'")]
    UnsupportedArch(String),
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("release asset '{0}' not found")]
    MissingReleaseAsset(String),
    #[error("checksum entry for '{0}' not found in {SHA256SUMS_ASSET_NAME}")]
    MissingChecksum(String),
    #[error("invalid checksum format for '{0}' in {SHA256SUMS_ASSET_NAME}")]
    InvalidChecksum(String),
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("unsafe path in release archive: '{0}'")]
    UnsafeArchivePath(String),
    #[error("release archive does not contain 'violin' binary")]
    MissingBinaryInArchive,
    #[error("cannot determine parent directory for '{}'", .0.display())]
    MissingParentDir(PathBuf),
    #[error("failed to resolve application paths: {0}")]
    Paths(#[from] ConfigError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedLibFile {
    pub name: String,
    pub path: PathBuf,
    pub mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedRelease {
    pub binary_path: PathBuf,
    pub lib_files: Vec<ExtractedLibFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateOutcome {
    UpToDate {
        current: Version,
        latest: Version,
    },
    Updated {
        from: Version,
        to: Version,
        binary_path: PathBuf,
    },
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

struct TempDirGuard(PathBuf);

impl TempDirGuard {
    fn new(prefix: &str) -> Result<Self, std::io::Error> {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        loop {
            let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = base.join(format!("{prefix}-{pid}-{seq}"));
            match fs::create_dir(&dir) {
                Ok(()) => return Ok(Self(dir)),
                Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
                Err(err) => return Err(err),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Parses a semantic version string (`MAJOR.MINOR.PATCH` with optional leading `v`/`V`).
pub fn parse_version(raw: &str) -> Result<Version, UpdateError> {
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);

    let mut parts = stripped.split('.');
    let major = parse_component(parts.next(), raw)?;
    let minor = parse_component(parts.next(), raw)?;
    let patch = parse_component(parts.next(), raw)?;
    if parts.next().is_some() {
        return Err(UpdateError::InvalidVersion(raw.to_string()));
    }

    Ok(Version {
        major,
        minor,
        patch,
    })
}

fn parse_component(part: Option<&str>, raw: &str) -> Result<u64, UpdateError> {
    let part = part.ok_or_else(|| UpdateError::InvalidVersion(raw.to_string()))?;
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return Err(UpdateError::InvalidVersion(raw.to_string()));
    }
    part.parse::<u64>()
        .map_err(|_| UpdateError::InvalidVersion(raw.to_string()))
}

/// Maps a Rust `std::env::consts::ARCH` value to the release target triple.
pub fn target_triple_for_arch(arch: &str) -> Result<&'static str, UpdateError> {
    match arch {
        "x86_64" => Ok("x86_64-unknown-linux-musl"),
        "aarch64" => Ok("aarch64-unknown-linux-musl"),
        _ => Err(UpdateError::UnsupportedArch(arch.to_string())),
    }
}

/// Extracts and validates the lowercase SHA-256 hex digest for `filename` from `SHA256SUMS.txt`.
pub fn parse_sha256sums(contents: &str, filename: &str) -> Result<String, UpdateError> {
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((hash, rest)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let entry_name = rest.trim_start();
        let entry_name = entry_name.strip_prefix('*').unwrap_or(entry_name);
        let entry_name = entry_name.strip_prefix("./").unwrap_or(entry_name);
        if entry_name != filename {
            continue;
        }
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(UpdateError::InvalidChecksum(filename.to_string()));
        }
        return Ok(hash.to_ascii_lowercase());
    }

    Err(UpdateError::MissingChecksum(filename.to_string()))
}

/// Verifies `bytes` against an expected 64-character hex SHA-256 digest.
pub fn verify_sha256(bytes: &[u8], expected_hex: &str) -> Result<(), UpdateError> {
    let digest = Sha256::digest(bytes);
    let actual = digest.iter().fold(String::with_capacity(64), |mut acc, b| {
        use fmt::Write as _;
        let _ = write!(acc, "{b:02x}");
        acc
    });
    let expected = expected_hex.to_ascii_lowercase();
    if actual != expected {
        return Err(UpdateError::ChecksumMismatch { expected, actual });
    }
    Ok(())
}

/// Extracts `violin` and direct `lib/*` runtime libraries from a `.tar.gz` release archive.
pub fn extract_release_archive(
    archive_bytes: &[u8],
    dest_dir: &Path,
) -> Result<ExtractedRelease, UpdateError> {
    fs::create_dir_all(dest_dir)?;

    let gz = GzDecoder::new(archive_bytes);
    let mut archive = Archive::new(gz);
    let mut binary_path: Option<PathBuf> = None;
    let mut lib_files: Vec<ExtractedLibFile> = Vec::new();

    for entry in archive.entries()? {
        let mut entry = entry?;
        let raw_path = entry.path()?.into_owned();
        let components = validate_archive_path(&raw_path)?;

        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            if components.len() > 1 && components[0] == "lib" {
                return Err(UpdateError::UnsafeArchivePath(
                    raw_path.display().to_string(),
                ));
            }
            continue;
        }

        if components.as_slice() == ["violin"] {
            if !entry_type.is_file() {
                return Err(UpdateError::UnsafeArchivePath(
                    raw_path.display().to_string(),
                ));
            }
            let dest = dest_dir.join("violin");
            entry.unpack(&dest)?;
            fs::set_permissions(&dest, fs::Permissions::from_mode(EXECUTABLE_MODE))?;
            binary_path = Some(dest);
        } else if components.first().copied() == Some("lib") {
            if components.len() != 2 || !entry_type.is_file() {
                return Err(UpdateError::UnsafeArchivePath(
                    raw_path.display().to_string(),
                ));
            }
            let file_name = components[1];
            let lib_dir = dest_dir.join("lib");
            fs::create_dir_all(&lib_dir)?;
            let dest = lib_dir.join(file_name);
            entry.unpack(&dest)?;
            let mode = lib_file_mode(file_name);
            fs::set_permissions(&dest, fs::Permissions::from_mode(mode))?;
            lib_files.push(ExtractedLibFile {
                name: file_name.to_string(),
                path: dest,
                mode,
            });
        }
    }

    let binary_path = binary_path.ok_or(UpdateError::MissingBinaryInArchive)?;
    Ok(ExtractedRelease {
        binary_path,
        lib_files,
    })
}

fn validate_archive_path(path: &Path) -> Result<Vec<&str>, UpdateError> {
    if path.is_absolute() {
        return Err(UpdateError::UnsafeArchivePath(path.display().to_string()));
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(segment) => {
                let s = segment
                    .to_str()
                    .ok_or_else(|| UpdateError::UnsafeArchivePath(path.display().to_string()))?;
                parts.push(s);
            }
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(UpdateError::UnsafeArchivePath(path.display().to_string()));
            }
        }
    }
    Ok(parts)
}

fn lib_file_mode(file_name: &str) -> u32 {
    if file_name.starts_with("ld-linux-") && file_name.contains(".so.") {
        EXECUTABLE_MODE
    } else {
        LIBRARY_MODE
    }
}

/// Atomically installs the extracted `violin` binary and any `lib/*` files into their targets.
pub fn install_extracted_release(
    extracted: &ExtractedRelease,
    target_exe: &Path,
    soloist_lib_dir: &Path,
) -> Result<(), UpdateError> {
    let target_parent = target_exe
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| UpdateError::MissingParentDir(target_exe.to_path_buf()))?;
    fs::create_dir_all(target_parent)?;
    atomic_replace_file(&extracted.binary_path, target_exe, EXECUTABLE_MODE)?;

    if !extracted.lib_files.is_empty() {
        fs::create_dir_all(soloist_lib_dir)?;
        for lib in &extracted.lib_files {
            let dest = soloist_lib_dir.join(&lib.name);
            atomic_replace_file(&lib.path, &dest, lib.mode)?;
        }
    }

    Ok(())
}

fn atomic_replace_file(src: &Path, dest: &Path, mode: u32) -> Result<(), UpdateError> {
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| UpdateError::MissingParentDir(dest.to_path_buf()))?;
    let file_name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("binary");
    let seq = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let staged = parent.join(format!(
        ".{file_name}.update.{}.{seq}.tmp",
        std::process::id()
    ));

    let result = (|| -> Result<(), std::io::Error> {
        fs::copy(src, &staged)?;
        fs::set_permissions(&staged, fs::Permissions::from_mode(mode))?;
        fs::rename(&staged, dest)?;
        Ok(())
    })();

    if let Err(err) = result {
        let _ = fs::remove_file(&staged);
        return Err(err.into());
    }
    Ok(())
}

/// Checks GitHub Releases for a newer version and updates the running binary if available.
pub async fn run_update() -> Result<UpdateOutcome, UpdateError> {
    let current = parse_version(env!("CARGO_PKG_VERSION"))?;
    let target = target_triple_for_arch(std::env::consts::ARCH)?;

    let client = reqwest::Client::builder()
        .user_agent(format!("violin/{current}"))
        .build()?;

    let release: GitHubRelease = client
        .get(RELEASES_LATEST_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let latest = parse_version(&release.tag_name)?;
    if latest <= current {
        return Ok(UpdateOutcome::UpToDate { current, latest });
    }

    let tag = release.tag_name.trim();
    let archive_name = format!("violin-{tag}-{target}.tar.gz");
    let archive_asset = release
        .assets
        .iter()
        .find(|asset| asset.name == archive_name)
        .ok_or_else(|| UpdateError::MissingReleaseAsset(archive_name.clone()))?;
    let checksums_asset = release
        .assets
        .iter()
        .find(|asset| asset.name == SHA256SUMS_ASSET_NAME)
        .ok_or_else(|| UpdateError::MissingReleaseAsset(SHA256SUMS_ASSET_NAME.to_string()))?;

    let checksums_text = client
        .get(&checksums_asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let expected_sha = parse_sha256sums(&checksums_text, &archive_name)?;

    let archive_bytes = client
        .get(&archive_asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    verify_sha256(&archive_bytes, &expected_sha)?;

    let current_exe = std::env::current_exe()?;
    let target_exe = fs::canonicalize(&current_exe).unwrap_or(current_exe);
    let soloist_lib_dir = AppPaths::resolve()?.soloist_dir.join("lib");

    let temp_dir = TempDirGuard::new("violin-update")?;
    let extracted = extract_release_archive(&archive_bytes, temp_dir.path())?;
    install_extracted_release(&extracted, &target_exe, &soloist_lib_dir)?;

    Ok(UpdateOutcome::Updated {
        from: current,
        to: latest,
        binary_path: target_exe,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;
    use tar::{Builder, Header};

    fn build_tar_gz(entries: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = Builder::new(encoder);
        for (path, data, mode) in entries {
            let mut header = Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            builder
                .append_data(&mut header, *path, *data)
                .expect("append tar entry");
        }
        let encoder = builder.into_inner().expect("finish tar");
        encoder.finish().expect("finish gzip")
    }

    #[test]
    fn parse_version_accepts_valid_and_orders() {
        let v1 = parse_version("0.1.5").expect("plain version");
        let v2 = parse_version(" v0.1.6 \n").expect("v-prefixed version");
        let v3 = parse_version("V1.0.0").expect("uppercase V version");
        assert_eq!(v1.to_string(), "0.1.5");
        assert_eq!(v2.to_string(), "0.1.6");
        assert!(v1 < v2);
        assert!(v2 < v3);
    }

    #[test]
    fn parse_version_rejects_malformed() {
        for bad in [
            "",
            "0.1",
            "0.1.2.3",
            "0.1.2-rc1",
            "v",
            "0.+1.2",
            "0.-1.2",
            "a.b.c",
        ] {
            assert!(
                matches!(parse_version(bad), Err(UpdateError::InvalidVersion(_))),
                "expected InvalidVersion for {bad:?}"
            );
        }
    }

    #[test]
    fn target_triple_for_arch_maps_supported_and_rejects_others() {
        assert_eq!(
            target_triple_for_arch("x86_64").unwrap(),
            "x86_64-unknown-linux-musl"
        );
        assert_eq!(
            target_triple_for_arch("aarch64").unwrap(),
            "aarch64-unknown-linux-musl"
        );
        assert!(matches!(
            target_triple_for_arch("armv7"),
            Err(UpdateError::UnsupportedArch(_))
        ));
    }

    #[test]
    fn parse_sha256sums_and_verify_sha256_cover_formats_and_errors() {
        let payload = b"violin-release-bytes";
        let digest = Sha256::digest(payload);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let upper_hex = hex.to_ascii_uppercase();

        let sums = format!(
            "{upper_hex}  *./violin-v0.1.6-x86_64-unknown-linux-musl.tar.gz\n\
             0000000000000000000000000000000000000000000000000000000000000000  other.tar.gz\n"
        );

        let parsed = parse_sha256sums(&sums, "violin-v0.1.6-x86_64-unknown-linux-musl.tar.gz")
            .expect("parse sha256sums");
        assert_eq!(parsed, hex);

        verify_sha256(payload, &parsed).expect("matching digest");
        assert!(matches!(
            verify_sha256(b"tampered", &parsed),
            Err(UpdateError::ChecksumMismatch { .. })
        ));
        assert!(matches!(
            parse_sha256sums(&sums, "missing.tar.gz"),
            Err(UpdateError::MissingChecksum(_))
        ));
    }

    #[test]
    fn extract_release_archive_extracts_binary_and_libs_and_ignores_docs() {
        let archive = build_tar_gz(&[
            ("violin", b"new-binary", 0o755),
            ("lib/ld-linux-x86-64.so.2", b"loader", 0o755),
            ("lib/libc.so.6", b"libc", 0o644),
            ("README.md", b"# readme", 0o644),
            ("LICENSE", b"MIT", 0o644),
        ]);
        let temp = TempDirGuard::new("violin-test-extract").expect("temp dir");
        let extracted = extract_release_archive(&archive, temp.path()).expect("extract");

        assert_eq!(fs::read(&extracted.binary_path).unwrap(), b"new-binary");
        assert_eq!(
            fs::metadata(&extracted.binary_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(extracted.lib_files.len(), 2);
        assert!(!temp.path().join("README.md").exists());
        assert!(!temp.path().join("LICENSE").exists());
    }

    #[test]
    fn extract_release_archive_rejects_missing_binary_and_path_traversal() {
        let no_bin = build_tar_gz(&[("README.md", b"hi", 0o644)]);
        let temp = TempDirGuard::new("violin-test-nobin").expect("temp dir");
        assert!(matches!(
            extract_release_archive(&no_bin, temp.path()),
            Err(UpdateError::MissingBinaryInArchive)
        ));

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        {
            let mut header = Header::new_gnu();
            let data = b"evil";
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            let path_bytes = b"../evil";
            header.as_old_mut().name[..path_bytes.len()].copy_from_slice(path_bytes);
            header.set_cksum();
            let mut builder = Builder::new(&mut encoder);
            builder
                .append(&header, &data[..])
                .expect("append traversal entry");
            builder.finish().expect("finish tar");
        }
        encoder.flush().expect("flush");
        let traversal_bytes = encoder.finish().expect("finish gz");

        let temp2 = TempDirGuard::new("violin-test-traversal").expect("temp dir");
        assert!(matches!(
            extract_release_archive(&traversal_bytes, temp2.path()),
            Err(UpdateError::UnsafeArchivePath(_))
        ));

        let nested_lib = build_tar_gz(&[
            ("violin", b"bin", 0o755),
            ("lib/sub/libc.so.6", b"nested", 0o644),
        ]);
        let temp3 = TempDirGuard::new("violin-test-nested").expect("temp dir");
        assert!(matches!(
            extract_release_archive(&nested_lib, temp3.path()),
            Err(UpdateError::UnsafeArchivePath(_))
        ));
    }

    #[test]
    fn install_extracted_release_replaces_binary_and_libs_atomically() {
        let archive = build_tar_gz(&[
            ("violin", b"updated-bin", 0o755),
            ("lib/ld-linux-x86-64.so.2", b"new-loader", 0o755),
            ("lib/libc.so.6", b"new-libc", 0o644),
        ]);
        let stage_temp = TempDirGuard::new("violin-test-stage").expect("stage dir");
        let extracted = extract_release_archive(&archive, stage_temp.path()).expect("extract");

        let target_root = TempDirGuard::new("violin-test-install").expect("target dir");
        let bin_dir = target_root.path().join("bin");
        let lib_dir = target_root.path().join("soloist/lib");
        fs::create_dir_all(&bin_dir).unwrap();
        let target_exe = bin_dir.join("violin");
        fs::write(&target_exe, b"old-bin").unwrap();

        install_extracted_release(&extracted, &target_exe, &lib_dir).expect("install");

        assert_eq!(fs::read(&target_exe).unwrap(), b"updated-bin");
        assert_eq!(
            fs::metadata(&target_exe).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::read(lib_dir.join("ld-linux-x86-64.so.2")).unwrap(),
            b"new-loader"
        );
        assert_eq!(
            fs::metadata(lib_dir.join("ld-linux-x86-64.so.2"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(fs::read(lib_dir.join("libc.so.6")).unwrap(), b"new-libc");
        assert_eq!(
            fs::metadata(lib_dir.join("libc.so.6"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );

        for dir in [&bin_dir, &lib_dir] {
            for entry in fs::read_dir(dir).unwrap() {
                let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                assert!(!name.ends_with(".tmp"), "leftover temp file found: {name}");
            }
        }
    }
}
