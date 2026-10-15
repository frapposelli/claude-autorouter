//! Descriptor-anchored transfer I/O. Errors retain incomplete owned outputs.
use nix::fcntl::{AtFlags, OFlag, open, openat};
use nix::sys::stat::{FileStat, Mode, SFlag, fchmod, fstat, fstatat, mkdirat};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};

pub struct Directory {
    fd: OwnedFd,
    pub path: PathBuf,
}
fn kind(info: &FileStat, wanted: SFlag) -> bool {
    SFlag::from_bits_truncate(info.st_mode) == wanted
}
fn same(a: &FileStat, b: &FileStat) -> bool {
    a.st_dev == b.st_dev && a.st_ino == b.st_ino
}
fn segment(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        return Err("Unsafe transfer path component".into());
    }
    Ok(())
}
impl Directory {
    pub fn open(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() || path.as_os_str().len() > 4096 {
            return Err("Transfer directory must be a bounded absolute path".into());
        }
        let flags = OFlag::O_RDONLY
            | OFlag::O_DIRECTORY
            | OFlag::O_NOFOLLOW
            | OFlag::O_CLOEXEC
            | OFlag::O_NONBLOCK;
        let fd = open(Path::new("/"), flags, Mode::empty())
            .map_err(|_| "Cannot open transfer directory root")?;
        let mut current = Self {
            fd,
            path: PathBuf::from("/"),
        };
        for part in path.components() {
            match part {
                Component::RootDir => {}
                Component::Normal(part) => {
                    current =
                        current.child(part.to_str().ok_or("Invalid transfer directory name")?)?;
                }
                _ => return Err("Unsafe transfer directory path".into()),
            }
        }
        Ok(current)
    }
    fn duplicate(&self) -> Result<Self, String> {
        Ok(Self {
            fd: self
                .fd
                .try_clone()
                .map_err(|_| "Cannot retain transfer directory")?,
            path: self.path.clone(),
        })
    }
    fn child(&self, name: &str) -> Result<Self, String> {
        segment(name)?;
        let before = fstatat(&self.fd, name, AtFlags::AT_SYMLINK_NOFOLLOW)
            .map_err(|_| "Cannot inspect transfer directory")?;
        if !kind(&before, SFlag::S_IFDIR) {
            return Err("Transfer ancestors must be real directories".into());
        }
        let fd = openat(
            &self.fd,
            name,
            OFlag::O_RDONLY
                | OFlag::O_DIRECTORY
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC
                | OFlag::O_NONBLOCK,
            Mode::empty(),
        )
        .map_err(|_| "Cannot open transfer directory")?;
        let after = fstat(&fd).map_err(|_| "Cannot inspect opened transfer directory")?;
        if !kind(&after, SFlag::S_IFDIR) || !same(&before, &after) {
            return Err("Transfer directory changed during admission".into());
        }
        Ok(Self {
            fd,
            path: self.path.join(name),
        })
    }
    pub fn create(&self, name: &str) -> Result<Self, String> {
        segment(name)?;
        mkdirat(&self.fd, name, Mode::S_IRWXU)
            .map_err(|_| "Transfer destination must be a new directory")?;
        let directory = self.child(name)?;
        fchmod(&directory.fd, Mode::S_IRWXU).map_err(|_| "Cannot protect transfer directory")?;
        Ok(directory)
    }
    /// Check the attached pathname before compatibility verification/publication.
    pub fn attached(&self) -> Result<(), String> {
        let current = Self::open(&self.path)?;
        let expected = fstat(&self.fd).map_err(|_| "Cannot inspect owned transfer root")?;
        let actual = fstat(&current.fd).map_err(|_| "Cannot inspect attached transfer root")?;
        if !kind(&expected, SFlag::S_IFDIR)
            || !kind(&actual, SFlag::S_IFDIR)
            || !same(&expected, &actual)
        {
            return Err("Owned transfer destination was moved or replaced".into());
        }
        Ok(())
    }
    fn parent(&self, relative: &str) -> Result<(Self, String), String> {
        if !super::super::safe(relative) {
            return Err("Unsafe transfer file path".into());
        }
        let mut parts = relative.split('/').peekable();
        let mut current = self.duplicate()?;
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                return Ok((current, part.into()));
            }
            current = current.child(part)?;
        }
        Err("Empty transfer file path".into())
    }
    pub fn read(&self, relative: &str, maximum: usize) -> Result<Vec<u8>, String> {
        self.read_with(relative, maximum, || Ok(()))
    }
    fn read_with(
        &self,
        relative: &str,
        maximum: usize,
        before_open: impl FnOnce() -> Result<(), String>,
    ) -> Result<Vec<u8>, String> {
        let (parent, name) = self.parent(relative)?;
        let before = fstatat(&parent.fd, name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW)
            .map_err(|_| "Cannot inspect transfer input")?;
        let admitted = |info: &FileStat| {
            kind(info, SFlag::S_IFREG)
                && info.st_nlink == 1
                && usize::try_from(info.st_size).is_ok_and(|size| size <= maximum)
        };
        if !admitted(&before) {
            return Err("Transfer input must be bounded regular unlinked data".into());
        }
        before_open()?;
        // NONBLOCK prevents a raced regular-to-FIFO change from hanging open.
        let fd = openat(
            &parent.fd,
            name.as_str(),
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
            Mode::empty(),
        )
        .map_err(|_| "Cannot open transfer input")?;
        let opened = fstat(&fd).map_err(|_| "Cannot inspect opened transfer input")?;
        if !admitted(&opened) || !same(&before, &opened) || before.st_size != opened.st_size {
            return Err("Transfer input changed during admission".into());
        }
        let mut file = File::from(fd);
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(
                u64::try_from(
                    maximum
                        .checked_add(1)
                        .ok_or("Invalid transfer read bound")?,
                )
                .map_err(|_| "Invalid transfer read bound")?,
            )
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read bounded transfer input")?;
        let after = fstat(&file).map_err(|_| "Cannot inspect completed transfer read")?;
        let attached = fstatat(&parent.fd, name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW)
            .map_err(|_| "Transfer input disappeared during read")?;
        if bytes.len() > maximum
            || !admitted(&after)
            || !same(&opened, &after)
            || after.st_size != opened.st_size
            || !same(&after, &attached)
            || usize::try_from(after.st_size).ok() != Some(bytes.len())
        {
            return Err("Transfer input changed identity or bounds".into());
        }
        Ok(bytes)
    }
    fn write(
        &self,
        name: &str,
        bytes: &[u8],
        mode: u32,
        checkpoint: &mut impl FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        segment(name)?;
        if mode != 0o644 {
            return Err("Unexpected transfer file mode".into());
        }
        let fd = openat(
            &self.fd,
            name,
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::S_IRUSR | Mode::S_IWUSR,
        )
        .map_err(|_| "Refusing to overwrite transfer file")?;
        let before = fstat(&fd).map_err(|_| "Cannot inspect new transfer file")?;
        if !kind(&before, SFlag::S_IFREG) || before.st_nlink != 1 || before.st_size != 0 {
            return Err("New transfer file identity is invalid".into());
        }
        let mut file = File::from(fd);
        for chunk in bytes.chunks(8192) {
            checkpoint()?;
            file.write_all(chunk)
                .map_err(|_| "Cannot write transfer file")?;
        }
        checkpoint()?;
        fchmod(
            &file,
            Mode::S_IRUSR | Mode::S_IWUSR | Mode::S_IRGRP | Mode::S_IROTH,
        )
        .map_err(|_| "Cannot restore transfer mode")?;
        let after = fstat(&file).map_err(|_| "Cannot inspect written transfer file")?;
        if !same(&before, &after)
            || after.st_nlink != 1
            || usize::try_from(after.st_size).ok() != Some(bytes.len())
        {
            return Err("Written transfer file identity changed".into());
        }
        Ok(())
    }
}
pub fn write_tree<'a>(
    root: &Directory,
    files: impl IntoIterator<Item = (&'a str, &'a [u8], u32)>,
    mut checkpoint: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let mut directories = BTreeMap::from([(String::new(), root.duplicate()?)]);
    for (path, bytes, mode) in files {
        checkpoint()?;
        let mut parts = path.split('/').collect::<Vec<_>>();
        let name = parts.pop().ok_or("Invalid logical transfer path")?;
        segment(name)?;
        let mut parent = String::new();
        for part in parts {
            let key = if parent.is_empty() {
                part.into()
            } else {
                format!("{parent}/{part}")
            };
            if !directories.contains_key(&key) {
                let directory = directories[&parent].create(part)?;
                directories.insert(key.clone(), directory);
            }
            parent = key;
        }
        directories[&parent].write(name, bytes, mode, &mut checkpoint)?;
    }
    root.attached()
}
pub fn check_tree(
    root: &Directory,
    expected: &BTreeMap<String, (&[u8], u32)>,
) -> Result<(), String> {
    fn walk(
        root: &Directory,
        path: &str,
        expected: &BTreeMap<String, (&[u8], u32)>,
        files: &mut Vec<String>,
    ) -> Result<(), String> {
        root.attached()?;
        for entry in fs::read_dir(&root.path).map_err(|_| "Cannot inspect restored reference")? {
            let entry = entry.map_err(|_| "Cannot inspect restored reference entry")?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "Invalid restored reference name")?;
            segment(&name)?;
            let relative = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}/{name}")
            };
            if entry
                .file_type()
                .map_err(|_| "Cannot inspect restored reference type")?
                .is_dir()
            {
                let prefix = format!("{relative}/");
                if !expected.keys().any(|path| path.starts_with(&prefix)) {
                    return Err("Restored reference contains foreign directories".into());
                }
                walk(&root.child(&name)?, &relative, expected, files)?;
            } else {
                if !expected.contains_key(&relative) {
                    return Err("Restored reference contains foreign files".into());
                }
                files.push(relative);
            }
            if files.len() > expected.len() {
                return Err("Restored reference has too many files".into());
            }
        }
        root.attached()
    }
    let mut found = Vec::new();
    walk(root, "", expected, &mut found)?;
    found.sort();
    if found
        .iter()
        .map(String::as_str)
        .ne(expected.keys().map(String::as_str))
    {
        return Err("Restored reference contains missing or foreign files".into());
    }
    for (path, (bytes, mode)) in expected {
        if root.read(path, bytes.len())?.as_slice() != *bytes {
            return Err("Restored reference bytes differ".into());
        }
        let (directory, name) = root.parent(path)?;
        let info = fstatat(&directory.fd, name.as_str(), AtFlags::AT_SYMLINK_NOFOLLOW)
            .map_err(|_| "Cannot inspect restored reference permissions")?;
        if *mode != 0o644 || info.st_mode & 0o777 != 0o644 {
            return Err("Restored reference mode differs".into());
        }
    }
    root.attached()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_process::Scratch;
    use std::os::unix::fs::{PermissionsExt, symlink};
    #[test]
    fn input_links_fifo_race_bounds_and_replaced_root_are_rejected() {
        let scratch = Scratch::new("paired-input").unwrap();
        let root = Directory::open(&scratch.0).unwrap();
        fs::write(scratch.0.join("input"), b"abc").unwrap();
        assert_eq!(root.read("input", 3).unwrap(), b"abc");
        assert!(root.read("input", 2).is_err());
        fs::hard_link(scratch.0.join("input"), scratch.0.join("hard")).unwrap();
        assert!(root.read("input", 3).is_err());
        fs::remove_file(scratch.0.join("hard")).unwrap();
        symlink("input", scratch.0.join("link")).unwrap();
        assert!(root.read("link", 3).is_err());
        assert!(
            root.read_with("input", 3, || {
                fs::rename(scratch.0.join("input"), scratch.0.join("old")).unwrap();
                nix::unistd::mkfifo(&scratch.0.join("input"), Mode::S_IRUSR | Mode::S_IWUSR)
                    .unwrap();
                Ok(())
            })
            .is_err()
        );
        assert!(root.read("input", 3).is_err());
        let child = root.create("owned").unwrap();
        fs::rename(&child.path, scratch.0.join("moved")).unwrap();
        fs::create_dir(&child.path).unwrap();
        fs::write(child.path.join("foreign"), b"keep").unwrap();
        assert!(child.attached().is_err());
        assert_eq!(fs::read(child.path.join("foreign")).unwrap(), b"keep");
    }
    #[test]
    fn output_create_only_modes_cancel_and_foreign_tree_are_preserved() {
        let scratch = Scratch::new("paired-output").unwrap();
        let parent = Directory::open(&scratch.0).unwrap();
        let root = parent.create("new").unwrap();
        assert_eq!(
            fs::metadata(&root.path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert!(parent.create("new").is_err());
        let expected: BTreeMap<String, (&[u8], u32)> = BTreeMap::from([
            ("nested/a".into(), (&b"abc"[..], 0o644)),
            ("b".into(), (&b"d"[..], 0o644)),
        ]);
        write_tree(
            &root,
            expected
                .iter()
                .map(|(path, (bytes, mode))| (path.as_str(), *bytes, *mode)),
            || Ok(()),
        )
        .unwrap();
        check_tree(&root, &expected).unwrap();
        assert!(write_tree(&root, [("b", &b"replace"[..], 0o644)], || Ok(())).is_err());
        assert_eq!(fs::read(root.path.join("b")).unwrap(), b"d");
        fs::create_dir(root.path.join("foreign")).unwrap();
        assert!(check_tree(&root, &expected).is_err());
        fs::remove_dir(root.path.join("foreign")).unwrap();
        fs::set_permissions(root.path.join("b"), fs::Permissions::from_mode(0o600)).unwrap();
        assert!(check_tree(&root, &expected).is_err());
        let interrupted = parent.create("interrupted").unwrap();
        let mut checks = 0;
        assert!(
            write_tree(&interrupted, [("a", &b"0123456789"[..], 0o644)], || {
                checks += 1;
                if checks == 3 {
                    Err("cancelled".into())
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert!(interrupted.path.is_dir());
        assert!(
            !interrupted.path.join("a").exists()
                || fs::read(interrupted.path.join("a")).unwrap().len() <= 10
        );
    }
}
