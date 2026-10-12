//! Stable file identity and free-space probing.
//!
//! §4.3(5) requires provenance to record "source file identity when stable",
//! and §7.10 states that a filename is never sufficient identity. The stable
//! identity used here is the pair the filesystem itself guarantees: device and
//! inode on Unix, volume serial and file index on Windows. Both survive a
//! rename, and both change when a file is replaced by an atomic rename — which
//! is exactly the distinction §4.3(6) needs between "the same file moved" and
//! "a different file now occupies that path".
//!
//! When the platform cannot supply one the answer is [`None`] rather than a
//! fabricated identity, and the caller falls back to the content digest.

use std::fmt;
use std::fs::Metadata;
use std::path::Path;

/// A filesystem-guaranteed identity for one file.
///
/// Rendered as `dev:ino` on Unix and `volume:index` on Windows. The rendering
/// is opaque: nothing parses it apart from equality.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableFileId(String);

impl StableFileId {
    /// The opaque identity string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StableFileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The stable identity of an already-stat'ed file, when the platform has one.
#[cfg(unix)]
#[must_use]
pub fn stable_file_id(_path: &Path, metadata: &Metadata) -> Option<StableFileId> {
    use std::os::unix::fs::MetadataExt;

    Some(StableFileId(format!(
        "{}:{}",
        metadata.dev(),
        metadata.ino()
    )))
}

/// The stable identity of a file, when the platform has one.
///
/// Windows exposes the volume serial number and the 64-bit file index only
/// through `GetFileInformationByHandle`, so the file is opened for metadata
/// access — no data is read and nothing is modified.
#[cfg(windows)]
#[must_use]
pub fn stable_file_id(path: &Path, _metadata: &Metadata) -> Option<StableFileId> {
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low_date_time: u32,
        high_date_time: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ByHandleFileInformation {
        file_attributes: u32,
        creation_time: FileTime,
        last_access_time: FileTime,
        last_write_time: FileTime,
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

    extern "system" {
        fn GetFileInformationByHandle(
            handle: *mut core::ffi::c_void,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    let file = std::fs::File::open(path).ok()?;
    let mut information = ByHandleFileInformation::default();
    // SAFETY: `file` owns a valid handle for the duration of the call and
    // `information` is a live, correctly sized, correctly aligned struct.
    let ok = unsafe {
        GetFileInformationByHandle(
            file.as_raw_handle().cast(),
            std::ptr::addr_of_mut!(information),
        )
    };
    if ok == 0 {
        return None;
    }
    let index =
        (u64::from(information.file_index_high) << 32) | u64::from(information.file_index_low);
    Some(StableFileId(format!(
        "{}:{index}",
        information.volume_serial_number
    )))
}

/// The stable identity of a file, when the platform has one.
#[cfg(not(any(unix, windows)))]
#[must_use]
pub fn stable_file_id(_path: &Path, _metadata: &Metadata) -> Option<StableFileId> {
    None
}

/// Modification time in milliseconds since the Unix epoch, or `0` when the
/// platform does not report one.
#[must_use]
pub fn mtime_ms(metadata: &Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or(0)
}

/// Free space available on the volume holding `path`.
pub trait DiskSpaceProbe: fmt::Debug + Send + Sync {
    /// Bytes a non-privileged process may still write there.
    fn available_bytes(&self, path: &Path) -> std::io::Result<u64>;
}

/// The real volume.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsDiskSpaceProbe;

impl DiskSpaceProbe for OsDiskSpaceProbe {
    fn available_bytes(&self, path: &Path) -> std::io::Result<u64> {
        let probe = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        fs4::available_space(probe)
    }
}

/// A volume with a fixed amount of space, for the out-of-space fixture.
#[derive(Debug, Clone, Copy)]
pub struct FixedDiskSpaceProbe {
    available: u64,
}

impl FixedDiskSpaceProbe {
    /// A volume reporting exactly `available` free bytes.
    #[must_use]
    pub const fn with_available(available: u64) -> Self {
        Self { available }
    }
}

impl DiskSpaceProbe for FixedDiskSpaceProbe {
    fn available_bytes(&self, _path: &Path) -> std::io::Result<u64> {
        Ok(self.available)
    }
}
