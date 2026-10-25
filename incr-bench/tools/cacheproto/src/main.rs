// `k` distinct indices below `n`.

mod real;
mod store;

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use real::Real;

const RUNS: usize = 5;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// `k` distinct indices below `n`.
    fn picks(&mut self, n: usize, k: usize) -> Vec<usize> {
        let mut v: Vec<usize> = (0..n).collect();
        for i in 0..k {
            let j = i + (self.next() as usize) % (n - i);
            v.swap(i, j);
        }
        v.truncate(k);
        v
    }
}

fn ms(ns: u128) -> f64 {
    ns as f64 / 1e6
}

struct Row {
    name: String,
    times: Vec<u128>,
    bytes: u64,
    note: String,
}

fn report(rows: &[Row]) {
    println!("\n| step | min ms | median ms | bytes written | note |");
    println!("|---|---:|---:|---:|---|");
    for r in rows {
        let mut t = r.times.clone();
        t.sort_unstable();
        println!(
            "| {} | {:.1} | {:.1} | {} | {} |",
            r.name,
            ms(t[0]),
            ms(t[t.len() / 2]),
            r.bytes,
            r.note
        );
    }
}

fn free_bytes(p: &Path) -> u64 {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::statvfs(c.as_ptr(), &mut s) }, 0);
    s.f_bavail as u64 * s.f_frsize as u64
}

fn need(p: &Path, bytes: u64) {
    let free = free_bytes(p);
    if free < bytes {
        eprintln!("only {free} bytes free under {}, need {bytes}", p.display());
        std::process::exit(2);
    }
}

/// Today's save, byte for byte: read every value and write the whole file anew.
fn today_rewrite(real: &Real, out: &Path) -> u64 {
    let f = std::fs::File::create(out).unwrap();
    let mut w = BufWriter::with_capacity(64 << 10, f);
    let mut ord: Vec<usize> = (0..real.values.len()).collect();
    ord.sort_unstable_by_key(|&i| real.values[i].pos);
    let b = &real.qc[..];
    let mut at = 0usize;
    for i in ord {
        let e = &real.values[i];
        w.write_all(&b[at..e.pos as usize]).unwrap();
        w.write_all(real.payload(i)).unwrap();
        at = e.pos as usize + e.len as usize;
    }
    w.write_all(&b[at..]).unwrap();
    w.flush().unwrap();
    b.len() as u64
}

fn checksum(b: &[u8]) -> u64 {
    b.iter().fold(0u64, |a, &x| a.rotate_left(5) ^ x as u64)
}

fn bench(real: &Real, work: &Path) {
    let n = real.values.len();
    let file = work.join("query-cache.bin");
    let qc_len = real.qc.len() as u64;
    let mut rows = Vec::new();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);

    // 1. Today: footer decode into the HashMap, then the full rewrite.
    let mut t = Vec::new();
    for _ in 0..RUNS {
        let s = Instant::now();
        let m = real::today_index(&real.qc);
        std::hint::black_box(m.len());
        t.push(s.elapsed().as_nanos());
    }
    rows.push(Row {
        name: "today: load footer index (HashMap of N)".into(),
        times: t,
        bytes: 0,
        note: format!("N = {n}"),
    });
    need(work, qc_len + (256 << 20));
    let out = work.join("today-out.bin");
    let mut t = Vec::new();
    let mut bytes = 0;
    for _ in 0..RUNS {
        let s = Instant::now();
        bytes = today_rewrite(real, &out);
        t.push(s.elapsed().as_nanos());
        std::fs::remove_file(&out).unwrap();
    }
    rows.push(Row {
        name: "today: read all values + write new file".into(),
        times: t,
        bytes,
        note: "byte copy, no decode/encode CPU".into(),
    });

    // 2a. Initial build of the proposed file.
    let mut t = Vec::new();
    let mut bytes = 0;
    for r in 0..RUNS {
        if r > 0 {
            std::fs::remove_file(&file).unwrap();
        }
        need(work, qc_len * 2);
        let s = Instant::now();
        bytes = store::build(&file, real).unwrap();
        t.push(s.elapsed().as_nanos());
    }
    rows.push(Row {
        name: "proposed: initial build (values + index)".into(),
        times: t,
        bytes,
        note: format!("file {} B", std::fs::metadata(&file).unwrap().len()),
    });

    // 2b. A session that rewrites k values; undone after each run.
    let mut t = Vec::new();
    for _ in 0..RUNS {
        let e = store::edit(&file, real, &[], true, false).unwrap();
        t.push(e.total_ns);
        store::undo(&file, e.undo.unwrap()).unwrap();
    }
    rows.push(Row {
        name: "proposed: no-op session (open+mmap+N lookups)".into(),
        times: t,
        bytes: 0,
        note: String::new(),
    });
    for pct in [1usize, 5, 20] {
        let k = n * pct / 100;
        let mut t = Vec::new();
        let mut lk = Vec::new();
        let mut sv = Vec::new();
        let (mut bytes, mut pages, mut vb) = (0, 0, 0);
        for _ in 0..RUNS {
            let picks = rng.picks(n, k);
            vb = picks.iter().map(|&i| real.values[i].len as u64).sum::<u64>();
            let e = store::edit(&file, real, &picks, true, false).unwrap();
            t.push(e.total_ns);
            lk.push(e.lookup_ns);
            sv.push(e.save_ns);
            bytes = e.appended + 8 * e.slot_stores + 8 * 8;
            pages = e.dirty_index_pages;
            store::undo(&file, e.undo.unwrap()).unwrap();
        }
        lk.sort_unstable();
        rows.push(Row {
            name: format!("proposed: session rewriting {pct}% (k = {k}), with N lookups"),
            times: t,
            bytes,
            note: format!(
                "values {vb} B; {pages} index pages dirtied; lookup part median {:.1} ms",
                ms(lk[lk.len() / 2])
            ),
        });
        rows.push(Row {
            name: format!("proposed: save part only, {pct}%"),
            times: sv,
            bytes,
            note: "lock, k appends, k slot stores, end store".into(),
        });
    }

    // 3. Random reads of M values during a build.
    let m = 100_000.min(n);
    let picks: Vec<usize> = (0..m).map(|_| rng.next() as usize % n).collect();
    let map = real::today_index(&real.qc);
    let mut t = Vec::new();
    for _ in 0..RUNS {
        let s = Instant::now();
        let mut x = 0;
        for &i in &picks {
            let e = &real.values[i];
            let pos = map[&e.index] as usize;
            x ^= checksum(&real.qc[pos..pos + e.len as usize]);
        }
        std::hint::black_box(x);
        t.push(s.elapsed().as_nanos());
    }
    rows.push(Row {
        name: format!("lookup: today footer HashMap, {m} random reads"),
        times: t,
        bytes: 0,
        note: "index by prev dep-node index".into(),
    });
    {
        let f = std::fs::File::open(&file).unwrap();
        let v = store::View { map: unsafe { memmap2::Mmap::map(&f) }.unwrap() };
        let anc = v.ancestors();
        let mut t = Vec::new();
        for _ in 0..RUNS {
            let s = Instant::now();
            let mut x = 0;
            for &i in &picks {
                let e = &real.values[i];
                let (_, w) = v.lookup(e.kind, &e.key).unwrap();
                let o = v.resolve(w, &anc).unwrap();
                x ^= checksum(v.value(o).2);
            }
            std::hint::black_box(x);
            t.push(s.elapsed().as_nanos());
        }
        rows.push(Row {
            name: format!("lookup: proposed in-file index, {m} random reads"),
            times: t,
            bytes: 0,
            note: "index by (kind, key fingerprint)".into(),
        });
    }

    // 2c.
    let mut t = Vec::new();
    let mut bytes = 0;
    let mut note = String::new();
    for _ in 0..RUNS {
        need(work, qc_len * 2);
        let k = n / 4;
        let mut dead = 0;
        while {
            let (live, d) = store::ratio(&file).unwrap();
            dead = d;
            d * 4 < live
        } {
            let picks = rng.picks(n, k);
            store::edit(&file, real, &picks, false, false).unwrap();
        }
        let (live, _) = store::ratio(&file).unwrap();
        let before = std::fs::metadata(&file).unwrap().len();
        let s = Instant::now();
        bytes = store::compact(&file).unwrap();
        t.push(s.elapsed().as_nanos());
        note = format!("dead {dead} / live {live} B, file {before} -> {bytes} B");
    }
    let v = store::verify(&file, real, false).unwrap();
    assert_eq!(v.found, n, "compaction lost values");
    rows.push(Row { name: "proposed: compaction at >=25% dead".into(), times: t, bytes, note });

    report(&rows);
}

/// Child of `killtest`: rewrites slices of the file until it is killed.
fn writer(real: &Real, file: &Path, seed: u64) {
    let mut rng = Rng(seed | 1);
    println!("ready");
    std::io::stdout().flush().unwrap();
    for _ in 0..200 {
        let picks = rng.picks(real.values.len(), 2000);
        store::edit(file, real, &picks, false, true).unwrap();
    }
    println!("done");
}

fn killtest(real: &Real, work: &Path, trials: usize) {
    let file = work.join("query-cache.bin");
    let me = std::env::current_exe().unwrap();
    let pid = std::process::id();
    let qc = format!("/proc/{pid}/fd/{}", real.qf.as_raw_fd());
    let dg = format!("/proc/{pid}/fd/{}", real.gf.as_raw_fd());
    let mut rng = Rng(0x1234_5678);
    let (mut mid, mut ok) = (0, 0);
    for t in 0..trials {
        let mut child = Command::new(&me)
            .args(["writer", &qc, &dg, file.to_str().unwrap(), &(t as u64 + 7).to_string()])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.as_mut().unwrap()).read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "ready");
        std::thread::sleep(Duration::from_millis(50 + rng.next() % 1500));
        unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
        child.wait().unwrap();
        let v = store::verify(&file, real, true).unwrap();
        let good = v.found == real.values.len() && v.mismatched == 0 && v.missing == 0;
        if v.state != "clean" {
            mid += 1;
        }
        if good {
            ok += 1;
        }
        println!(
            "trial {t}: {}; found {} missing {} mismatched {}; {} slots name an uncommitted generation; end {} file {}",
            v.state, v.found, v.missing, v.mismatched, v.foreign_words, v.end, v.file_len
        );
    }
    // The next writer must append past the dead bytes and commit.
    let picks = Rng(99).picks(real.values.len(), 2000);
    store::edit(&file, real, &picks, false, true).unwrap();
    let v = store::verify(&file, real, true).unwrap();
    println!(
        "after one more committed writer: {}; found {} missing {} mismatched {}",
        v.state, v.found, v.missing, v.mismatched
    );
    println!("kill test: {ok}/{trials} trials left a valid file; {mid} kills landed mid-write");
}

fn describe(real: &Real) {
    let total: u64 = real.values.iter().map(|e| e.len as u64).sum();
    let mut lens: Vec<u32> = real.values.iter().map(|e| e.len).collect();
    lens.sort_unstable();
    println!(
        "query-cache.bin {} ({} B), dep-graph.bin {}",
        real.qc_path.display(),
        real.qc.len(),
        real.dg_path.display()
    );
    println!(
        "query values N = {} ({} B, median {} B, p99 {} B, max {} B); side effects {}; allocs {}; hygiene {}; values with no dep node {}; kind bits {}",
        real.values.len(),
        total,
        lens[lens.len() / 2],
        lens[lens.len() * 99 / 100],
        lens[lens.len() - 1],
        real.side_effects,
        real.allocs,
        real.hygiene,
        real.values_without_node,
        real.kind_bits
    );
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let load = |s: &str| -> Real {
        let p = PathBuf::from(s);
        let sess =
            if p.join("query-cache.bin").exists() { p } else { real::find_session(&p).unwrap() };
        real::load(&sess).unwrap()
    };
    match a.get(1).map(String::as_str) {
        Some("describe") => describe(&load(&a[2])),
        Some("bench") => {
            let real = load(&a[2]);
            describe(&real);
            bench(&real, Path::new(&a[3]));
        }
        Some("killtest") => {
            let real = load(&a[2]);
            let work = Path::new(&a[3]);
            if !work.join("query-cache.bin").exists() {
                store::build(&work.join("query-cache.bin"), &real).unwrap();
            }
            killtest(&real, work, a.get(4).map_or(10, |s| s.parse().unwrap()));
        }
        Some("writer") => {
            let real = real::load_files(Path::new(&a[2]), Path::new(&a[3])).unwrap();
            writer(&real, Path::new(&a[4]), a[5].parse().unwrap());
        }
        _ => {
            eprintln!(
                "usage: cacheproto describe|bench|killtest <session dir or crate dir prefix> [workdir] [trials]"
            );
            std::process::exit(1);
        }
    }
}
