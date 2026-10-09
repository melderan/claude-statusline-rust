// ─────────────────────────────────────────────────────────────────────
// Git info via gix (pure Rust)
// ─────────────────────────────────────────────────────────────────────

pub(crate) struct GitInfo {
    pub(crate) branch: String,
    pub(crate) age_secs: Option<i64>,
    pub(crate) ahead: u32,
    pub(crate) behind: u32,
    pub(crate) dirty: bool,
}

pub(crate) fn git_info(path: &str) -> Option<GitInfo> {
    let repo = gix::discover(path).ok()?;

    let head = repo.head().ok()?;
    let branch = match head.referent_name() {
        Some(n) => n.shorten().to_string(),
        None => "detached".to_string(),
    };

    let head_commit = repo.head_commit().ok();
    let age_secs = head_commit.as_ref().and_then(|c| {
        let t = c.time().ok()?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64;
        Some(now - t.seconds)
    });

    let (ahead, behind) = ahead_behind(&repo).unwrap_or((0, 0));
    let dirty = is_dirty(&repo).unwrap_or(false);

    Some(GitInfo {
        branch,
        age_secs,
        ahead,
        behind,
        dirty,
    })
}

pub(crate) fn ahead_behind(repo: &gix::Repository) -> Option<(u32, u32)> {
    let head = repo.head().ok()?;
    let head_ref = head.try_into_referent()?;
    let head_oid = head_ref.id();
    let upstream = head_ref
        .remote_tracking_ref_name(gix::remote::Direction::Fetch)?
        .ok()?;
    let upstream_ref = repo.find_reference(upstream.as_ref()).ok()?;
    let upstream_oid = upstream_ref.id();
    // The same counts as `git rev-list --left-right --count HEAD...@{u}`:
    // commits reachable from one tip and not from the other. Hiding the other
    // tip's ancestors is what makes this right across merge commits; a walk
    // that stops when it first meets the merge base counts commits older than
    // the base that the walk reaches first on another parent.
    let ahead = count_only(repo, head_oid.detach(), upstream_oid.detach())?;
    let behind = count_only(repo, upstream_oid.detach(), head_oid.detach())?;
    Some((ahead, behind))
}

/// Commits reachable from `tip` and not from `hidden`. None on any walk error.
pub(crate) fn count_only(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    hidden: gix::ObjectId,
) -> Option<u32> {
    let mut n = 0u32;
    for info in repo.rev_walk([tip]).with_hidden([hidden]).all().ok()? {
        info.ok()?;
        n += 1;
    }
    Some(n)
}

pub(crate) fn is_dirty(repo: &gix::Repository) -> Option<bool> {
    // gix::status returns an iterator of changes; any item means dirty.
    let platform = repo
        .status(gix::progress::Discard)
        .ok()?
        .index_worktree_submodules(gix::status::Submodule::AsConfigured { check_dirty: false });
    let mut iter = platform.into_iter(None).ok()?;
    Some(iter.next().is_some())
}
