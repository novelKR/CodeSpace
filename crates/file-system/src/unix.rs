//! No-follow unix mode bits. Codex `FileMetadata` has no mode field, so
//! these helpers stay CodeSpace-owned and walk from `/` with `O_NOFOLLOW`.

use std::ffi::OsString;
use std::io;
use std::path::{Component, Path};

use rustix::fd::OwnedFd;
use rustix::fs::{chmodat, open, openat, statat, AtFlags, FileType, Mode, OFlags};

use crate::{map_io, FsError};

#[cfg(any(target_os = "linux", target_os = "android"))]
fn directory_access_flags() -> OFlags {
    OFlags::PATH
}

#[cfg(target_vendor = "apple")]
fn directory_access_flags() -> OFlags {
    OFlags::from_bits_retain(libc::O_SEARCH as u32)
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
fn directory_access_flags() -> OFlags {
    OFlags::RDONLY
}

fn components(path: &Path) -> io::Result<Vec<OsString>> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no-follow filesystem operations require an absolute path",
        ));
    }
    path.components()
        .filter_map(|component| match component {
            Component::RootDir => None,
            Component::Normal(component) => Some(Ok(component.to_os_string())),
            Component::CurDir | Component::ParentDir | Component::Prefix(_) => {
                Some(Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "no-follow filesystem operations require a normalized path",
                )))
            }
        })
        .collect()
}

fn root() -> io::Result<OwnedFd> {
    open(
        "/",
        directory_access_flags() | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)
}

fn open_directory(parent: &OwnedFd, name: &OsString) -> io::Result<OwnedFd> {
    openat(
        parent,
        name.as_os_str(),
        directory_access_flags() | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)
}

fn parent(path: &Path) -> io::Result<(OwnedFd, OsString)> {
    let mut components = components(path)?;
    let leaf = components
        .pop()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path must name an entry"))?;
    let mut directory = root()?;
    for component in components {
        directory = open_directory(&directory, &component)?;
    }
    Ok((directory, leaf))
}

pub(crate) fn unix_mode_sync(path: &Path) -> Result<u32, FsError> {
    let (directory, leaf) = parent(path).map_err(map_io)?;
    let metadata = statat(&directory, leaf.as_os_str(), AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|err| map_io(io::Error::from(err)))?;
    if FileType::from_raw_mode(metadata.st_mode).is_symlink() {
        return Err(FsError::SymlinkRejected);
    }
    Ok(metadata.st_mode as u32)
}

pub(crate) fn set_unix_mode_sync(path: &Path, mode: u32) -> Result<(), FsError> {
    let (directory, leaf) = parent(path).map_err(map_io)?;
    let metadata = statat(&directory, leaf.as_os_str(), AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|err| map_io(io::Error::from(err)))?;
    if FileType::from_raw_mode(metadata.st_mode).is_symlink() {
        return Err(FsError::SymlinkRejected);
    }
    chmodat(
        &directory,
        leaf.as_os_str(),
        Mode::from_raw_mode(mode as rustix::fs::RawMode),
        AtFlags::SYMLINK_NOFOLLOW,
    )
    .map_err(|err| map_io(io::Error::from(err)))
}
