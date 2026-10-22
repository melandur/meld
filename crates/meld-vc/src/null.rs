//! Fallback backend used when no version control system is found (port of
//! `meld/vc/_null.py`). Every file is "normal" and its base revision is a
//! copy of itself.

use std::path::{Path, PathBuf};

use crate::command;
use crate::entry::Action;
use crate::error::Result;
use crate::paths;
use crate::vc::{TreeCache, Vc, VcCore, VcKind};

/// The "None" backend.
#[derive(Debug, Clone)]
pub struct NullVc {
    core: VcCore,
}

impl NullVc {
    /// Any folder is a "repository" root of the null backend.
    pub fn new(path: &Path) -> NullVc {
        let path = paths::abspath(path);
        let location = if path.is_dir() {
            path
        } else {
            paths::parent_or_self(&path)
        };
        NullVc {
            core: VcCore {
                root: location.clone(),
                location,
                cache: TreeCache::default(),
            },
        }
    }
}

impl Vc for NullVc {
    fn kind(&self) -> VcKind {
        VcKind::Null
    }
    fn core(&self) -> &VcCore {
        &self.core
    }
    fn core_mut(&mut self) -> &mut VcCore {
        &mut self.core
    }

    fn supported_actions(&self) -> &'static [Action] {
        &[]
    }

    fn update_tree_state_cache(&mut self, _path: &Path) -> Result<()> {
        Ok(())
    }

    fn get_path_for_repo_file(&self, path: &Path, _commit: Option<&str>) -> Result<PathBuf> {
        let content = std::fs::read(path)?;
        command::write_temp(&content, "", &paths::suffix(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::State;

    #[test]
    fn null_vc_lists_and_copies() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        std::fs::write(&f, b"hello").unwrap();
        let mut vc = NullVc::new(&f);
        assert_eq!(vc.location(), dir.path());
        vc.refresh_vc_state(None).unwrap();
        let entries = vc.get_entries(dir.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].state, State::Normal);
        let copy = vc.get_path_for_repo_file(&f, None).unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), b"hello");
        assert!(copy.to_string_lossy().ends_with(".txt"));
        std::fs::remove_file(copy).unwrap();
        let actions = vc.get_valid_actions(&[(f.clone(), State::Normal)]);
        assert_eq!(
            actions.into_iter().collect::<Vec<_>>(),
            vec![Action::Compare]
        );
    }
}
