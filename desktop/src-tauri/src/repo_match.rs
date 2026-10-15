//! Finding the local checkout of a GitHub repository for an open link: a folder whose git
//! remotes (any of them, so a fork with the target as `upstream` counts) point at it.

use std::path::{Path, PathBuf};

use refactor_diff_server::exec::Tools;

use crate::deep_link::RepoSlug;

/// The lowercase `owner/name` of a github.com remote URL, in any of the forms git accepts:
/// `https://github.com/o/n(.git)`, `git@github.com:o/n.git`, `ssh://git@github.com/o/n`.
pub fn github_slug(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let rest = if let Some(rest) = remote.strip_prefix("git@github.com:") {
        rest
    } else {
        let (_, after_scheme) = remote.split_once("://")?;
        // Drop `user@` / `user:token@`, then the host (with an optional port).
        let after_user = after_scheme
            .split_once('@')
            .filter(|(user, _)| !user.contains('/'))
            .map_or(after_scheme, |(_, r)| r);
        let (host, path) = after_user.split_once('/')?;
        let host = host.split(':').next().unwrap_or(host);
        if !host.eq_ignore_ascii_case("github.com") && !host.eq_ignore_ascii_case("www.github.com")
        {
            return None;
        }
        path
    };
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    RepoSlug::parse(rest).map(|s| s.key())
}

/// The GitHub slugs of `repo`'s remotes (empty if it isn't a git repository).
pub fn remote_slugs(tools: &Tools, repo: &Path) -> Vec<String> {
    let output = tools
        .command("git")
        .arg("-C")
        .arg(repo)
        .args(["remote", "-v"])
        .output();
    match output {
        Ok(out) if out.status.success() => parse_remotes(&String::from_utf8_lossy(&out.stdout)),
        _ => Vec::new(),
    }
}

/// `git remote -v` lines (`origin\thttps://github.com/o/n.git (fetch)`) → slugs.
fn parse_remotes(text: &str) -> Vec<String> {
    let mut slugs: Vec<String> = text
        .lines()
        .filter_map(|line| line.split_whitespace().nth(1))
        .filter_map(github_slug)
        .collect();
    slugs.dedup();
    slugs
}

/// Where to look for `slug`, best first: the folder remembered for it, the open repository,
/// the recent ones, then `<reposDir>/<name>` and `<reposDir>/<owner>/<name>`.
pub fn candidates(
    remembered: Option<PathBuf>,
    current: Option<PathBuf>,
    recents: Vec<PathBuf>,
    repos_dir: &Path,
    slug: &RepoSlug,
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let conventional = [
        repos_dir.join(&slug.name),
        repos_dir.join(&slug.owner).join(&slug.name),
    ];
    for p in remembered
        .into_iter()
        .chain(current)
        .chain(recents)
        .chain(conventional)
    {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// The first existing folder in `candidates` with a remote for `slug`.
pub fn find(
    slug: &RepoSlug,
    candidates: &[PathBuf],
    is_dir: impl Fn(&Path) -> bool,
    slugs_of: impl Fn(&Path) -> Vec<String>,
) -> Option<PathBuf> {
    let key = slug.key();
    candidates
        .iter()
        .find(|p| is_dir(p) && slugs_of(p).contains(&key))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_url_forms() {
        for url in [
            "https://github.com/SBayer55/refactor-diff.git",
            "https://github.com/sbayer55/refactor-diff",
            "https://github.com/sbayer55/refactor-diff/",
            "https://user:token@github.com/sbayer55/refactor-diff.git",
            "git@github.com:sbayer55/refactor-diff.git",
            "ssh://git@github.com/sbayer55/refactor-diff.git",
            "ssh://git@github.com:22/sbayer55/refactor-diff",
        ] {
            assert_eq!(
                github_slug(url).as_deref(),
                Some("sbayer55/refactor-diff"),
                "{url}"
            );
        }
        for url in [
            "https://gitlab.com/o/n.git",
            "git@gitlab.com:o/n.git",
            "/local/path/repo",
            "https://github.com/o",
            "https://github.com.evil.com/o/n",
        ] {
            assert_eq!(github_slug(url), None, "{url}");
        }
    }

    #[test]
    fn parses_remote_listing() {
        let text = "origin\tgit@github.com:me/n.git (fetch)\norigin\tgit@github.com:me/n.git (push)\nupstream\thttps://github.com/o/n.git (fetch)\nupstream\thttps://github.com/o/n.git (push)\nlocal\t/tmp/x (fetch)\n";
        assert_eq!(parse_remotes(text), vec!["me/n", "o/n"]);
    }

    #[test]
    fn candidate_order_and_matching() {
        let slug = RepoSlug::parse("o/n").unwrap();
        let list = candidates(
            Some("/remembered".into()),
            Some("/current".into()),
            vec!["/recent".into(), "/current".into()],
            Path::new("/code"),
            &slug,
        );
        let expect: Vec<PathBuf> = ["/remembered", "/current", "/recent", "/code/n", "/code/o/n"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(list, expect);

        let remotes = |p: &Path| match p.to_str().unwrap() {
            "/current" => vec!["other/repo".to_string()],
            "/recent" | "/code/n" => vec!["me/n".to_string(), "o/n".to_string()],
            _ => vec![],
        };
        let exists = |p: &Path| p != Path::new("/remembered");
        assert_eq!(
            find(&slug, &list, exists, remotes),
            Some(PathBuf::from("/recent"))
        );
        assert_eq!(
            find(&RepoSlug::parse("x/y").unwrap(), &list, exists, remotes),
            None
        );
    }
}
