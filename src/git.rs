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
    let mut ahead = 0u32;
    let mut behind = 0u32;
    // left-right counts via rev_walk
    let platform = repo
        .rev_walk([head_oid.detach(), upstream_oid.detach()])
        .sorting(gix::revision::walk::Sorting::BreadthFirst);
    // Simpler: compute merge base, then count commits on each side.
    let base = repo
        .merge_base(head_oid.detach(), upstream_oid.detach())
        .ok()?;
    for info in repo.rev_walk([head_oid.detach()]).all().ok()? {
        let info = info.ok()?;
        if info.id == base {
            break;
        }
        ahead += 1;
    }
    for info in repo.rev_walk([upstream_oid.detach()]).all().ok()? {
        let info = info.ok()?;
        if info.id == base {
            break;
        }
        behind += 1;
    }
    let _ = platform;
    Some((ahead, behind))
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
