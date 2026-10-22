//! Which git remotes are on a forge (from their URLs)

use meld_ailens::git::RunOptions;
use meld_ailens::{CancelToken, GitRepository};

use crate::error::Result;
use crate::models::{ForgeKind, ForgeRemote};

/// The host and path of a remote URL: `git@host:path`,
/// `ssh://user@host:port/path`, `https://host/path`, without `.git`
pub fn split_url(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let (host, path) = if let Some((_, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        // A port, but not an IPv6 address's colons
        let host = match host.rsplit_once(':') {
            Some((h, port)) if !h.contains(':') && port.chars().all(|c| c.is_ascii_digit()) => h,
            _ => host,
        };
        (host.to_string(), path.to_string())
    } else {
        // scp-like: [user@]host:path, but not a local path like /a:b or C:\x
        let (authority, path) = url.split_once(':')?;
        if authority.contains('/') || authority.len() <= 1 {
            return None;
        }
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        (host.to_string(), path.to_string())
    };
    let host = host.to_lowercase();
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path).trim_matches('/');
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some((host, path.to_string()))
}

/// The forge hosting a remote, if Meld can talk to it: github.com (and
/// GitHub Enterprise hosts named `github.*`) through `gh`, hosts with
/// `gitlab` in their name through `glab`
pub fn parse_remote(name: &str, url: &str) -> Option<ForgeRemote> {
    let (host, path) = split_url(url)?;
    let host = match host.as_str() {
        // SSH over the HTTPS port
        "ssh.github.com" => "github.com".to_string(),
        "altssh.gitlab.com" => "gitlab.com".to_string(),
        _ => host,
    };
    let first = host.split('.').next().unwrap_or_default();
    let kind = if host == "github.com" || first == "github" {
        ForgeKind::GitHub
    } else if host.split('.').any(|part| part == "gitlab") || first.contains("gitlab") {
        ForgeKind::GitLab
    } else {
        return None;
    };
    let segments = path.split('/').filter(|s| !s.is_empty()).count();
    if segments < 2 || (kind == ForgeKind::GitHub && segments != 2) {
        return None;
    }
    Some(ForgeRemote {
        name: name.to_string(),
        url: url.to_string(),
        kind,
        host,
        path,
    })
}

/// `(name, url)` of every remote, in the order of the configuration
///
/// Read from the configuration rather than `git remote -v`, which applies
/// `url.<base>.insteadOf` rewrites: the URL as written says which forge it
/// is.
pub fn remote_urls(repo: &GitRepository, cancel: &CancelToken) -> Result<Vec<(String, String)>> {
    let result = repo.run_with(
        &["config", "--get-regexp", r"^remote\..*\.url$"],
        RunOptions {
            ok: &[0, 1],
            ..Default::default()
        },
        cancel,
    )?;
    let text = String::from_utf8_lossy(&result.stdout);
    Ok(parse_config_urls(&text))
}

fn parse_config_urls(text: &str) -> Vec<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    text.lines()
        .filter_map(|line| {
            let (key, url) = line.split_once(' ')?;
            let name = key.strip_prefix("remote.")?.strip_suffix(".url")?;
            // A remote can have several URLs; the first one fetches
            seen.insert(name.to_string())
                .then(|| (name.to_string(), url.trim().to_string()))
        })
        .collect()
}

/// The repository's remotes on forges Meld can talk to
pub fn forge_remotes(repo: &GitRepository, cancel: &CancelToken) -> Result<Vec<ForgeRemote>> {
    Ok(remote_urls(repo, cancel)?
        .into_iter()
        .filter_map(|(name, url)| parse_remote(&name, &url))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_split_into_host_and_path() {
        let cases = [
            (
                "git@github.com:melandur/meld.git",
                Some(("github.com", "melandur/meld")),
            ),
            (
                "https://gitlab.gnome.org/GNOME/meld.git",
                Some(("gitlab.gnome.org", "GNOME/meld")),
            ),
            (
                "ssh://git@gitlab.example.com:2222/group/sub/proj.git/",
                Some(("gitlab.example.com", "group/sub/proj")),
            ),
            (
                "https://user:token@GitHub.com/o/r",
                Some(("github.com", "o/r")),
            ),
            ("/srv/git/repo.git", None),
            ("../relative", None),
            ("C:\\repo", None),
            ("https://github.com/", None),
        ];
        for (url, expected) in cases {
            let got = split_url(url);
            let expected = expected.map(|(h, p)| (h.to_string(), p.to_string()));
            assert_eq!(got, expected, "{url}");
        }
    }

    #[test]
    fn hosts_pick_the_forge() {
        let github = parse_remote("github", "git@github.com:melandur/meld.git").unwrap();
        assert_eq!(github.kind, ForgeKind::GitHub);
        assert_eq!(github.path, "melandur/meld");
        let gitlab = parse_remote("origin", "https://gitlab.gnome.org/GNOME/meld.git").unwrap();
        assert_eq!(gitlab.kind, ForgeKind::GitLab);
        assert_eq!(gitlab.host, "gitlab.gnome.org");
        assert_eq!(
            parse_remote("x", "ssh://git@ssh.github.com:443/o/r.git")
                .unwrap()
                .host,
            "github.com"
        );
        assert_eq!(
            parse_remote("x", "https://github.example.com/o/r")
                .unwrap()
                .kind,
            ForgeKind::GitHub
        );
        assert_eq!(
            parse_remote("x", "https://gitlab.com/a/b/c").unwrap().path,
            "a/b/c"
        );
        // GitHub repositories are always owner/name
        assert_eq!(parse_remote("x", "https://github.com/a/b/c"), None);
        assert_eq!(parse_remote("x", "https://bitbucket.org/a/b"), None);
        assert_eq!(parse_remote("x", "/local/path"), None);
    }

    #[test]
    fn config_urls_keep_their_order_and_first_url() {
        let text = "remote.origin.url https://gitlab.gnome.org/GNOME/meld.git\n\
                    remote.github.url git@github.com:melandur/meld.git\n\
                    remote.github.url https://example.com/second\n\
                    remote.my.fork.url git@github.com:me/fork.git\n";
        assert_eq!(
            parse_config_urls(text),
            [
                (
                    "origin".to_string(),
                    "https://gitlab.gnome.org/GNOME/meld.git".to_string()
                ),
                (
                    "github".to_string(),
                    "git@github.com:melandur/meld.git".to_string()
                ),
                (
                    "my.fork".to_string(),
                    "git@github.com:me/fork.git".to_string()
                ),
            ]
        );
    }
}
