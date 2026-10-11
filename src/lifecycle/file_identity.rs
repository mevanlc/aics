use std::io;
use std::path::Path;

// Volume/device plus a 128-bit file identifier (64-bit inode on Unix).
pub(super) type FileIdentity = (u64, u64, u64);

pub(super) fn identity(path: &Path) -> io::Result<Option<FileIdentity>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path)?;
        Ok(Some((metadata.dev(), metadata.ino(), 0)))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FileIdInfo, GetFileInformationByHandleEx, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO,
        };
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)?;
        let mut info = FILE_ID_INFO::default();
        // SAFETY: file owns a live handle and info is a writable buffer of the
        // exact size and type requested by FileIdInfo, including for directories.
        let success = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileIdInfo,
                std::ptr::from_mut(&mut info).cast(),
                std::mem::size_of::<FILE_ID_INFO>() as u32,
            )
        };
        if success == 0 {
            return Err(io::Error::last_os_error());
        }
        let bytes = info.FileId.Identifier;
        Ok(Some((
            info.VolumeSerialNumber,
            u64::from_le_bytes(bytes[..8].try_into().expect("eight bytes")),
            u64::from_le_bytes(bytes[8..].try_into().expect("eight bytes")),
        )))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        // Without stable file identity, leave destinations for manual recovery.
        Ok(None)
    }
}
