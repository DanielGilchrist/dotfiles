use std::fs::DirBuilder;
use std::io;
use std::num::NonZeroUsize;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use agent_core::names::SessionName;
use nix::unistd::Uid;

const SOCKET_SUFFIX: &str = ".sock";

/// The most bytes of a socket path the platform's `sockaddr_un` holds, with
/// its terminating NUL.
const SUN_PATH: usize = if cfg!(any(target_os = "linux", target_os = "android")) {
    108
} else {
    104
};

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("the runtime directory {path} {problem}")]
    Unfit { path: PathBuf, problem: Unfit },
    #[error("AGENT_RUNTIME_DIR is {0}, which isn't an absolute path")]
    Relative(PathBuf),
    #[error("the runtime directory {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Unfit {
    #[error("is a symbolic link")]
    Symlink,
    #[error("is not a directory")]
    NotDirectory,
    #[error("belongs to another user")]
    NotOurs,
    #[error("doesn't exist")]
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the socket path {path} is {len} bytes, and this platform allows {max}")]
pub struct SocketPathTooLong {
    pub path: PathBuf,
    pub len: usize,
    pub max: usize,
}

/// Where a runtime directory is wanted: `AGENT_RUNTIME_DIR` as given, else
/// `$XDG_RUNTIME_DIR/agent`, else `/tmp/agent-<uid>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeDir(PathBuf);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Creation {
    Create,
    Existing,
}

impl RuntimeDir {
    /// A relative `AGENT_RUNTIME_DIR` is an error, since it would name a
    /// different directory in every process. A relative `XDG_RUNTIME_DIR` is
    /// ignored, as the XDG specification says.
    pub fn from_env() -> Result<Self, RuntimeError> {
        Self::from_vars(
            std::env::var_os("AGENT_RUNTIME_DIR"),
            std::env::var_os("XDG_RUNTIME_DIR"),
        )
    }

    fn from_vars(
        explicit: Option<std::ffi::OsString>,
        xdg: Option<std::ffi::OsString>,
    ) -> Result<Self, RuntimeError> {
        let explicit = explicit.map(PathBuf::from);
        if let Some(relative) = explicit.as_ref().filter(|path| !path.is_absolute()) {
            return Err(RuntimeError::Relative(relative.clone()));
        }
        let xdg = xdg.map(PathBuf::from).filter(|path| path.is_absolute());
        Ok(Self::locate(explicit, xdg, Uid::current()))
    }

    pub fn locate(explicit: Option<PathBuf>, xdg: Option<PathBuf>, uid: Uid) -> Self {
        Self(match (explicit, xdg) {
            (Some(explicit), _) => explicit,
            (None, Some(xdg)) => xdg.join("agent"),
            (None, None) => PathBuf::from(format!("/tmp/agent-{uid}")),
        })
    }

    /// The longest session name whose socket path still fits `sockaddr_un`
    /// under this directory, at least 1 so a name is never "limited" to
    /// none. A name over it is refused by [`Runtime::socket`] anyway.
    pub fn name_budget(&self) -> NonZeroUsize {
        let fixed = self.0.as_os_str().len() + "/".len() + SOCKET_SUFFIX.len();
        NonZeroUsize::new((SUN_PATH - 1).saturating_sub(fixed)).unwrap_or(NonZeroUsize::MIN)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// The directory as a [`Runtime`], created 0700 when `creation` says so.
    /// What exists must be a real directory of ours, and its mode is
    /// tightened to 0700.
    pub fn open(&self, creation: Creation) -> Result<Runtime, RuntimeError> {
        let io = |source| RuntimeError::Io {
            path: self.0.clone(),
            source,
        };
        if creation == Creation::Create {
            if let Some(parent) = self.0.parent() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
            match DirBuilder::new().mode(0o700).create(&self.0) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io(e)),
            }
        }
        let unfit = |problem| RuntimeError::Unfit {
            path: self.0.clone(),
            problem,
        };
        let meta = match std::fs::symlink_metadata(&self.0) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(unfit(Unfit::Missing)),
            Err(e) => return Err(io(e)),
        };
        if meta.file_type().is_symlink() {
            return Err(unfit(Unfit::Symlink));
        }
        if !meta.is_dir() {
            return Err(unfit(Unfit::NotDirectory));
        }
        if meta.uid() != Uid::current().as_raw() {
            return Err(unfit(Unfit::NotOurs));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700))
                .map_err(io)?;
        }
        Ok(Runtime(self.0.clone()))
    }
}

/// A runtime directory that was checked to be ours and not a link. Only
/// [`RuntimeDir::open`] makes one, so a socket path is only built under one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtime(PathBuf);

impl Runtime {
    pub fn socket(&self, name: &SessionName) -> Result<SocketPath, SocketPathTooLong> {
        SocketPath::new(self.0.join(format!("{name}{SOCKET_SUFFIX}")))
    }
}

/// A unix socket path that fits `sockaddr_un`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocketPath(PathBuf);

impl SocketPath {
    fn new(path: PathBuf) -> Result<Self, SocketPathTooLong> {
        let len = path.as_os_str().len();
        if len >= SUN_PATH {
            return Err(SocketPathTooLong {
                path,
                len,
                max: SUN_PATH - 1,
            });
        }
        Ok(Self(path))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// Where a host keeps its record and log: `<state>/hosts/<name>.json` and
/// `.log`. Not in the runtime directory, which a reboot or `tmp_cleaner`
/// empties while a record must outlive both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFiles(PathBuf);

impl HostFiles {
    pub fn under(state: &Path) -> Self {
        Self(state.join("hosts"))
    }

    pub fn dir(&self) -> &Path {
        &self.0
    }

    pub fn create_dir(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.0)
    }

    pub fn record(&self, name: &SessionName) -> PathBuf {
        self.0.join(format!("{name}.json"))
    }

    pub fn log(&self, name: &SessionName) -> PathBuf {
        self.0.join(format!("{name}.log"))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    fn session(name: &str) -> SessionName {
        name.parse().expect("a session name")
    }

    #[test]
    fn the_name_budget_is_the_longest_name_whose_socket_still_fits() {
        let uid = Uid::current();
        for dir in [
            "/tmp/a",
            "/tmp/agent-501",
            "/var/folders/zz/abcdefghijklmnopqrstuvwxyz/T/agent",
        ] {
            let located = RuntimeDir::locate(Some(dir.into()), None, uid);
            let budget = located.name_budget().get();
            let runtime = Runtime(PathBuf::from(dir));
            assert!(
                runtime.socket(&session(&"n".repeat(budget))).is_ok(),
                "{dir}"
            );
            assert!(
                runtime.socket(&session(&"n".repeat(budget + 1))).is_err(),
                "{dir}"
            );
        }
    }

    #[test]
    fn a_directory_with_no_room_for_a_name_still_has_a_budget_of_one() {
        let long = format!("/{}", "d".repeat(200));
        let located = RuntimeDir::locate(Some(long.into()), None, Uid::current());
        assert_eq!(located.name_budget().get(), 1);
    }

    #[test]
    fn the_override_wins_then_xdg_then_tmp() {
        let uid = Uid::from_raw(501);
        let located = |explicit: Option<&str>, xdg: Option<&str>| {
            RuntimeDir::locate(explicit.map(Into::into), xdg.map(Into::into), uid)
                .path()
                .to_path_buf()
        };
        assert_eq!(
            located(Some("/run/x"), Some("/run/user/1")),
            Path::new("/run/x")
        );
        assert_eq!(
            located(None, Some("/run/user/1")),
            Path::new("/run/user/1/agent")
        );
        assert_eq!(located(None, None), Path::new("/tmp/agent-501"));
    }

    #[test]
    fn a_relative_override_is_an_error_and_not_a_silent_fallback() {
        let error = RuntimeDir::from_vars(Some("rel/dir".into()), Some("/run/user/1".into()))
            .expect_err("refused");
        assert!(matches!(error, RuntimeError::Relative(path) if path == Path::new("rel/dir")));
    }

    #[test]
    fn a_relative_xdg_directory_is_ignored() {
        let dir = RuntimeDir::from_vars(None, Some("run".into())).expect("falls back");
        assert!(dir.path().starts_with("/tmp"));
    }

    #[test]
    fn a_new_runtime_directory_is_private() {
        let base = tempfile::tempdir().expect("tempdir");
        let dir = RuntimeDir::locate(Some(base.path().join("rt")), None, Uid::current());
        dir.open(Creation::Create).expect("opens");
        let mode = std::fs::metadata(dir.path()).expect("stat").mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn a_loose_directory_of_ours_is_tightened() {
        let base = tempfile::tempdir().expect("tempdir");
        let path = base.path().join("rt");
        std::fs::create_dir(&path).expect("mkdir");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        RuntimeDir::locate(Some(path.clone()), None, Uid::current())
            .open(Creation::Existing)
            .expect("opens");
        assert_eq!(
            std::fs::metadata(&path).expect("stat").mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn a_symlink_is_refused_even_to_a_directory_of_ours() {
        let base = tempfile::tempdir().expect("tempdir");
        let real = base.path().join("real");
        std::fs::create_dir(&real).expect("mkdir");
        let link = base.path().join("link");
        symlink(&real, &link).expect("symlink");
        let error = RuntimeDir::locate(Some(link), None, Uid::current())
            .open(Creation::Create)
            .expect_err("refused");
        assert!(matches!(
            error,
            RuntimeError::Unfit {
                problem: Unfit::Symlink,
                ..
            }
        ));
    }

    #[test]
    fn a_directory_owned_by_someone_else_is_refused() {
        let error = RuntimeDir::locate(Some("/".into()), None, Uid::current())
            .open(Creation::Existing)
            .expect_err("refused");
        let owned_by_us = Uid::current().is_root();
        assert_eq!(
            matches!(
                error,
                RuntimeError::Unfit {
                    problem: Unfit::NotOurs,
                    ..
                }
            ),
            !owned_by_us,
            "{error}"
        );
    }

    #[test]
    fn a_file_is_not_a_runtime_directory() {
        let base = tempfile::tempdir().expect("tempdir");
        let path = base.path().join("file");
        std::fs::write(&path, "x").expect("write");
        let error = RuntimeDir::locate(Some(path), None, Uid::current())
            .open(Creation::Existing)
            .expect_err("refused");
        assert!(matches!(
            error,
            RuntimeError::Unfit {
                problem: Unfit::NotDirectory,
                ..
            }
        ));
    }

    #[test]
    fn a_missing_directory_is_not_created_for_a_client() {
        let base = tempfile::tempdir().expect("tempdir");
        let error = RuntimeDir::locate(Some(base.path().join("none")), None, Uid::current())
            .open(Creation::Existing)
            .expect_err("refused");
        assert!(matches!(
            error,
            RuntimeError::Unfit {
                problem: Unfit::Missing,
                ..
            }
        ));
    }

    fn runtime_with_length(total: usize) -> (tempfile::TempDir, Runtime, SessionName) {
        let base = tempfile::tempdir().expect("tempdir");
        let runtime = RuntimeDir::locate(Some(base.path().to_path_buf()), None, Uid::current())
            .open(Creation::Existing)
            .expect("opens");
        let fixed = base.path().as_os_str().len() + "/".len() + ".sock".len();
        let name = "n".repeat(total - fixed);
        (base, runtime, session(&name))
    }

    #[test]
    fn a_socket_path_one_byte_over_the_platform_limit_is_a_typed_error() {
        let (_base, runtime, long) = runtime_with_length(SUN_PATH);
        let error = runtime.socket(&long).expect_err("too long");
        assert_eq!(error.len, SUN_PATH);
        assert_eq!(error.max, SUN_PATH - 1);
    }

    #[test]
    fn a_socket_path_at_the_limit_binds_and_the_next_byte_would_not() {
        let (_base, runtime, name) = runtime_with_length(SUN_PATH - 1);
        let path = runtime.socket(&name).expect("fits");
        std::os::unix::net::UnixListener::bind(path.as_path())
            .expect("the operating system accepts what the type allows");
    }
}
