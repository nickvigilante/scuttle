//! Pull request references in each forge's own form, such as `owner/repo#123` on GitHub or
//! `group/project!123` on GitLab, read from the pull request's URL. The chat's diff status
//! names no repository, so its URL is parsed the way the server's git providers parse it
//! (`ParsePullRequestURL` in `coderd/externalauth/gitprovider`).

/// The forge that hosts a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forge {
    GitHub,
    GitLab,
    /// Gitea and Forgejo, which share Gitea's URLs.
    Gitea,
    /// Bitbucket Cloud.
    Bitbucket,
    AzureDevOps,
}

impl Forge {
    /// What comes between the repository and the number: `!` where `#` names an issue or a
    /// work item (GitLab merge requests, Azure DevOps pull requests), else `#`.
    pub fn sigil(self) -> char {
        match self {
            Forge::GitLab | Forge::AzureDevOps => '!',
            Forge::GitHub | Forge::Gitea | Forge::Bitbucket => '#',
        }
    }
}

/// A pull request its URL names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRef {
    pub forge: Forge,
    /// What the repository sits under: a GitHub or Gitea owner, a GitLab group path with its
    /// subgroups, a Bitbucket workspace, or an Azure DevOps project.
    pub owner: String,
    pub repo: String,
    pub number: i64,
}

impl PrRef {
    /// The reference in at most `max` cells: `owner/repo#N` when it fits, then `repo#N`, then
    /// the repository cut short with an ellipsis, then `#N` (or `!N`), which is never cut.
    pub fn text(&self, max: usize) -> String {
        let tail = format!("{}{}", self.forge.sigil(), self.number);
        let full = format!("{}/{}{tail}", self.owner, self.repo);
        if full.len() <= max {
            return full;
        }
        let short = format!("{}{tail}", self.repo);
        if short.len() <= max {
            return short;
        }
        // At least one character of the repository before the ellipsis, or nothing of it.
        let room = max.saturating_sub(tail.len() + 1);
        if room == 0 {
            return tail;
        }
        format!("{}\u{2026}{tail}", &self.repo[..room])
    }
}

/// Whether `s` is a path segment the parser accepts: the characters the server's GitHub
/// pattern allows, all ASCII, so a segment's length is its width in cells.
fn segment(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// A pull request number: digits only, more than zero.
fn number(s: &str) -> Option<i64> {
    if !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok().filter(|n| *n > 0)
}

/// The pull request `url` names, or `None` when no forge's form matches. The host picks the
/// forge first (github.com, gitlab.com, bitbucket.org, dev.azure.com); on any other host the
/// path's shape does, so self-hosted GitLab (`/-/merge_requests/N`), Gitea and Forgejo
/// (`/pulls/N`), GitHub Enterprise (`/pull/N`), and Azure DevOps Server
/// (`/_git/repo/pullrequest/N`) are read too.
pub fn parse_pr_url(url: &str) -> Option<PrRef> {
    let rest = url.trim();
    let rest = rest
        .strip_prefix("https://")
        .or_else(|| rest.strip_prefix("http://"))?;
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let (authority, path) = rest.split_once('/')?;
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host)
        .to_ascii_lowercase();
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if !parts.iter().all(|p| segment(p)) {
        return None;
    }
    let pr = |forge, owner: &str, repo: &str, n: &str| {
        Some(PrRef {
            forge,
            owner: owner.to_owned(),
            repo: repo.to_owned(),
            number: number(n)?,
        })
    };
    let github = |parts: &[&str]| match parts {
        [owner, repo, "pull", n, ..] => pr(Forge::GitHub, owner, repo, n),
        _ => None,
    };
    // `group[/subgroup...]/project/-/merge_requests/N`, as `splitOwnerRepo` splits it.
    let gitlab = |parts: &[&str]| {
        let at = parts.iter().position(|p| *p == "-")?;
        let (project, marker) = parts.split_at(at);
        match (project, marker) {
            ([groups @ .., repo], ["-", "merge_requests", n, ..]) if !groups.is_empty() => {
                pr(Forge::GitLab, &groups.join("/"), repo, n)
            }
            _ => None,
        }
    };
    let gitea = |parts: &[&str]| match parts {
        [owner, repo, "pulls", n, ..] => pr(Forge::Gitea, owner, repo, n),
        _ => None,
    };
    let bitbucket = |parts: &[&str]| match parts {
        [workspace, repo, "pull-requests", n, ..] => pr(Forge::Bitbucket, workspace, repo, n),
        _ => None,
    };
    // `org/project/_git/repo/pullrequest/N`; a server install puts a collection first.
    let azure = |parts: &[&str]| match parts {
        [_, project, "_git", repo, "pullrequest", n, ..] => {
            pr(Forge::AzureDevOps, project, repo, n)
        }
        _ => None,
    };
    match host.as_str() {
        "github.com" => github(&parts),
        "gitlab.com" => gitlab(&parts),
        "bitbucket.org" => bitbucket(&parts),
        "dev.azure.com" => azure(&parts),
        _ => gitlab(&parts)
            .or_else(|| gitea(&parts))
            .or_else(|| azure(&parts))
            .or_else(|| github(&parts)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(forge: Forge, owner: &str, repo: &str, number: i64) -> Option<PrRef> {
        Some(PrRef {
            forge,
            owner: owner.into(),
            repo: repo.into(),
            number,
        })
    }

    #[test]
    fn each_forge_url_parses_to_its_repository_and_number() {
        for (url, want) in [
            (
                "https://github.com/coder/coder/pull/30498",
                pr(Forge::GitHub, "coder", "coder", 30498),
            ),
            (
                "https://github.com/coder/coder/pull/12/files?w=1#diff",
                pr(Forge::GitHub, "coder", "coder", 12),
            ),
            (
                "https://GitHub.com/coder/coder/pull/12",
                pr(Forge::GitHub, "coder", "coder", 12),
            ),
            (
                "https://github.example.com/coder/coder/pull/7",
                pr(Forge::GitHub, "coder", "coder", 7),
            ),
            (
                "https://gitlab.com/gitlab-org/gitlab/-/merge_requests/123",
                pr(Forge::GitLab, "gitlab-org", "gitlab", 123),
            ),
            (
                "https://git.example.com/group/sub/deeper/project/-/merge_requests/4/diffs",
                pr(Forge::GitLab, "group/sub/deeper", "project", 4),
            ),
            (
                "https://gitea.com/gitea/tea/pulls/5",
                pr(Forge::Gitea, "gitea", "tea", 5),
            ),
            (
                "https://codeberg.org/forgejo/forgejo/pulls/6/files",
                pr(Forge::Gitea, "forgejo", "forgejo", 6),
            ),
            (
                "https://bitbucket.org/atlassian/python-bitbucket/pull-requests/8/overview",
                pr(Forge::Bitbucket, "atlassian", "python-bitbucket", 8),
            ),
            (
                "https://dev.azure.com/contoso/Fabrikam/_git/web/pullrequest/9",
                pr(Forge::AzureDevOps, "Fabrikam", "web", 9),
            ),
            (
                "https://tfs.example.com/Collection/Fabrikam/_git/web/pullrequest/10",
                pr(Forge::AzureDevOps, "Fabrikam", "web", 10),
            ),
            (
                "http://localhost:3000/me/repo/pulls/11",
                pr(Forge::Gitea, "me", "repo", 11),
            ),
        ] {
            assert_eq!(parse_pr_url(url), want, "{url}");
        }
    }

    #[test]
    fn an_unrecognized_url_parses_to_nothing() {
        for url in [
            "",
            "not a url",
            "https://github.com/coder/coder/issues/12",
            "https://github.com/coder/coder/pull/0",
            "https://github.com/coder/coder/pull/12a",
            "https://github.com/coder/pull/12",
            "https://gitlab.com/-/merge_requests/3",
            "https://gitlab.com/project/-/merge_requests/3",
            "https://bitbucket.org/ws/repo/pull/3",
            "https://dev.azure.com/org/project/_git/repo/pullrequests",
            "https://example.com/some/where",
            "https://github.com/c%20o/coder/pull/1",
            "ftp://github.com/coder/coder/pull/1",
            "https://github.com/coder/coder/pull/99999999999999999999",
        ] {
            assert_eq!(parse_pr_url(url), None, "{url}");
        }
    }

    #[test]
    fn each_forge_writes_its_own_reference() {
        let full = usize::MAX;
        let text = |url: &str| parse_pr_url(url).unwrap().text(full);
        assert_eq!(
            text("https://github.com/coder/coder/pull/30498"),
            "coder/coder#30498"
        );
        assert_eq!(text("https://gitea.com/gitea/tea/pulls/5"), "gitea/tea#5");
        assert_eq!(
            text("https://gitlab.example.com/group/sub/project/-/merge_requests/4"),
            "group/sub/project!4",
            "a merge request is !, and nested groups stay"
        );
        assert_eq!(
            text("https://bitbucket.org/ws/repo/pull-requests/8"),
            "ws/repo#8"
        );
        assert_eq!(
            text("https://dev.azure.com/org/proj/_git/repo/pullrequest/9"),
            "proj/repo!9",
            "Azure DevOps marks a pull request with !, as # is a work item"
        );
    }

    #[test]
    fn a_tight_reference_drops_the_owner_then_cuts_the_repository() {
        let r = parse_pr_url("https://github.com/nickvigilante/scuttle/pull/5").unwrap();
        assert_eq!(r.text(23), "nickvigilante/scuttle#5");
        assert_eq!(r.text(22), "scuttle#5", "the owner goes first");
        assert_eq!(r.text(9), "scuttle#5");
        assert_eq!(r.text(8), "scutt\u{2026}#5", "then the repository is cut");
        assert_eq!(r.text(4), "s\u{2026}#5");
        assert_eq!(r.text(3), "#5", "the number is never cut");
        assert_eq!(r.text(0), "#5");
        let mr = parse_pr_url("https://gitlab.com/a/b/project/-/merge_requests/4").unwrap();
        assert_eq!(
            mr.text(10),
            "project!4",
            "the whole group path goes at once"
        );
        assert_eq!(mr.text(2), "!4", "the bare number keeps its sigil");
    }
}
