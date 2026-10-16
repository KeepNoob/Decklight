use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use fs2::FileExt;
use pdfium_render::prelude::{PdfDocument, Pdfium};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime};

const PDFIUM_VERSION: &str = "8100";
const PDFIUM_RELEASE: &str = "157.0.8100.0";
const PDFIUM_SOURCE: &str = "bblanchon/pdfium-binaries chromium/8100";
const PDFIUM_RELEASE_BASE_URL: &str =
    "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/8100/";
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LIBRARY_BYTES: u64 = 128 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlatformInfo {
    target: &'static str,
    archive_name: &'static str,
    library_member: &'static str,
    library_name: &'static str,
    archive_size: u64,
    archive_sha256: &'static str,
    library_size: u64,
    library_sha256: &'static str,
}

const LINUX_AARCH64: PlatformInfo = PlatformInfo {
    target: "linux-aarch64",
    archive_name: "pdfium-linux-arm64.tgz",
    library_member: "lib/libpdfium.so",
    library_name: "libpdfium.so",
    archive_size: 3_714_894,
    archive_sha256: "58c87a95a40a24c914a9c755fae798843f46f06adf9e5e9f2c893302d4bdb040",
    library_size: 8_091_184,
    library_sha256: "51faba3b1b580d8da572b560442f51e1e629180bae70ed37294a19f220623026",
};

const LINUX_X86_64: PlatformInfo = PlatformInfo {
    target: "linux-x86_64",
    archive_name: "pdfium-linux-x64.tgz",
    library_member: "lib/libpdfium.so",
    library_name: "libpdfium.so",
    archive_size: 3_799_482,
    archive_sha256: "1c1894a12d4d96ee01400c3874444126254180c5c7d2ab628576b67f8120320a",
    library_size: 7_964_952,
    library_sha256: "b8784cbd652ad2993332b7695e2c80f3a797f877fc47d9be97b66bca9639539e",
};

const MACOS_AARCH64: PlatformInfo = PlatformInfo {
    target: "macos-aarch64",
    archive_name: "pdfium-mac-arm64.tgz",
    library_member: "lib/libpdfium.dylib",
    library_name: "libpdfium.dylib",
    archive_size: 3_529_672,
    archive_sha256: "7ea647c7a7a9c1169be3f947000bc071ece2fade2cd6bd7e401e3675c6560cce",
    library_size: 7_360_768,
    library_sha256: "38d2326d9bfa8ed935bcd757348244d96e1ea4b0e274d3ee789c5b922726e350",
};

const MACOS_X86_64: PlatformInfo = PlatformInfo {
    target: "macos-x86_64",
    archive_name: "pdfium-mac-x64.tgz",
    library_member: "lib/libpdfium.dylib",
    library_name: "libpdfium.dylib",
    archive_size: 3_725_041,
    archive_sha256: "d513a0530a0f0c1ca9929ded06660e27edb9a108a7d18d9df55a5fb060b091c8",
    library_size: 7_733_224,
    library_sha256: "89d52641d360e6963d41764a3e12f973903ba536cf23fbfd4333a6a2ef03cd48",
};

const WINDOWS_AARCH64: PlatformInfo = PlatformInfo {
    target: "windows-aarch64",
    archive_name: "pdfium-win-arm64.tgz",
    library_member: "bin/pdfium.dll",
    library_name: "pdfium.dll",
    archive_size: 3_643_348,
    archive_sha256: "cca9b28d406ae2a902b0b710a92ab699c4c2931564ab6efe44630014d492a9ca",
    library_size: 6_878_720,
    library_sha256: "9b73b701916dbbca1a7c2ea79b228a9536bbff90bf9ee67e93b3c13b591f0daa",
};

const WINDOWS_X86_64: PlatformInfo = PlatformInfo {
    target: "windows-x86_64",
    archive_name: "pdfium-win-x64.tgz",
    library_member: "bin/pdfium.dll",
    library_name: "pdfium.dll",
    archive_size: 3_875_218,
    archive_sha256: "51621bcc1d9be9ea448f4b84b0516862b928e3968d3fca899af1a6d6e2848d6d",
    library_size: 7_512_064,
    library_sha256: "ae913f407e8d3f5cdbf08a9869564d647b13e132e01eb376c0a5a33b7a227a45",
};

static RESOLUTION_MUTEX: Mutex<()> = Mutex::new(());
static PDFIUM_INSTANCE: OnceLock<Pdfium> = OnceLock::new();
static PDFIUM_FFI_MUTEX: Mutex<Option<CachedPdfiumDocument>> = Mutex::new(None);
static TEMPORARY_FILE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub(super) fn bind_pdfium() -> anyhow::Result<Pdfium> {
    #[cfg(target_arch = "wasm32")]
    {
        bail!(
            "PDFium runtime is unsupported on wasm32; supported targets are macOS, Windows, and Linux on x86_64 or aarch64"
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let library_path = ensure_pdfium_library().with_context(|| {
            format!(
                "failed to prepare pinned PDFium {PDFIUM_RELEASE} from {PDFIUM_SOURCE}; check network access and the local application-data directory"
            )
        })?;
        let bindings = Pdfium::bind_to_library(&library_path).map_err(|error| {
            anyhow::anyhow!(
                "failed to bind PDFium library at {} from {PDFIUM_SOURCE} ({PDFIUM_RELEASE}): {error}",
                library_path.display()
            )
        })?;
        Ok(Pdfium::new(bindings))
    }
}

pub(super) struct CachedPdfiumDocument {
    path: PathBuf,
    modified_at: Option<SystemTime>,
    length: u64,
    document: PdfDocument<'static>,
}

impl CachedPdfiumDocument {
    fn matches(&self, path: &Path, metadata: &fs::Metadata) -> bool {
        self.path == path
            && self.length == metadata.len()
            && self.modified_at == metadata.modified().ok()
    }
}

fn pdfium_instance() -> Result<&'static Pdfium> {
    if let Some(pdfium) = PDFIUM_INSTANCE.get() {
        return Ok(pdfium);
    }

    let pdfium = bind_pdfium()?;
    if PDFIUM_INSTANCE.set(pdfium).is_err() {
        bail!("PDFium instance initialization raced unexpectedly");
    }
    PDFIUM_INSTANCE
        .get()
        .ok_or_else(|| anyhow::anyhow!("PDFium instance was not available after initialization"))
}

fn lock_pdfium_ffi() -> MutexGuard<'static, Option<CachedPdfiumDocument>> {
    match PDFIUM_FFI_MUTEX.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Exclusive access to PDFium. PDFium is not thread-safe, so every call into it, including
/// closing a document, happens while a session is alive.
pub(super) struct PdfiumSession<'guard> {
    pdfium: &'static Pdfium,
    cached_document: &'guard mut Option<CachedPdfiumDocument>,
}

impl PdfiumSession<'_> {
    pub(super) fn pdfium(&self) -> &'static Pdfium {
        self.pdfium
    }

    /// Closes the render cache's document so its file handle is released before the file is
    /// replaced.
    pub(super) fn release_render_document(&mut self) {
        *self.cached_document = None;
    }
}

pub(super) fn with_pdfium<T>(
    operation: impl FnOnce(&mut PdfiumSession<'_>) -> Result<T>,
) -> Result<T> {
    let mut cached_document = lock_pdfium_ffi();
    let pdfium = pdfium_instance()?;
    operation(&mut PdfiumSession {
        pdfium,
        cached_document: &mut cached_document,
    })
}

/// Closes `document` while holding the PDFium lock.
pub(super) fn close_pdfium_document(document: PdfDocument<'static>) {
    let _guard = lock_pdfium_ffi();
    drop(document);
}

/// Runs `operation` against the PDFium document for `path`.
///
/// Parsing a PDF is a large share of the cost of rendering one page, so the most recently
/// used document stays open and is reused until the file's size or modification time changes.
pub(super) fn with_pdfium_document<T>(
    path: &Path,
    operation: impl FnOnce(&PdfDocument<'static>) -> Result<T>,
) -> Result<T> {
    let mut cached_document = lock_pdfium_ffi();
    let pdfium = pdfium_instance()?;
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to read metadata for {}", path.display()))?;

    let cache_is_current = cached_document
        .as_ref()
        .is_some_and(|cached| cached.matches(path, &metadata));
    if !cache_is_current {
        // Close the previous document before opening the next one so two copies are never resident.
        *cached_document = None;
        let document = pdfium
            .load_pdf_from_file(path, None)
            .with_context(|| format!("failed to load PDF with PDFium: {}", path.display()))?;
        *cached_document = Some(CachedPdfiumDocument {
            path: path.to_path_buf(),
            modified_at: metadata.modified().ok(),
            length: metadata.len(),
            document,
        });
    }

    let cached = cached_document
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("PDFium document cache was empty after loading"))?;
    operation(&cached.document)
}

fn platform_info(os: &str, arch: &str) -> Result<&'static PlatformInfo> {
    match (os, arch) {
        ("linux", "aarch64") => Ok(&LINUX_AARCH64),
        ("linux", "x86_64") => Ok(&LINUX_X86_64),
        ("macos", "aarch64") => Ok(&MACOS_AARCH64),
        ("macos", "x86_64") => Ok(&MACOS_X86_64),
        ("windows", "aarch64") => Ok(&WINDOWS_AARCH64),
        ("windows", "x86_64") => Ok(&WINDOWS_X86_64),
        _ => bail!(
            "unsupported PDFium platform {os}/{arch}; supported targets are macOS, Windows, and Linux on x86_64 or aarch64"
        ),
    }
}

fn current_platform() -> Result<&'static PlatformInfo> {
    platform_info_for_environment(
        std::env::consts::OS,
        std::env::consts::ARCH,
        cfg!(target_env = "musl").then_some("musl"),
    )
}

fn platform_info_for_environment(
    os: &str,
    arch: &str,
    target_environment: Option<&str>,
) -> Result<&'static PlatformInfo> {
    if os == "linux" && target_environment == Some("musl") {
        bail!(
            "unsupported PDFium platform linux/{arch} with musl; PDFium artifacts require a glibc Linux build"
        );
    }
    platform_info(os, arch)
}

fn cache_directory(platform: &PlatformInfo) -> Result<PathBuf> {
    let project_dirs = ProjectDirs::from("com", "decklight", "decklight").ok_or_else(|| {
        anyhow::anyhow!("could not determine the local application-data directory for Decklight")
    })?;
    Ok(project_dirs
        .data_local_dir()
        .join("runtimes")
        .join("pdfium")
        .join(PDFIUM_VERSION)
        .join(platform.target))
}

fn ensure_pdfium_library() -> Result<PathBuf> {
    let _resolution_guard = match RESOLUTION_MUTEX.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };

    let platform = current_platform()?;
    let cache_directory = cache_directory(platform)?;
    let library_path = cache_directory.join(platform.library_name);

    if validate_cached_library(&library_path, platform)? {
        return Ok(library_path);
    }

    fs::create_dir_all(&cache_directory).with_context(|| {
        format!(
            "failed to create PDFium cache directory {}",
            cache_directory.display()
        )
    })?;
    let _cache_lock = acquire_cache_lock(&cache_directory)?;

    if validate_cached_library(&library_path, platform)? {
        return Ok(library_path);
    }

    remove_file_if_present(&library_path).with_context(|| {
        format!(
            "failed to remove an invalid cached PDFium library at {}",
            library_path.display()
        )
    })?;

    let archive = download_archive(platform)?;
    let library = extract_library_bytes(
        &archive,
        platform.library_member,
        platform.library_size,
        platform.library_sha256,
    )?;
    verify_bytes(
        &library,
        platform.library_size,
        platform.library_sha256,
        "extracted PDFium library",
    )?;
    atomic_publish_bytes(&library_path, &library)?;

    if !validate_cached_library(&library_path, platform)? {
        bail!(
            "published PDFium library at {} failed post-publication size or SHA-256 validation",
            library_path.display()
        );
    }

    Ok(library_path)
}

fn acquire_cache_lock(cache_directory: &Path) -> Result<File> {
    let lock_path = cache_directory.join(".pdfium.lock");
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed to open PDFium cache lock {}", lock_path.display()))?;
    lock_file
        .lock_exclusive()
        .with_context(|| format!("failed to lock PDFium cache lock {}", lock_path.display()))?;
    Ok(lock_file)
}

fn download_archive(platform: &PlatformInfo) -> Result<Vec<u8>> {
    let url = format!("{PDFIUM_RELEASE_BASE_URL}{}", platform.archive_name);
    let client = Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(format!("Decklight/{PDFIUM_RELEASE} PDFium runtime"))
        .build()
        .context("failed to create the PDFium download client")?;
    let mut response = client
        .get(&url)
        .send()
        .with_context(|| format!("failed to download pinned PDFium archive from {url}"))?
        .error_for_status()
        .with_context(|| format!("pinned PDFium archive request returned an error for {url}"))?;

    if let Some(content_length) = response.content_length()
        && content_length > MAX_ARCHIVE_BYTES
    {
        bail!(
            "PDFium archive response from {url} exceeds the {} byte download limit",
            MAX_ARCHIVE_BYTES
        );
    }
    if let Some(content_length) = response.content_length()
        && content_length != platform.archive_size
    {
        bail!(
            "PDFium archive response from {url} has size {content_length}, expected {}",
            platform.archive_size
        );
    }

    let capacity = usize::try_from(platform.archive_size)
        .context("PDFium archive size does not fit in this process address space")?;
    let mut archive = Vec::with_capacity(capacity);
    let mut buffer = [0_u8; 64 * 1024];
    let mut downloaded = 0_u64;
    loop {
        let bytes_read = response
            .read(&mut buffer)
            .context("failed while reading the downloaded PDFium archive")?;
        if bytes_read == 0 {
            break;
        }
        downloaded = downloaded
            .checked_add(bytes_read as u64)
            .context("PDFium archive download size overflowed")?;
        if downloaded > MAX_ARCHIVE_BYTES {
            bail!(
                "PDFium archive response from {url} exceeds the {} byte download limit",
                MAX_ARCHIVE_BYTES
            );
        }
        archive.extend_from_slice(&buffer[..bytes_read]);
    }

    verify_bytes(
        &archive,
        platform.archive_size,
        platform.archive_sha256,
        "downloaded PDFium archive",
    )?;
    Ok(archive)
}

fn extract_library_bytes(
    archive_bytes: &[u8],
    expected_member: &str,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<Vec<u8>> {
    if archive_bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        bail!(
            "PDFium archive exceeds the {} byte extraction limit",
            MAX_ARCHIVE_BYTES
        );
    }

    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(archive_bytes));
    let mut archive = tar::Archive::new(decoder);
    let mut extracted_library = None;

    for entry in archive
        .entries()
        .context("failed to enumerate the PDFium archive")?
    {
        let mut entry = entry.context("failed to read a PDFium archive entry")?;
        let entry_path = entry
            .path()
            .context("failed to read a PDFium archive entry path")?
            .into_owned();
        if entry_path != Path::new(expected_member) {
            continue;
        }

        if !entry.header().entry_type().is_file() {
            bail!("expected PDFium archive member {expected_member} is not a regular file");
        }

        let entry_size = entry
            .header()
            .size()
            .context("failed to read the PDFium archive member size")?;
        if entry_size > MAX_LIBRARY_BYTES {
            bail!(
                "PDFium archive member {expected_member} exceeds the {} byte extraction limit",
                MAX_LIBRARY_BYTES
            );
        }
        if entry_size != expected_size {
            bail!(
                "PDFium archive member {expected_member} has size {entry_size}, expected {expected_size}"
            );
        }
        if extracted_library.is_some() {
            bail!("PDFium archive contains duplicate member {expected_member}");
        }

        let capacity = usize::try_from(entry_size)
            .context("PDFium library size does not fit in this process address space")?;
        let mut library = Vec::with_capacity(capacity);
        entry
            .read_to_end(&mut library)
            .with_context(|| format!("failed to read PDFium archive member {expected_member}"))?;
        if library.len() as u64 != entry_size {
            bail!("PDFium archive member {expected_member} was truncated while extracting");
        }
        verify_bytes(
            &library,
            expected_size,
            expected_sha256,
            "extracted PDFium library",
        )?;
        extracted_library = Some(library);
    }

    extracted_library.ok_or_else(|| {
        anyhow::anyhow!("PDFium archive does not contain the expected member {expected_member}")
    })
}

fn verify_bytes(
    bytes: &[u8],
    expected_size: u64,
    expected_sha256: &str,
    label: &str,
) -> Result<()> {
    let actual_size = bytes.len() as u64;
    if actual_size != expected_size {
        bail!("{label} has size {actual_size}, expected {expected_size}");
    }

    let actual_sha256 = sha256_hex(bytes);
    if actual_sha256 != expected_sha256 {
        bail!("{label} has SHA-256 {actual_sha256}, expected {expected_sha256}");
    }
    Ok(())
}

fn validate_cached_library(path: &Path, platform: &PlatformInfo) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to inspect cached PDFium library {}", path.display())
            });
        }
    };
    if !metadata.file_type().is_file() || metadata.len() != platform.library_size {
        return Ok(false);
    }

    let mut file = File::open(path)
        .with_context(|| format!("failed to open cached PDFium library {}", path.display()))?;
    let file_metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect cached PDFium library {}", path.display()))?;
    if !file_metadata.file_type().is_file() || file_metadata.len() != platform.library_size {
        return Ok(false);
    }

    let mut hasher = Sha256::new();
    let mut total_read = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let bytes_read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read cached PDFium library {}", path.display()))?;
        if bytes_read == 0 {
            break;
        }
        total_read = total_read
            .checked_add(bytes_read as u64)
            .context("cached PDFium library size overflowed")?;
        if total_read > platform.library_size {
            return Ok(false);
        }
        hasher.update(&buffer[..bytes_read]);
    }

    let actual_sha256 = digest_to_hex(hasher.finalize());
    Ok(total_read == platform.library_size && actual_sha256 == platform.library_sha256)
}

fn sha256_hex(bytes: impl AsRef<[u8]>) -> String {
    digest_to_hex(Sha256::digest(bytes.as_ref()))
}

fn digest_to_hex(digest: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let digest = digest.as_ref();
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest.iter().copied() {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn atomic_publish_bytes(destination: &Path, bytes: &[u8]) -> Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("PDFium cache destination has no parent directory"))?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create PDFium cache directory {}",
            parent.display()
        )
    })?;

    let file_name = destination
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("PDFium cache destination has no file name"))?
        .to_string_lossy();
    let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporary_path = parent.join(format!(
        ".{file_name}.tmp-{}-{sequence}",
        std::process::id()
    ));

    let publish_result = (|| -> Result<()> {
        {
            let mut temporary_file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary_path)
                .with_context(|| {
                    format!(
                        "failed to create temporary PDFium library {}",
                        temporary_path.display()
                    )
                })?;
            temporary_file.write_all(bytes).with_context(|| {
                format!(
                    "failed to write temporary PDFium library {}",
                    temporary_path.display()
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&temporary_path, fs::Permissions::from_mode(0o755))
                    .with_context(|| {
                        format!(
                            "failed to set usable permissions on temporary PDFium library {}",
                            temporary_path.display()
                        )
                    })?;
            }
            temporary_file.sync_all().with_context(|| {
                format!(
                    "failed to sync temporary PDFium library {}",
                    temporary_path.display()
                )
            })?;
        }

        #[cfg(windows)]
        remove_file_if_present(destination).with_context(|| {
            format!(
                "failed to replace existing PDFium library {}",
                destination.display()
            )
        })?;

        fs::rename(&temporary_path, destination).with_context(|| {
            format!(
                "failed to atomically publish PDFium library {}",
                destination.display()
            )
        })?;
        Ok(())
    })();

    match publish_result {
        Ok(()) => Ok(()),
        Err(error) => match remove_file_if_present(&temporary_path) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(error.context(format!(
                "also failed to clean temporary PDFium library {}: {cleanup_error}",
                temporary_path.display()
            ))),
        },
    }
}

fn remove_file_if_present(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => fs::remove_file(path)
            .with_context(|| format!("failed to remove temporary PDFium file {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error)
            .with_context(|| format!("failed to inspect temporary PDFium file {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::fs;
    use std::io::Cursor;
    use tar::{Builder, EntryType, Header};
    use tempfile::tempdir;

    const TEST_PLATFORM: PlatformInfo = PlatformInfo {
        target: "test",
        archive_name: "fixture.tgz",
        library_member: "lib/libpdfium.test",
        library_name: "libpdfium.test",
        archive_size: 0,
        archive_sha256: "",
        library_size: 1,
        library_sha256: "ca978112ca1bbdcafac231b39a23dc4da786eff8147c4e72b9807785afee48bb",
    };

    fn archive_with_entry(path: &str, bytes: &[u8], entry_type: EntryType) -> Result<Vec<u8>> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut builder = Builder::new(encoder);
        let mut header = Header::new_gnu();
        header
            .set_path(path)
            .with_context(|| format!("failed to set fixture path {path}"))?;
        header.set_size(bytes.len() as u64);
        header.set_entry_type(entry_type);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append(&header, Cursor::new(bytes))
            .context("failed to append fixture archive entry")?;
        let encoder = builder
            .into_inner()
            .context("failed to finish fixture tar archive")?;
        encoder
            .finish()
            .context("failed to finish fixture gzip stream")
    }

    #[test]
    fn maps_all_supported_platforms_to_pinned_artifacts() -> Result<()> {
        let cases = [
            (
                "linux",
                "aarch64",
                "linux-aarch64",
                "pdfium-linux-arm64.tgz",
                "lib/libpdfium.so",
                "libpdfium.so",
                3_714_894,
                "58c87a95a40a24c914a9c755fae798843f46f06adf9e5e9f2c893302d4bdb040",
                8_091_184,
                "51faba3b1b580d8da572b560442f51e1e629180bae70ed37294a19f220623026",
            ),
            (
                "linux",
                "x86_64",
                "linux-x86_64",
                "pdfium-linux-x64.tgz",
                "lib/libpdfium.so",
                "libpdfium.so",
                3_799_482,
                "1c1894a12d4d96ee01400c3874444126254180c5c7d2ab628576b67f8120320a",
                7_964_952,
                "b8784cbd652ad2993332b7695e2c80f3a797f877fc47d9be97b66bca9639539e",
            ),
            (
                "macos",
                "aarch64",
                "macos-aarch64",
                "pdfium-mac-arm64.tgz",
                "lib/libpdfium.dylib",
                "libpdfium.dylib",
                3_529_672,
                "7ea647c7a7a9c1169be3f947000bc071ece2fade2cd6bd7e401e3675c6560cce",
                7_360_768,
                "38d2326d9bfa8ed935bcd757348244d96e1ea4b0e274d3ee789c5b922726e350",
            ),
            (
                "macos",
                "x86_64",
                "macos-x86_64",
                "pdfium-mac-x64.tgz",
                "lib/libpdfium.dylib",
                "libpdfium.dylib",
                3_725_041,
                "d513a0530a0f0c1ca9929ded06660e27edb9a108a7d18d9df55a5fb060b091c8",
                7_733_224,
                "89d52641d360e6963d41764a3e12f973903ba536cf23fbfd4333a6a2ef03cd48",
            ),
            (
                "windows",
                "aarch64",
                "windows-aarch64",
                "pdfium-win-arm64.tgz",
                "bin/pdfium.dll",
                "pdfium.dll",
                3_643_348,
                "cca9b28d406ae2a902b0b710a92ab699c4c2931564ab6efe44630014d492a9ca",
                6_878_720,
                "9b73b701916dbbca1a7c2ea79b228a9536bbff90bf9ee67e93b3c13b591f0daa",
            ),
            (
                "windows",
                "x86_64",
                "windows-x86_64",
                "pdfium-win-x64.tgz",
                "bin/pdfium.dll",
                "pdfium.dll",
                3_875_218,
                "51621bcc1d9be9ea448f4b84b0516862b928e3968d3fca899af1a6d6e2848d6d",
                7_512_064,
                "ae913f407e8d3f5cdbf08a9869564d647b13e132e01eb376c0a5a33b7a227a45",
            ),
        ];

        for (
            os,
            arch,
            target,
            archive_name,
            library_member,
            library_name,
            archive_size,
            archive_sha256,
            library_size,
            library_sha256,
        ) in cases
        {
            let platform = platform_info(os, arch)?;
            assert_eq!(platform.target, target);
            assert_eq!(platform.archive_name, archive_name);
            assert_eq!(platform.library_member, library_member);
            assert_eq!(platform.library_name, library_name);
            assert_eq!(platform.archive_size, archive_size);
            assert_eq!(platform.archive_sha256, archive_sha256);
            assert_eq!(platform.library_size, library_size);
            assert_eq!(platform.library_sha256, library_sha256);
        }
        Ok(())
    }

    #[test]
    fn rejects_unsupported_platforms() {
        assert!(platform_info("freebsd", "x86_64").is_err());
        assert!(platform_info("linux", "x86").is_err());
        assert!(platform_info("windows", "arm").is_err());
    }

    #[test]
    fn rejects_linux_musl_without_using_glibc_artifacts() {
        let error = platform_info_for_environment("linux", "x86_64", Some("musl"))
            .expect_err("musl Linux must not map to a glibc artifact");
        assert!(error.to_string().contains("musl"));
        assert!(platform_info_for_environment("linux", "x86_64", Some("gnu")).is_ok());
    }

    #[test]
    fn verifies_exact_size_and_sha256() -> Result<()> {
        let bytes = b"pdfium fixture";
        let digest = sha256_hex(bytes);
        verify_bytes(bytes, bytes.len() as u64, &digest, "fixture")?;
        assert!(verify_bytes(bytes, bytes.len() as u64 + 1, &digest, "fixture").is_err());
        assert!(verify_bytes(bytes, bytes.len() as u64, "00", "fixture").is_err());
        Ok(())
    }

    #[test]
    fn validates_cached_library_and_rejects_same_sized_corruption() -> Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join(TEST_PLATFORM.library_name);
        fs::write(&path, b"a")?;
        assert!(validate_cached_library(&path, &TEST_PLATFORM)?);

        fs::write(&path, b"b")?;
        assert!(!validate_cached_library(&path, &TEST_PLATFORM)?);
        Ok(())
    }

    #[test]
    fn atomically_published_library_passes_cache_validation() -> Result<()> {
        let directory = tempdir()?;
        let path = directory.path().join(TEST_PLATFORM.library_name);
        atomic_publish_bytes(&path, b"a")?;
        assert!(validate_cached_library(&path, &TEST_PLATFORM)?);
        Ok(())
    }

    #[test]
    fn extracts_only_the_expected_regular_member() -> Result<()> {
        let library = b"pdfium fixture";
        let archive = archive_with_entry("lib/libpdfium.so", library, EntryType::Regular)?;
        let digest = sha256_hex(library);
        let extracted =
            extract_library_bytes(&archive, "lib/libpdfium.so", library.len() as u64, &digest)?;
        assert_eq!(extracted, library);
        Ok(())
    }

    #[test]
    fn rejects_missing_or_wrong_member() -> Result<()> {
        let archive = archive_with_entry("lib/other.so", b"pdfium", EntryType::Regular)?;
        let error = extract_library_bytes(&archive, "lib/libpdfium.so", 6, "00")
            .expect_err("member should be missing");
        assert!(error.to_string().contains("expected member"));
        Ok(())
    }

    #[test]
    fn rejects_oversized_member() -> Result<()> {
        let archive = archive_with_entry("lib/libpdfium.so", b"too large", EntryType::Regular)?;
        let digest = sha256_hex(b"too large");
        let error = extract_library_bytes(&archive, "lib/libpdfium.so", 3, &digest)
            .expect_err("member size should not match the manifest");
        assert!(error.to_string().contains("has size"));
        Ok(())
    }

    #[test]
    fn rejects_non_file_expected_member() -> Result<()> {
        let archive = archive_with_entry("lib/libpdfium.so", &[], EntryType::Directory)?;
        let error = extract_library_bytes(
            &archive,
            "lib/libpdfium.so",
            0,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        )
        .expect_err("directory should not be accepted as the library");
        assert!(error.to_string().contains("not a regular file"));
        Ok(())
    }
}
