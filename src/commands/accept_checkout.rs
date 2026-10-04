//! Local allocation proof for binding and population recovery.
use super::*;
use serde::{Deserialize, Serialize};

pub const CHECKOUT_RECOVERY_SCHEMA: &str = "arc-accept-checkout/1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Allocation {
    pub schema: String,
    pub repository_id: String,
    pub change_id: String,
    pub branch: String,
    pub worktree: PathBuf,
    pub administrative_dir: PathBuf,
    pub head: String,
    pub owner_token: String,
}

impl Allocation {
    pub fn path(&self) -> PathBuf {
        self.administrative_dir.join("accept-checkout.json")
    }

    pub fn record(
        ctx: &Ctx,
        store: &Store,
        change: &str,
        branch: &str,
        worktree: &Path,
        head: &str,
        token: String,
    ) -> Result<Self> {
        let allocation = Self {
            schema: CHECKOUT_RECOVERY_SCHEMA.into(),
            repository_id: store.repository_id.clone(),
            change_id: change.into(),
            branch: branch.into(),
            worktree: config::resolved(worktree),
            administrative_dir: gitio::worktree_admin_dir(worktree)?,
            head: head.into(),
            owner_token: token,
        };
        allocation.validate_identity(ctx, store, change, branch, head)?;
        crate::replica::write_exclusive(
            &allocation.path(),
            &serde_json::to_vec_pretty(&allocation)?,
        )?;
        Ok(allocation)
    }

    pub fn load(worktree: &Path) -> Result<Option<Self>> {
        let path = gitio::worktree_admin_dir(worktree)?.join("accept-checkout.json");
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .with_context(|| format!("malformed allocation recovery {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error)
                .with_context(|| format!("cannot read allocation recovery {}", path.display())),
        }
    }

    pub fn validate_identity(
        &self,
        ctx: &Ctx,
        store: &Store,
        change: &str,
        branch: &str,
        head: &str,
    ) -> Result<()> {
        if self.schema != CHECKOUT_RECOVERY_SCHEMA
            || self.repository_id != store.repository_id
            || self.change_id != change
            || self.branch != branch
            || self.head != head
            || !ctx.admits_path(&self.worktree)
            || !gitio::worktree_admin_matches(&ctx.cwd, &self.worktree, &self.administrative_dir)
            || gitio::common_dir(&self.worktree)? != gitio::common_dir(&ctx.cwd)?
            || gitio::current_branch(&self.worktree)?.as_deref() != Some(branch)
            || gitio::head(&self.worktree)? != head
        {
            bail!("allocation recovery identity or ownership moved for {}; keeping the checkout and owner record", self.worktree.display());
        }
        Ok(())
    }

    pub fn validate(
        &self,
        ctx: &Ctx,
        store: &Store,
        change: &str,
        branch: &str,
        head: &str,
    ) -> Result<()> {
        self.validate_identity(ctx, store, change, branch, head)?;
        if !gitio::worktree_owner_matches(&self.worktree, change, Some(&self.owner_token)) {
            let owner_path = self.administrative_dir.join("arc-owner");
            if owner_path.try_exists()? || !self.unpopulated()? {
                bail!(
                    "allocation owner moved; keeping checkout {}",
                    self.worktree.display()
                );
            }
        }
        Ok(())
    }

    pub fn ensure_owner(&self, ctx: &Ctx, store: &Store) -> Result<()> {
        self.validate(ctx, store, &self.change_id, &self.branch, &self.head)?;
        if !gitio::worktree_owner_matches(&self.worktree, &self.change_id, Some(&self.owner_token))
        {
            gitio::write_worktree_owner_with_token(
                &self.administrative_dir,
                &self.change_id,
                &self.owner_token,
            )?;
        }
        Ok(())
    }

    /// Only the allocated .git link may exist before index population. Any
    /// additional content or index makes a dirty checkout ineligible for reset.
    pub fn unpopulated(&self) -> Result<bool> {
        if self.administrative_dir.join("index").try_exists()? {
            return Ok(false);
        }
        for entry in std::fs::read_dir(&self.worktree)? {
            if entry?.file_name() != ".git" {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn clear(&self, ctx: &Ctx, store: &Store) -> Result<()> {
        self.validate(ctx, store, &self.change_id, &self.branch, &self.head)?;
        if Self::load(&self.worktree)?.as_ref() != Some(self) {
            bail!("allocation recovery changed; keeping its record");
        }
        let events = store.load_events(&self.change_id)?;
        if !events.iter().any(|event| event.repository_id == self.repository_id && matches!(&event.payload, Payload::ChangeCheckoutBound { worktree, worktree_owner_token, worktree_created: true, .. } if config::resolved(Path::new(worktree)) == self.worktree && worktree_owner_token.as_deref() == Some(&self.owner_token))) {
            bail!("allocation recovery has no published local binding; keeping its record");
        }
        std::fs::remove_file(self.path()).context("cannot finish allocation recovery")
    }
}
