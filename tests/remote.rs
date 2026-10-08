//! Remote directories through a fake `ssh` that runs the command locally.

use std::fs::{create_dir_all, write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

const SIMWATCH: &str = env!("CARGO_BIN_EXE_simwatch");

/// Like ssh, the last argument is the command for the remote shell
const FAKE_SSH: &str = "#!/bin/sh\nfor a; do cmd=$a; done\nexec sh -c \"$cmd\"\n";

fn setup(dir: &Path, remote_program: &str) -> std::path::PathBuf {
    let ssh = dir.join("fake-ssh");
    write(&ssh, FAKE_SSH).unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cfg = dir.join("config.toml");
    write(
        &cfg,
        format!(
            "ssh = [\"{}\"]\nremote_program = \"{remote_program}\"\nslurm = false\n",
            ssh.display()
        ),
    )
    .unwrap();
    for (sub, name) in [("remote/a", "far-away"), ("local/b", "close-by")] {
        create_dir_all(dir.join(sub)).unwrap();
        write(
            dir.join(sub).join("simwatch.toml"),
            format!("name = \"{name}\"\n"),
        )
        .unwrap();
    }
    cfg
}

fn run(dir: &Path, cfg: &Path, roots: &[String]) -> Output {
    Command::new(SIMWATCH)
        .env("HOME", dir)
        .arg("--config")
        .arg(cfg)
        .arg("--print")
        .args(roots)
        .output()
        .unwrap()
}

#[test]
fn print_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let cfg = setup(dir, SIMWATCH);
    let remote = format!("fakehost:{}", dir.join("remote").display());

    let out = run(dir, &cfg, std::slice::from_ref(&remote));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{out:?}");
    assert!(stdout.contains("far-away"), "{stdout}");
    // A single host needs no Host column
    assert!(!stdout.contains("Host"), "{stdout}");

    let local = dir.join("local").display().to_string();
    let out = run(dir, &cfg, &[remote, local]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("far-away") && stdout.contains("close-by"),
        "{stdout}"
    );
    assert!(
        stdout.contains("Host") && stdout.contains("fakehost"),
        "{stdout}"
    );
    assert!(stdout.contains("local"), "{stdout}");
}

#[test]
fn missing_remote_program() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let cfg = setup(dir, "/nonexistent/simwatch");
    let remote = format!("fakehost:{}", dir.join("remote").display());
    let out = run(dir, &cfg, &[remote]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("fakehost: `/nonexistent/simwatch` not found"),
        "{stderr}"
    );
}
