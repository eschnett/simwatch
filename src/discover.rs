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
            max_depth: 8,
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
    // Entries are (directory, depth, index into `tops`)
    let mut queue: VecDeque<(PathBuf, usize, Option<usize>)> = VecDeque::new();
    for root in roots {
        // Roots themselves may be symlinks
        match fs::canonicalize(root) {
            Ok(r) if r.is_dir() => {
                if !queue.iter().any(|(q, _, _)| *q == r) {
                    queue.push_back((r, 0, None));
                }
            }
            Ok(_) => res
                .warnings
                .push(format!("{}: not a directory", root.display())),
            Err(e) => res.warnings.push(format!("{}: {e}", root.display())),
        }
    }

    let mut entries_capped = 0;
    // Directories just below a root, with the number of directories visited
    // in each, to name the culprits when `max_dirs` is hit
    let mut tops: Vec<(String, usize)> = Vec::new();
    // Overlapping roots would otherwise visit directories twice. Paths below a
    // canonical root are canonical, since symlinks are not followed.
    let mut visited: HashSet<PathBuf> = HashSet::new();
    while let Some((dir, depth, top)) = queue.pop_front() {
        if visited.contains(&dir) {
            continue;
        }
        if res.dirs_visited >= limits.max_dirs {
            res.warnings.push(max_dirs_warning(limits.max_dirs, tops));
            break;
        }
        if res.sims.len() >= limits.max_sims {
            res.warnings.push(format!(
                "stopped after {} simulations (max_sims)",
                limits.max_sims
            ));
            break;
        }
        res.dirs_visited += 1;
        let top = top.or_else(|| {
            (depth == 1).then(|| {
                let name = dir.file_name().unwrap_or_default().to_string_lossy();
                tops.push((name.into_owned(), 0));
                tops.len() - 1
            })
        });
        if let Some(t) = top {
            tops[t].1 += 1;
        }
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
                    queue.push_back((entry.path(), depth + 1, top));
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

/// Name the subtrees below the roots that used up most of the budget, so that
/// they can be added to `skip_dirs`. The names come early in the message
/// because the status bar truncates warnings.
fn max_dirs_warning(max_dirs: usize, mut tops: Vec<(String, usize)>) -> String {
    tops.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let big: Vec<String> = tops
        .iter()
        .take(3)
        .filter(|(_, n)| *n >= (max_dirs / 10).max(2))
        .map(|(name, n)| format!("{name} ({n})"))
        .collect();
    if big.is_empty() {
        format!("stopped after {max_dirs} directories (max_dirs)")
    } else {
        format!("max_dirs = {max_dirs} reached, most in {}", big.join(", "))
    }
}

fn skip(name: &str, limits: &Limits) -> bool {
    name.starts_with('.')
        || limits
            .skip_dirs
            .iter()
            .any(|s| s == name || (s.contains(['*', '?']) && glob_match(s, name)))
}

/// Shell-style matching of a whole name, where `*` matches any sequence of
/// characters and `?` matches one character.
fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut i, mut j) = (0, 0);
    // Position of the last `*` in the pattern, and where in the name it
    // started matching
    let mut star: Option<(usize, usize)> = None;
    while j < n.len() {
        if i < p.len() && (p[i] == '?' || p[i] == n[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some((i, j));
            i += 1;
        } else if let Some((si, sj)) = star {
            // Let the last `*` match one more character
            star = Some((si, sj + 1));
            i = si + 1;
            j = sj + 1;
        } else {
            return false;
        }
    }
    p[i..].iter().all(|&c| c == '*')
}

/// Resolve an image path from a status file relative to its simulation
/// directory, refusing anything that would leave that directory.
pub fn confined_path(sim_dir: &Path, file: &str) -> Option<PathBuf> {
    let rel = Path::new(file);
    if file.is_empty()
        || !rel.components().all(|c| {
            matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        })
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
        sim(&r.join("b/c/d/e/f/g/h/i/too_deep"));
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

    #[test]
    fn max_dirs_names_subtrees() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        for i in 0..30 {
            create_dir_all(r.join(format!("build/d{i}"))).unwrap();
        }
        create_dir_all(r.join("depot/d")).unwrap();
        sim(&r.join("runs/x"));
        // The root, three directories below it, and 26 of the 32 at depth 2
        let limits = Limits {
            max_dirs: 30,
            ..Limits::default()
        };
        let res = scan(&[r.to_path_buf()], &limits);
        assert_eq!(res.warnings.len(), 1);
        let w = &res.warnings[0];
        assert!(
            w.starts_with("max_dirs = 30 reached, most in build ("),
            "{w}"
        );
        // Too few directories to be worth naming
        assert!(!w.contains("depot") && !w.contains("runs"), "{w}");

        // The budget is used up just below the root: nothing to name
        let limits = Limits {
            max_dirs: 3,
            ..Limits::default()
        };
        let res = scan(&[r.to_path_buf()], &limits);
        assert_eq!(res.warnings, ["stopped after 3 directories (max_dirs)"]);
    }

    #[test]
    fn skip_dirs_patterns() {
        let tmp = tempfile::tempdir().unwrap();
        let r = tmp.path();
        sim(&r.join("ygg15133/x"));
        sim(&r.join("ygg15133-mpt/x"));
        sim(&r.join("julia-depot/x"));
        sim(&r.join("runs/x"));
        sim(&r.join("runs/ygg"));
        let limits = Limits {
            skip_dirs: vec!["ygg?*".into(), "*-depot".into()],
            ..Limits::default()
        };
        let res = scan(&[r.to_path_buf()], &limits);
        assert_eq!(names(&res, r), ["runs/x", "runs/ygg"]);
    }

    #[test]
    fn glob() {
        assert!(glob_match("ygg*", "ygg"));
        assert!(glob_match("ygg*", "ygg15133-mpt"));
        assert!(!glob_match("ygg*", "xygg"));
        assert!(glob_match("*-depot", "julia-depot"));
        assert!(!glob_match("*-depot", "julia-depot2"));
        assert!(glob_match("a*b*c", "aXbYbZc"));
        assert!(!glob_match("a*b*c", "aXbYbZ"));
        assert!(glob_match("out-??", "out-12"));
        assert!(!glob_match("out-??", "out-123"));
        assert!(glob_match("*", ""));
        assert!(glob_match("ü?", "üß"));
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
        let res = scan(
            &[PathBuf::from("/nonexistent/simwatch")],
            &Limits::default(),
        );
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
