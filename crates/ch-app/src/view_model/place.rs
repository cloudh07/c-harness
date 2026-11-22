//! Where a session works: the machine, the repository and the folder the agent writes in.

use std::path::Path;

use super::{first_chars, last_chars};
use crate::settings::{Location, Settings};

/// The machine a session runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sandbox {
    /// This computer, in a worktree C-Harness makes for the session.
    Local,
    /// A host reached over SSH, in a checkout prepared there.
    Ssh {
        host: String,
        port: Option<u16>,
        workdir: String,
    },
}

/// Where a session works, read from Settings when it starts, so changing Settings later does not
/// relabel a session that is already running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// The repository's folder name.
    pub repo: String,
    pub sandbox: Sandbox,
}

impl Place {
    pub fn of(settings: &Settings) -> Self {
        match &settings.location {
            Location::Local => Self {
                repo: last_folder(&settings.repo),
                sandbox: Sandbox::Local,
            },
            Location::Ssh {
                host,
                port,
                remote_workdir,
                ..
            } => Self {
                repo: last_folder(remote_workdir),
                sandbox: Sandbox::Ssh {
                    host: host.trim().to_string(),
                    port: *port,
                    workdir: remote_workdir.trim().to_string(),
                },
            },
        }
    }

    /// "local", or the SSH host with the port Settings names.
    pub fn sandbox_label(&self) -> String {
        match &self.sandbox {
            Sandbox::Local => "local".to_owned(),
            Sandbox::Ssh {
                host, port: None, ..
            } => host.clone(),
            Sandbox::Ssh {
                host,
                port: Some(port),
                ..
            } if host.contains(':') => format!("[{host}]:{port}"),
            Sandbox::Ssh {
                host,
                port: Some(port),
                ..
            } => format!("{host}:{port}"),
        }
    }

    /// The folder the agent writes in: the session's worktree on this computer, once it exists,
    /// or the checkout on the SSH host.
    pub fn folder(&self, worktree: Option<&Path>) -> Option<String> {
        match &self.sandbox {
            Sandbox::Local => worktree.map(|path| path.display().to_string()),
            Sandbox::Ssh { workdir, .. } => (!workdir.is_empty()).then(|| workdir.clone()),
        }
    }
}

fn last_folder(path: &str) -> String {
    path.trim()
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// `text` cut to `max` characters by dropping its middle, which keeps a path's drive and its
/// last folders.
pub fn elide_middle(text: &str, max: usize) -> String {
    if !first_chars(text, max).1 {
        return text.to_owned();
    }
    let keep = max.saturating_sub(1);
    let tail = keep * 2 / 3;
    format!(
        "{}…{}",
        first_chars(text, keep - tail).0,
        last_chars(text, tail).0
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn ssh(host: &str, port: Option<u16>, workdir: &str) -> Settings {
        Settings {
            location: Location::Ssh {
                host: host.to_string(),
                port,
                remote_workdir: workdir.to_string(),
                remote_runner: "ch-runner".to_string(),
            },
            ..Settings::default()
        }
    }

    #[test]
    fn a_local_session_is_named_by_its_repository_folder() {
        let windows = Settings {
            repo: "D:\\code\\c-harness\\".to_string(),
            ..Settings::default()
        };
        assert_eq!(
            Place::of(&windows),
            Place {
                repo: "c-harness".to_string(),
                sandbox: Sandbox::Local,
            }
        );
        let unix = Settings {
            repo: " /home/dev/work/app/ ".to_string(),
            ..Settings::default()
        };
        assert_eq!(Place::of(&unix).repo, "app");
    }

    #[test]
    fn an_ssh_session_is_named_by_its_remote_checkout_and_keeps_the_host() {
        let place = Place::of(&ssh(" devbox ", Some(2222), " /srv/app/ "));
        assert_eq!(
            place,
            Place {
                repo: "app".to_string(),
                sandbox: Sandbox::Ssh {
                    host: "devbox".to_string(),
                    port: Some(2222),
                    workdir: "/srv/app/".to_string(),
                },
            }
        );
    }

    #[test]
    fn the_sandbox_label_says_local_or_names_the_host_and_port() {
        let local = Place::of(&Settings::default());
        assert_eq!(local.sandbox_label(), "local");
        assert_eq!(
            Place::of(&ssh("devbox", None, "/srv/app")).sandbox_label(),
            "devbox"
        );
        assert_eq!(
            Place::of(&ssh("devbox", Some(2222), "/srv/app")).sandbox_label(),
            "devbox:2222"
        );
        assert_eq!(
            Place::of(&ssh("::1", Some(2222), "/srv/app")).sandbox_label(),
            "[::1]:2222",
            "an IPv6 address keeps its port apart"
        );
    }

    #[test]
    fn the_folder_is_the_local_worktree_once_known_or_the_remote_checkout() {
        let local = Place::of(&Settings::default());
        assert_eq!(
            local.folder(None),
            None,
            "no worktree before the runner starts"
        );
        let worktree = PathBuf::from("worktrees").join("s1");
        assert_eq!(
            local.folder(Some(&worktree)),
            Some(worktree.display().to_string())
        );
        let remote = Place::of(&ssh("devbox", None, " /srv/app "));
        assert_eq!(
            remote.folder(Some(&worktree)),
            Some("/srv/app".to_string()),
            "an SSH session works in the checkout itself"
        );
        assert_eq!(Place::of(&ssh("devbox", None, " ")).folder(None), None);
    }

    #[test]
    fn a_short_path_is_shown_whole() {
        assert_eq!(elide_middle("/srv/app", 20), "/srv/app");
        assert_eq!(elide_middle("", 20), "");
    }

    #[test]
    fn a_long_path_keeps_its_start_and_more_of_its_end() {
        let path = "C:\\Users\\dev\\AppData\\Local\\c-harness\\worktrees\\s1732000000000";
        let shown = elide_middle(path, 40);
        assert_eq!(shown.chars().count(), 40);
        assert!(shown.starts_with("C:\\Users\\"), "{shown}");
        assert!(shown.ends_with("worktrees\\s1732000000000"), "{shown}");
        assert_eq!(shown.matches('…').count(), 1, "{shown}");
    }

    #[test]
    fn eliding_counts_characters_not_bytes() {
        let path = "/home/người-dùng/dự-án/thư-mục/phiên-làm-việc";
        let shown = elide_middle(path, 16);
        assert_eq!(shown.chars().count(), 16);
        assert!(shown.ends_with("làm-việc"), "{shown}");
    }
}
