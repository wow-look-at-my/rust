use super::*;

#[test]
fn test_timestamp_serialization() {
    for i in 0..1_000u64 {
        let time = UNIX_EPOCH + Duration::new(i * 1_434_578, (i as u32) * 239_000);
        let s = timestamp_to_string(time);
        assert_eq!(Ok(time), string_to_timestamp(&s));
    }
}

#[test]
fn test_find_source_directory_in_iter() {
    // Find newest
    assert_eq!(
        find_source_directory_in_iter(
            [
                PathBuf::from("crate-dir/s-3234-0000-svh"),
                PathBuf::from("crate-dir/s-2234-0000-svh"),
                PathBuf::from("crate-dir/s-1234-0000-svh")
            ]
            .into_iter(),
        ),
        Some(PathBuf::from("crate-dir/s-3234-0000-svh"))
    );

    // Filter out "-working"
    assert_eq!(
        find_source_directory_in_iter(
            [
                PathBuf::from("crate-dir/s-3234-0000-working"),
                PathBuf::from("crate-dir/s-2234-0000-svh"),
                PathBuf::from("crate-dir/s-1234-0000-svh")
            ]
            .into_iter(),
        ),
        Some(PathBuf::from("crate-dir/s-2234-0000-svh"))
    );

    // Handle empty
    assert_eq!(find_source_directory_in_iter([].into_iter()), None);

    // Handle only working
    assert_eq!(
        find_source_directory_in_iter(
            [
                PathBuf::from("crate-dir/s-3234-0000-working"),
                PathBuf::from("crate-dir/s-2234-0000-working"),
                PathBuf::from("crate-dir/s-1234-0000-working")
            ]
            .into_iter(),
        ),
        None
    );
}

const TEST_ID: &str = "0123456789abc";

fn time(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

/// Creates `{incr_dir}/{name}` with one old finalized session and its lock file.
/// Returns the crate dir and the lock file.
fn make_crate_dir(incr_dir: &Path, name: &str) -> (PathBuf, PathBuf) {
    let crate_dir = incr_dir.join(name);
    let session_dir = crate_dir.join(format!("s-{}-0000-svh", timestamp_to_string(time(1_000))));
    std_fs::create_dir_all(&session_dir).unwrap();
    std_fs::write(session_dir.join(DEP_GRAPH_FILENAME), b"graph").unwrap();
    let lock_file = lock_file_path(&session_dir);
    std_fs::write(&lock_file, b"").unwrap();
    (crate_dir, lock_file)
}

#[test]
fn test_crate_dir_crate_name() {
    assert_eq!(crate_dir_crate_name(&format!("foo-{TEST_ID}")), Some("foo"));
    assert_eq!(crate_dir_crate_name(&format!("foo_bar-{TEST_ID}")), Some("foo_bar"));
    assert_eq!(crate_dir_crate_name("foo-0123"), None);
    assert_eq!(crate_dir_crate_name(&format!("foo-{TEST_ID}0")), None);
    assert_eq!(crate_dir_crate_name("foo-0123456789ABC"), None);
    assert_eq!(crate_dir_crate_name(&format!("-{TEST_ID}")), None);
    assert_eq!(crate_dir_crate_name("foo"), None);
}

#[test]
fn test_session_entry_timestamp() {
    let ts = timestamp_to_string(time(42));
    assert_eq!(session_entry_timestamp(&format!("s-{ts}-0000-svh")), Some(time(42)));
    assert_eq!(session_entry_timestamp(&format!("s-{ts}-0000-working")), Some(time(42)));
    assert_eq!(session_entry_timestamp(&format!("s-{ts}-0000.lock")), Some(time(42)));
    assert_eq!(session_entry_timestamp("dep-graph.bin"), None);
    assert_eq!(session_entry_timestamp(&format!("s-{ts}.lock")), None);
}

#[test]
fn test_select_configs_to_prune() {
    let current = PathBuf::from("incr/foo-current");
    let entries = vec![
        (PathBuf::from("incr/foo-b"), time(20)),
        (current.clone(), time(5)),
        (PathBuf::from("incr/foo-c"), time(30)),
        (PathBuf::from("incr/foo-a"), time(10)),
    ];
    let paths = |names: &[&str]| -> Vec<PathBuf> {
        names.iter().map(|n| PathBuf::from(format!("incr/foo-{n}"))).collect()
    };

    // The current dir is kept even though it is the oldest.
    assert_eq!(select_configs_to_prune(entries.clone(), &current, 2), paths(&["a", "b"]));
    assert_eq!(select_configs_to_prune(entries.clone(), &current, 1), paths(&["a", "b", "c"]));
    assert_eq!(select_configs_to_prune(entries.clone(), &current, 0), paths(&["a", "b", "c"]));
    assert_eq!(select_configs_to_prune(entries.clone(), &current, 4), paths(&[]));
    assert_eq!(select_configs_to_prune(entries, &current, 10), paths(&[]));
}

#[test]
fn test_evict_for_size_cap() {
    let current = PathBuf::from("incr/foo-current");
    let entries = vec![
        (PathBuf::from("incr/bar-b"), time(20), 10),
        (current.clone(), time(5), 10),
        (PathBuf::from("incr/baz-c"), time(30), 10),
        (PathBuf::from("incr/foo-a"), time(10), 10),
    ];
    let a = PathBuf::from("incr/foo-a");
    let b = PathBuf::from("incr/bar-b");
    let c = PathBuf::from("incr/baz-c");

    // Oldest first, and stop once the total fits.
    let mut tried = Vec::new();
    let evicted = evict_for_size_cap(entries.clone(), &current, 25, |p| {
        tried.push(p.to_owned());
        true
    });
    assert_eq!(evicted, vec![a.clone(), b.clone()]);
    assert_eq!(tried, evicted);

    // Already under the cap.
    assert!(evict_for_size_cap(entries.clone(), &current, 40, |_| true).is_empty());

    // The current dir is never selected, even if the cap cannot be met.
    assert_eq!(
        evict_for_size_cap(entries.clone(), &current, 5, |_| true),
        vec![a.clone(), b.clone(), c.clone()]
    );

    // A dir that cannot be deleted is skipped and the next one is tried.
    assert_eq!(evict_for_size_cap(entries, &current, 25, |p| p != a.as_path()), vec![b, c]);
}

#[test]
fn test_list_crate_dirs() {
    let dir = rustc_fs_util::TempDirBuilder::new().tempdir_in(std::env::temp_dir()).unwrap();
    let incr_dir = dir.path();
    let foo = incr_dir.join(format!("foo-{TEST_ID}"));
    let foo2 = incr_dir.join("foo-zzzzzzzzzzzzz");
    let foo_bar = incr_dir.join(format!("foo_bar-{TEST_ID}"));
    std_fs::create_dir(&foo).unwrap();
    std_fs::create_dir(&foo2).unwrap();
    std_fs::create_dir(&foo_bar).unwrap();
    std_fs::create_dir(incr_dir.join("foo-0123")).unwrap();
    std_fs::create_dir(incr_dir.join("foo")).unwrap();
    std_fs::write(incr_dir.join("foo-aaaaaaaaaaaaa"), b"").unwrap();

    assert_eq!(list_crate_dirs(incr_dir, Some("foo")).unwrap(), vec![foo.clone(), foo2.clone()]);
    assert_eq!(list_crate_dirs(incr_dir, Some("foo_bar")).unwrap(), vec![foo_bar.clone()]);
    assert_eq!(list_crate_dirs(incr_dir, None).unwrap(), vec![foo, foo2, foo_bar]);
}

#[test]
fn test_try_delete_crate_dir_skips_locked() {
    let dir = rustc_fs_util::TempDirBuilder::new().tempdir_in(std::env::temp_dir()).unwrap();
    let (crate_dir, lock_file) = make_crate_dir(dir.path(), &format!("foo-{TEST_ID}"));

    let lock = flock::Lock::try_lock(&lock_file, false, true).unwrap();
    assert_eq!(try_delete_crate_dir(&crate_dir).unwrap(), false);
    assert!(crate_dir.exists());
    assert!(lock_file.exists());

    drop(lock);
    assert_eq!(try_delete_crate_dir(&crate_dir).unwrap(), true);
    assert!(!crate_dir.exists());
}

#[test]
fn test_try_delete_crate_dir_skips_recent() {
    let dir = rustc_fs_util::TempDirBuilder::new().tempdir_in(std::env::temp_dir()).unwrap();
    let crate_dir = dir.path().join(format!("foo-{TEST_ID}"));
    let session_dir = generate_session_dir_path(&crate_dir);
    std_fs::create_dir_all(&session_dir).unwrap();
    std_fs::write(lock_file_path(&session_dir), b"").unwrap();

    assert_eq!(try_delete_crate_dir(&crate_dir).unwrap(), false);
    assert!(session_dir.exists());
}

#[test]
fn test_replace_hard_link_with_copy_unshares_hard_link() {
    let dir = rustc_fs_util::TempDirBuilder::new().tempdir_in(std::env::temp_dir()).unwrap();
    let file = dir.path().join("file");
    let link = dir.path().join("link");
    std_fs::write(&file, b"original").unwrap();
    std_fs::hard_link(&file, &link).unwrap();

    replace_hard_link_with_copy(&link).unwrap();

    std_fs::write(&file, b"changed").unwrap();
    assert_eq!(std_fs::read(&link).unwrap(), b"original");
    assert!(!link.with_added_extension("tmp").exists());
}
