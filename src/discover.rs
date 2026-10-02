//! Bounded discovery of simulation directories.
//!
//! A breadth-first walk from the configured roots that never visits more than
//! a fixed number of directories, never reads more than a fixed number of
//! entries per directory, and never follows symbolic links to directories.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use crate::format::STATUS_FILE;

#[derive(Clone, Debug)]
pub struct Limits {
    /// Roots have depth 0
    pub max_depth: usize,
    pub max_dirs: usize,
    pub max_entries_per_dir: usize,
    pub max_sims: usize,
    /// Directory names that are never entered
    pub skip_dirs: Vec<String>,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_depth: 5,
            max_dirs: 5000,
            max_entries_per_dir: 2000,
            max_sims: 500,
            skip_dirs: [".git", "target", "node_modules", "__pycache__"]
                .map(String::from)
                .to_vec(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScanResult {
    /// Directories containing a status file
    pub sims: Vec<PathBuf>,
    pub dirs_visited: usize,
    /// Problems and limits hit, for the status bar
    pub warnings: Vec<String>,
}

pub fn scan(roots: &[PathBuf], limits: &Limits) -> ScanResult {
    let mut res = ScanResult::default();
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    for root in roots {
        // Roots themselves may be symlinks
        match fs::canonicalize(root) {
            Ok(r) if r.is_dir() => {
                if !queue.iter().any(|(q, _)| *q == r) {
                    queue.push_back((r, 0));
                }
            }
            Ok(_) => res.warnings.push(format!("{}: not a directory", root.display())),
            Err(e) => res.warnings.push(format!("{}: {e}", root.display())),
        }
    }

    let mut entries_capped = 0;
    // Overlapping roots would otherwise visit directories twice. Paths below a
    // canonical root are canonical, since symlinks are not followed.
    let mut visited: HashSet<PathBuf> = HashSet::new();
    while let Some((dir, depth)) = queue.pop_front() {
        if visited.contains(&dir) {
            continue;
        }
        if res.dirs_visited >= limits.max_dirs {
            res.warnings.push(format!(
                "stopped after {} directories (max_dirs)",
                limits.max_dirs
            ));
            break;
        }
        if res.sims.len() >= limits.max_sims {
            res.warnings
                .push(format!("stopped after {} simulations (max_sims)", limits.max_sims));
            break;
        }
        res.dirs_visited += 1;
        visited.insert(dir.clone());
        let rd = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => {
                // Unreadable directories below a root are common (permissions); only
                // report problems with the roots themselves
                if depth == 0 {
                    res.warnings.push(format!("{}: {e}", dir.display()));
                }
                continue;
            }
        };
        let mut has_status = false;
        for (n, entry) in rd.enumerate() {
            if n >= limits.max_entries_per_dir {
                entries_capped += 1;
                break;
            }
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // `file_type` uses the directory entry type and does not follow symlinks
            let Ok(ft) = entry.file_type() else { continue };
            if name == STATUS_FILE {
                if ft.is_file() || ft.is_symlink() {
                    has_status = true;
                }
            } else if ft.is_dir() && !skip(&name, limits) {
                // Hitting max_depth is normal and not worth a warning
                if depth < limits.max_depth {
                    queue.push_back((entry.path(), depth + 1));
                }
            }
        }
        if has_status {
            res.sims.push(dir);
        }
    }
    if entries_capped > 0 {
        res.warnings.push(format!(
            "{entries_capped} directories had more than {} entries (max_entries_per_dir)",
            limits.max_entries_per_dir
        ));
    }
    res
}

fn skip(name: &str, limits: &Limits) -> bool {
    name.starts_with('.') || limits.skip_dirs.iter().any(|s| s == name)
}

/// Resolve an image path from a status file relative to its simulation
/// directory, refusing anything that would leave that directory.
pub fn confined_path(sim_dir: &Path, file: &str) -> Option<PathBuf> {
    let rel = Path::new(file);
    if file.is_empty()
        || !rel
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir))
    {
        return None;
    }
    let path = sim_dir.join(rel);
    // Symlinks must not point outside either
    let canon = fs::canonicalize(&path).ok()?;
    let base = fs::canonicalize(sim_dir).ok()?;
    canon.starts_with(&base).then_some(canon)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, write};

    fn sim(dir: &Path) {
        create_dir_all(dir).unwrap();
        write(dir.join(STATUS_FILE), "").unwrap();
    }

    fn names(res: &ScanResult, root: &Path) -> Vec<String> {
        let root = fs::canonicalize(root).unwrap();
        let mut v: Vec<String> = res
            .sims
            .iter()
            .map(|p| p.strip_prefix(&root).unwrap().display().to_string())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn finds_and_limits() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        sim(&r.join("a"));
        sim(&r.join("a/nested"));
        sim(&r.join("b/c/d"));
        sim(&r.join("b/c/d/e/f/g/too_deep"));
        sim(&r.join(".hidden/x"));
        sim(&r.join("target/x"));
        create_dir_all(r.join("a/simwatch.toml.d")).unwrap();

        let res = scan(&[r.to_path_buf()], &Limits::default());
        assert_eq!(names(&res, r), ["a", "a/nested", "b/c/d"]);
        assert!(res.warnings.is_empty(), "{:?}", res.warnings);

        let limits = Limits {
            max_depth: 1,
            ..Limits::default()
        };
        assert_eq!(names(&scan(&[r.to_path_buf()], &limits), r), ["a"]);

        let limits = Limits {
            max_dirs: 2,
            ..Limits::default()
        };
        let res = scan(&[r.to_path_buf()], &limits);
        assert_eq!(res.dirs_visited, 2);
        assert!(res.warnings[0].contains("max_dirs"));

        let limits = Limits {
            max_sims: 1,
            ..Limits::default()
        };
        let res = scan(&[r.to_path_buf()], &limits);
        assert_eq!(res.sims.len(), 1);
        assert!(res.warnings[0].contains("max_sims"));
    }

    #[test]
    fn entries_per_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        for i in 0..50 {
            write(r.join(format!("file{i}")), "").unwrap();
        }
        let limits = Limits {
            max_entries_per_dir: 10,
            ..Limits::default()
        };
        let res = scan(&[r.to_path_buf()], &limits);
        assert!(res.warnings[0].contains("max_entries_per_dir"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loops_are_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        sim(&r.join("a"));
        std::os::unix::fs::symlink(r, r.join("a/loop")).unwrap();
        let res = scan(&[r.to_path_buf(), r.join("a")], &Limits::default());
        assert_eq!(names(&res, r), ["a"]);
        assert!(res.dirs_visited <= 3);
    }

    #[test]
    fn missing_root() {
        let res = scan(&[PathBuf::from("/nonexistent/simwatch")], &Limits::default());
        assert!(res.sims.is_empty());
        assert_eq!(res.warnings.len(), 1);
    }

    #[test]
    fn confinement() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        create_dir_all(r.join("sim/plots")).unwrap();
        write(r.join("sim/plots/a.png"), "").unwrap();
        write(r.join("secret.png"), "").unwrap();
        let s = r.join("sim");
        assert!(confined_path(&s, "plots/a.png").is_some());
        assert!(confined_path(&s, "./plots/a.png").is_some());
        assert!(confined_path(&s, "../secret.png").is_none());
        assert!(confined_path(&s, "plots/../../secret.png").is_none());
        assert!(confined_path(&s, &r.join("secret.png").display().to_string()).is_none());
        assert!(confined_path(&s, "").is_none());
        assert!(confined_path(&s, "missing.png").is_none());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(r.join("secret.png"), s.join("link.png")).unwrap();
            assert!(confined_path(&s, "link.png").is_none());
        }
    }
}
