//@ run-pass
//@ needs-subprocess
//@ only-unix
//@ ignore-fuchsia no /bin/sh

// A file the kernel cannot execute, such as a script with no `#!` line, runs
// through /bin/sh, as execvp does.

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn main() {
    let dir = std::env::temp_dir().join(format!("spawn-no-shebang-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("script");
    std::fs::write(&script, "printf 'ran %s\\n' \"$1\"\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let out = Command::new(&script).arg("with-arg").output().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();

    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ran with-arg\n");
}
