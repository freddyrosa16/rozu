//! File replacement relative to opened directories, never a re-resolved pathname.

#[cfg(unix)]
mod unix {
    use std::ffi::{CStr, CString};
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Component, Path};

    use crate::{Workspace, patch::MAX_PATCH_BYTES};

    fn name(value: &std::ffi::OsStr) -> io::Result<CString> {
        CString::new(value.as_encoded_bytes())
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))
    }

    fn open_at(parent: &File, name: &CStr, flags: i32) -> io::Result<File> {
        // The borrowed directory remains open; the returned descriptor has one owner.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // A successful openat transfers this new descriptor to File.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub(crate) fn open_root(path: &Path) -> io::Result<File> {
        let mut directory = File::open("/")?;
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(part) => {
                    directory =
                        open_at(&directory, &name(part)?, libc::O_RDONLY | libc::O_DIRECTORY)?;
                }
                _ => return Err(io::Error::other("workspace root must be canonical")),
            }
        }
        Ok(directory)
    }

    pub(crate) struct WorkspaceFile {
        workspace: Workspace,
        // Keep all ancestors so a settings alias to any ancestor can be rejected at use.
        directories: Vec<File>,
        name: CString,
    }

    impl WorkspaceFile {
        pub(crate) fn open(workspace: &Workspace, path: &Path) -> io::Result<Self> {
            let relative = path
                .strip_prefix(workspace.root())
                .map_err(|_| io::Error::other("path is outside the workspace"))?;
            if workspace.is_protected(path) {
                return Err(io::Error::other("tools may not modify .rozu settings"));
            }
            let mut parts = relative.components().peekable();
            let mut directories = vec![workspace.directory.try_clone()?];
            while let Some(part) = parts.next() {
                let Component::Normal(part) = part else {
                    return Err(io::Error::other("write path must be resolved"));
                };
                let part = name(part)?;
                if parts.peek().is_none() {
                    let target = Self {
                        workspace: workspace.clone(),
                        directories,
                        name: part,
                    };
                    target.check_protected()?;
                    return Ok(target);
                }
                Self::check_directories(workspace, &directories)?;
                let parent = directories.last().expect("root directory");
                let directory = match open_at(parent, &part, libc::O_RDONLY | libc::O_DIRECTORY) {
                    Ok(directory) => directory,
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {
                        // Both creation and reopening are relative to the same stable parent.
                        let result =
                            unsafe { libc::mkdirat(parent.as_raw_fd(), part.as_ptr(), 0o755) };
                        if result < 0
                            && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists
                        {
                            return Err(io::Error::last_os_error());
                        }
                        open_at(parent, &part, libc::O_RDONLY | libc::O_DIRECTORY)?
                    }
                    Err(err) => return Err(err),
                };
                directories.push(directory);
            }
            Err(io::Error::other("write target has no file name"))
        }

        fn parent(&self) -> &File {
            self.directories.last().expect("root directory")
        }

        // stat's integer widths differ between macOS and Linux.
        #[allow(clippy::unnecessary_cast)]
        fn check_directories(workspace: &Workspace, directories: &[File]) -> io::Result<()> {
            // Follow only the settings entry for its identity, never to read or write data.
            // This also protects a real directory named "settings" when .rozu aliases it.
            let mut settings = std::mem::MaybeUninit::<libc::stat>::uninit();
            let result = unsafe {
                libc::fstatat(
                    workspace.directory.as_raw_fd(),
                    c".rozu".as_ptr(),
                    settings.as_mut_ptr(),
                    0,
                )
            };
            if result < 0 {
                let err = io::Error::last_os_error();
                return if err.kind() == io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(err)
                };
            }
            // fstatat initialized the complete stat structure on success.
            let settings = unsafe { settings.assume_init() };
            for directory in directories {
                let meta = directory.metadata()?;
                if meta.dev() == settings.st_dev as u64 && meta.ino() == settings.st_ino as u64 {
                    return Err(io::Error::other("tools may not modify .rozu settings"));
                }
            }
            Ok(())
        }

        fn check_protected(&self) -> io::Result<()> {
            Self::check_directories(&self.workspace, &self.directories)
        }

        fn existing(&self) -> io::Result<Option<File>> {
            self.check_protected()?;
            match open_at(self.parent(), &self.name, libc::O_RDONLY) {
                Ok(file) if file.metadata()?.is_file() => Ok(Some(file)),
                Ok(_) => Err(io::Error::other("target is not a regular file")),
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(err) => Err(err),
            }
        }

        fn replacement_metadata(&self) -> io::Result<Option<libc::stat>> {
            self.check_protected()?;
            let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
            // Inspect the leaf without following it or requiring read permission on its contents.
            let result = unsafe {
                libc::fstatat(
                    self.parent().as_raw_fd(),
                    self.name.as_ptr(),
                    metadata.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if result < 0 {
                let err = io::Error::last_os_error();
                return if err.kind() == io::ErrorKind::NotFound {
                    Ok(None)
                } else {
                    Err(err)
                };
            }
            // fstatat initialized the complete structure on success.
            let metadata = unsafe { metadata.assume_init() };
            if metadata.st_mode & libc::S_IFMT != libc::S_IFREG {
                return Err(io::Error::other("target is not a regular file"));
            }
            Ok(Some(metadata))
        }

        pub(crate) fn read(&self) -> io::Result<Option<String>> {
            let Some(file) = self.existing()? else {
                return Ok(None);
            };
            if file.metadata()?.len() > MAX_PATCH_BYTES as u64 {
                return Err(io::Error::other("patch input exceeds the 1 MiB limit"));
            }
            let mut bytes = Vec::new();
            file.take(MAX_PATCH_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_PATCH_BYTES {
                return Err(io::Error::other("patch input exceeds the 1 MiB limit"));
            }
            String::from_utf8(bytes)
                .map(Some)
                .map_err(|_| io::Error::other("patch input is not UTF-8 text"))
        }

        /// Atomically replaces the leaf and reports whether a regular file existed.
        #[allow(clippy::unnecessary_cast)] // mode_t is u16 on macOS and u32 on Linux.
        pub(crate) fn replace(&self, content: &str) -> io::Result<bool> {
            let metadata = self.replacement_metadata()?;
            let parent = self.parent();
            let mut temporary = Temporary::new(parent)?;
            temporary.file.write_all(content.as_bytes())?;
            if let Some(meta) = &metadata {
                temporary
                    .file
                    .set_permissions(std::fs::Permissions::from_mode(meta.st_mode as u32))?;
            }
            self.check_protected()?;
            // renameat does not follow the destination leaf, and both names use one parent fd.
            let result = unsafe {
                libc::renameat(
                    parent.as_raw_fd(),
                    temporary.name.as_ptr(),
                    parent.as_raw_fd(),
                    self.name.as_ptr(),
                )
            };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
            temporary.persisted = true;
            Ok(metadata.is_some())
        }
    }

    struct Temporary<'a> {
        parent: &'a File,
        name: CString,
        file: File,
        persisted: bool,
    }

    impl<'a> Temporary<'a> {
        fn new(parent: &'a File) -> io::Result<Self> {
            for _ in 0..16 {
                let mut random = [0u8; 16];
                // getentropy writes exactly this small initialized buffer, or returns an error.
                if unsafe { libc::getentropy(random.as_mut_ptr().cast(), random.len()) } < 0 {
                    return Err(io::Error::last_os_error());
                }
                let name = CString::new(format!(".rozu-{:032x}.tmp", u128::from_ne_bytes(random)))
                    .unwrap();
                match open_at(parent, &name, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL) {
                    Ok(file) => {
                        return Ok(Self {
                            parent,
                            name,
                            file,
                            persisted: false,
                        });
                    }
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(err) => return Err(err),
                }
            }
            Err(io::Error::other(
                "could not create an exclusive temporary file",
            ))
        }
    }

    impl Drop for Temporary<'_> {
        fn drop(&mut self) {
            if !self.persisted {
                // Cleanup is anchored even if the parent directory has been renamed.
                unsafe {
                    libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), 0);
                }
            }
        }
    }
}

#[cfg(unix)]
pub(crate) use unix::{WorkspaceFile, open_root};

#[cfg(not(unix))]
pub(crate) struct WorkspaceFile;

#[cfg(not(unix))]
impl WorkspaceFile {
    pub(crate) fn open(_: &crate::Workspace, _: &std::path::Path) -> std::io::Result<Self> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "secure file writes require macOS or Linux",
        ))
    }
    pub(crate) fn read(&self) -> std::io::Result<Option<String>> {
        unreachable!()
    }
    pub(crate) fn replace(&self, _: &str) -> std::io::Result<bool> {
        unreachable!()
    }
}
