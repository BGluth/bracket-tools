//! Filesystem helpers: `~` expansion for configured paths and atomic writes
//! (tmp + rename) so a crash mid-write never leaves a torn feed or snapshot.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

pub fn expand_home(path: &Path) -> PathBuf {
    match (path.strip_prefix("~"), env::var_os("HOME")) {
        (Ok(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => path.to_path_buf(),
    }
}

pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp = tmp_path(path);
    fs::write(&tmp, contents).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("moving {} into place", path.display()))?;

    Ok(())
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use std::{env, fs, path::Path, process};

    use super::{expand_home, write_atomic};

    #[test]
    fn expands_a_leading_tilde_only() {
        let home = env::var("HOME").expect("HOME is set in the test environment");
        assert_eq!(expand_home(Path::new("~/x/y")), Path::new(&home).join("x/y"));
        assert_eq!(expand_home(Path::new("/abs/x")), Path::new("/abs/x"));
        assert_eq!(expand_home(Path::new("~user/x")), Path::new("~user/x"));
    }

    #[test]
    fn atomic_write_creates_parents_and_leaves_no_tmp() {
        let dir = env::temp_dir().join(format!("gg-watch-files-{}", process::id()));
        let target = dir.join("nested/feed.json");

        write_atomic(&target, b"one").unwrap();
        write_atomic(&target, b"two").unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "two");
        assert!(!dir.join("nested/feed.json.tmp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
