//! The proposed `query-cache.bin`: one file with values, an in-file hash index of
//! 8-byte words, and a chain of delta blocks. A session appends its values and one
//! delta block, then commits with one 8-byte store of the chain head.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::{Mmap, MmapMut};

use crate::real::Real;

pub const HDR: u64 = 4096;
const MAGIC: u64 = u64::from_le_bytes(*b"QCINPLC2");
const H_MAGIC: usize = 0;
const H_WRITING: usize = 8;
const H_INDEX_OFF: usize = 16;
const H_INDEX_CAP: usize = 24;
const H_END: usize = 32;
const H_LIVE: usize = 40;
const H_DEAD: usize = 48;
/// The committed generation while the chain is empty.
const H_BASE_GEN: usize = 56;
const H_NEXT_GEN: usize = 64;
/// The commit word: the offset of the newest delta block, or zero.
const H_HEAD: usize = 72;
const H_USED: usize = 80;
const H_TABLES: usize = 88;

/// Index slots every few values. A lookup that misses probes slots at this load.
pub const LOAD_PCT: u64 = 85;
pub const MERGE_DIV: u64 = 16;
pub const CHECK: usize = 8;

const OFF_BITS: u32 = 34;
const GEN_BITS: u32 = 12;
const GEN_SHIFT: u32 = OFF_BITS;
const HAS_PREV: u64 = 1 << (OFF_BITS + GEN_BITS);
#[allow(dead_code)]
const TOMB: u64 = 1 << (OFF_BITS + GEN_BITS + 1);
const TAG_SHIFT: u32 = 48;
pub const GEN_MAX: u64 = (1 << GEN_BITS) - 1;

/// Block header: prev block, generation, entries, live, dead, entries in the chain.
const BLK_HDR: u64 = 48;

pub fn hashes(kind: u16, key: &[u8; 16]) -> (u64, u64) {
    let a = u64::from_le_bytes(key[..8].try_into().unwrap());
    let b = u64::from_le_bytes(key[8..].try_into().unwrap());
    let k = kind as u64;
    let mix = |x: u64| {
        let x = (x ^ (x >> 33)).wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        let x = (x ^ (x >> 33)).wrapping_mul(0xC4CE_B9FE_1A85_EC53);
        x ^ (x >> 33)
    };
    let h = mix(a ^ mix(b ^ k.wrapping_mul(0x9E37_79B9_7F4A_7C15)));
    let h2 = mix(b.rotate_left(17) ^ mix(a ^ k.wrapping_add(0x632B_E59B_D9B4_E019)));
    (h, h2)
}

fn slot_of(h: u64, cap: u64) -> u64 {
    ((h as u128 * cap as u128) >> 64) as u64
}

fn tag_of(h: u64) -> u64 {
    h & 0xffff
}

pub fn word(h: u64, generation: u64, off: u64, flags: u64) -> u64 {
    debug_assert!(off < 1 << OFF_BITS && (1..=GEN_MAX).contains(&generation));
    tag_of(h) << TAG_SHIFT | flags | generation << GEN_SHIFT | off
}

fn gen_of(w: u64) -> u64 {
    (w >> GEN_SHIFT) & GEN_MAX
}

fn off_of(w: u64) -> u64 {
    w & ((1 << OFF_BITS) - 1)
}

pub fn leb_len(mut v: u64) -> usize {
    let mut n = 1;
    while v >= 0x80 {
        v >>= 7;
        n += 1;
    }
    n
}

fn put_leb(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push(v as u8 | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

pub fn entry_size(len: usize, prev: bool) -> u64 {
    (if prev { 8 } else { 0 } + CHECK + leb_len(len as u64) + len) as u64
}

fn put_entry(buf: &mut Vec<u8>, prev: u64, h2: u64, bytes: &[u8]) -> usize {
    if prev != 0 {
        buf.extend_from_slice(&prev.to_le_bytes());
    }
    buf.extend_from_slice(&h2.to_le_bytes()[..CHECK]);
    put_leb(buf, bytes.len() as u64);
    let at = buf.len();
    buf.extend_from_slice(bytes);
    at
}

pub fn boot_tag() -> u64 {
    let id = std::fs::read("/proc/sys/kernel/random/boot_id").unwrap_or_default();
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in id {
        h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
    }
    h | 1
}

/// A reader's snapshot: the mapping, the committed generation and the chain as a map.
pub struct View<M: AsRef<[u8]>> {
    pub map: M,
    pub committed: u64,
    pub head: u64,
    /// Hash of the key to the newest committed word of the chain.
    pub delta: HashMap<u64, u64>,
    pub chain_n: u64,
}

pub struct Block {
    pub prev: u64,
    pub generation: u64,
    pub n: u64,
    pub live: u64,
    pub dead: u64,
    pub chain_n: u64,
}

impl<M: AsRef<[u8]>> View<M> {
    pub fn open(map: M) -> Result<Self, String> {
        let mut v = View { map, committed: 0, head: 0, delta: HashMap::new(), chain_n: 0 };
        if v.bytes().len() < HDR as usize || v.h(H_MAGIC) != MAGIC {
            return Err("bad magic".into());
        }
        // The head load comes first. A merge stores the base generation before it clears the head.
        v.head = v.h(H_HEAD);
        v.committed = if v.head == 0 { v.h(H_BASE_GEN) } else { v.block(v.head).generation };
        let mut b = v.head;
        let mut first = true;
        while b != 0 {
            let blk = v.block(b);
            if first {
                v.chain_n = blk.chain_n;
                first = false;
            }
            for i in 0..blk.n {
                let e = b + BLK_HDR + i * 16;
                let (h, w) = (v.u64_at(e), v.u64_at(e + 8));
                v.delta.entry(h).or_insert(w);
            }
            b = blk.prev;
        }
        Ok(v)
    }

    fn bytes(&self) -> &[u8] {
        self.map.as_ref()
    }

    pub fn atom(&self, off: usize) -> &AtomicU64 {
        let b = self.bytes();
        assert!(off % 8 == 0 && off + 8 <= b.len());
        // The mapping is shared with other processes. The atomic is the only access to this word.
        unsafe { &*(b.as_ptr().add(off) as *const AtomicU64) }
    }

    pub fn h(&self, f: usize) -> u64 {
        self.atom(f).load(Ordering::Acquire)
    }

    fn u64_at(&self, off: u64) -> u64 {
        let o = off as usize;
        u64::from_le_bytes(self.bytes()[o..o + 8].try_into().unwrap())
    }

    pub fn block(&self, off: u64) -> Block {
        let r = |i: u64| self.u64_at(off + i * 8);
        Block { prev: r(0), generation: r(1), n: r(2), live: r(3), dead: r(4), chain_n: r(5) }
    }

    /// Live and dead entry bytes as of the snapshot.
    pub fn accounting(&self) -> (u64, u64) {
        if self.head == 0 {
            (self.h(H_LIVE), self.h(H_DEAD))
        } else {
            let b = self.block(self.head);
            (b.live, b.dead)
        }
    }

    fn check_at(&self, w: u64) -> Option<u64> {
        let p = (off_of(w) + if w & HAS_PREV != 0 { 8 } else { 0 }) as usize;
        let b = self.bytes();
        if p + CHECK > b.len() {
            return None;
        }
        let mut c = [0u8; 8];
        c[..CHECK].copy_from_slice(&b[p..p + CHECK]);
        Some(u64::from_le_bytes(c))
    }

    fn check_ok(&self, w: u64, h2: u64) -> bool {
        let mask = if CHECK == 8 { u64::MAX } else { (1u64 << (CHECK * 8)) - 1 };
        self.check_at(w) == Some(h2 & mask)
    }

    /// The index slot that holds the key, or the empty slot where it would go, and its word.
    pub fn main_slot(&self, h: u64, h2: u64) -> (u64, Option<u64>) {
        let off = self.h(H_INDEX_OFF);
        let cap = self.h(H_INDEX_CAP);
        let mut i = slot_of(h, cap);
        loop {
            let w = self.atom((off + i * 8) as usize).load(Ordering::Acquire);
            if w == 0 {
                return (i, None);
            }
            if w >> TAG_SHIFT == tag_of(h) && self.check_ok(w, h2) {
                return (i, Some(w));
            }
            i += 1;
            if i == cap {
                i = 0;
            }
        }
    }

    /// The newest word of the key: the chain first, then the index.
    pub fn lookup(&self, kind: u16, key: &[u8; 16]) -> Option<u64> {
        let (h, h2) = hashes(kind, key);
        if let Some(&w) = self.delta.get(&h) {
            if self.check_ok(w, h2) {
                return Some(w);
            }
        }
        self.main_slot(h, h2).1
    }

    /// Follows `prev` to a word of the snapshot. Returns `None` for a tombstone or a newer key.
    pub fn resolve(&self, mut w: u64) -> Option<u64> {
        while w != 0 {
            if gen_of(w) <= self.committed {
                return (w & TOMB == 0).then_some(w);
            }
            if w & HAS_PREV == 0 {
                return None;
            }
            w = self.u64_at(off_of(w));
        }
        None
    }

    pub fn value(&self, w: u64) -> &[u8] {
        let b = self.bytes();
        let mut p = (off_of(w) + if w & HAS_PREV != 0 { 8 } else { 0 }) as usize + CHECK;
        let mut len = 0usize;
        let mut s = 0;
        loop {
            let x = b[p];
            p += 1;
            len |= ((x & 0x7f) as usize) << s;
            if x & 0x80 == 0 {
                break;
            }
            s += 7;
        }
        &b[p..p + len]
    }

    pub fn entry_bytes(&self, w: u64) -> u64 {
        entry_size(self.value(w).len(), w & HAS_PREV != 0)
    }
}

fn store(v: &View<MmapMut>, f: usize, x: u64) {
    v.atom(f).store(x, Ordering::Release);
}

fn map_rw(f: &File) -> Result<View<MmapMut>, String> {
    View::open(unsafe { MmapMut::map_mut(f) }.map_err(|e| e.to_string())?)
}

fn map_ro(f: &File) -> Result<View<Mmap>, String> {
    View::open(unsafe { Mmap::map(f) }.map_err(|e| e.to_string())?)
}

/// Writes `entries` (kind, key, bytes) into a fresh file, then `tables` bytes that stand
/// for the per-session tables. Returns the file size.
pub fn write_fresh<'a>(
    path: &Path,
    n: usize,
    entries: impl Iterator<Item = (u16, [u8; 16], &'a [u8])>,
    tables: u64,
) -> std::io::Result<u64> {
    let cap = (n as u64 * 100).div_ceil(LOAD_PCT).max(64);
    let data = HDR + cap * 8;
    let mut f = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(path)?;
    f.set_len(data)?;
    f.seek(SeekFrom::Start(data))?;
    let mut w = BufWriter::with_capacity(1 << 20, &mut f);
    let mut at = data;
    let mut keys = Vec::with_capacity(n);
    let mut live = 0;
    let mut buf = Vec::new();
    for (kind, key, bytes) in entries {
        let (h, h2) = hashes(kind, &key);
        buf.clear();
        put_entry(&mut buf, 0, h2, bytes);
        w.write_all(&buf)?;
        keys.push((h, at));
        at += buf.len() as u64;
        live += buf.len() as u64;
    }
    let tables_at = at;
    std::io::copy(&mut std::io::repeat(0).take(tables), &mut w)?;
    at += tables;
    w.flush()?;
    drop(w);
    // Slot order makes the index writes sequential.
    keys.sort_unstable_by_key(|&(h, _)| slot_of(h, cap));
    let mut m = unsafe { MmapMut::map_mut(&f)? };
    let idx = &mut m[HDR as usize..data as usize];
    let mut used = 0;
    for (h, off) in keys {
        let mut i = slot_of(h, cap) as usize;
        while idx[i * 8..i * 8 + 8] != [0; 8] {
            i = (i + 1) % cap as usize;
        }
        idx[i * 8..i * 8 + 8].copy_from_slice(&word(h, 1, off, 0).to_le_bytes());
        used += 1;
    }
    let hdr = [
        (H_INDEX_OFF, HDR),
        (H_INDEX_CAP, cap),
        (H_END, at),
        (H_LIVE, live),
        (H_DEAD, 0),
        (H_BASE_GEN, 1),
        (H_NEXT_GEN, 2),
        (H_HEAD, 0),
        (H_USED, used),
        (H_TABLES, tables_at),
        (H_WRITING, 0),
        (H_MAGIC, MAGIC),
    ];
    for (f, x) in hdr {
        m[f..f + 8].copy_from_slice(&x.to_le_bytes());
    }
    Ok(at)
}

pub fn build(path: &Path, real: &Real) -> std::io::Result<u64> {
    write_fresh(
        path,
        real.values.len(),
        real.values.iter().enumerate().map(|(i, e)| (e.kind, e.key, real.inner(i))),
        real.tables_bytes(),
    )
}

pub enum Open {
    Clean,
    /// A writer of this boot died mid-write. Its bytes are unreachable, nothing else.
    KilledWriter,
    /// The flag names another boot: the OS crashed mid-write, so the pages may be torn.
    Discard,
}

pub fn check_open(v: &View<impl AsRef<[u8]>>) -> Open {
    match v.h(H_WRITING) {
        0 => Open::Clean,
        t if t == boot_tag() => Open::KilledWriter,
        _ => Open::Discard,
    }
}

pub struct Edit {
    pub lookup_ns: u128,
    pub total_ns: u128,
    /// From the lock to the cleared flag, the merge included.
    pub save_ns: u128,
    pub merge_ns: u128,
    pub appended: u64,
    pub merged_words: u64,
    pub dirty_index_pages: u64,
    pub undo: Option<Undo>,
}

pub struct Undo {
    len: u64,
    words: Vec<(usize, u64)>,
    hdr: Vec<(usize, u64)>,
}

/// The chain size at which a session merges it into the index.
pub fn default_merge_at(real: &Real) -> u64 {
    real.values.len() as u64 / MERGE_DIV
}

/// The byte that `slow` writers XOR into the first byte of a value of generation `g`.
pub fn mark(g: u64) -> u8 {
    (g as u8) | 1
}

/// A session: open, look up every value, rewrite the `picks`. `slow` writes one
/// entry per call with pauses, so a kill can land mid-write, and marks each value
/// with its generation.
pub fn edit(
    path: &Path,
    real: &Real,
    picks: &[usize],
    keep_undo: bool,
    slow: bool,
    merge_at: u64,
) -> Result<Edit, String> {
    let t0 = std::time::Instant::now();
    let f = OpenOptions::new().read(true).write(true).open(path).map_err(|e| e.to_string())?;
    let v = map_rw(&f)?;
    if let Open::Discard = check_open(&v) {
        return Err("written during an OS crash: discard".into());
    }
    let mut sink = 0u64;
    for e in &real.values {
        if let Some(w) = v.lookup(e.kind, &e.key).and_then(|w| v.resolve(w)) {
            sink = sink.wrapping_add(v.value(w).len() as u64);
        }
    }
    std::hint::black_box(sink);
    let lookup_ns = t0.elapsed().as_nanos();
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("cache locked by another writer: skip saving".into());
    }
    let file_len = f.metadata().map_err(|e| e.to_string())?.len();
    let fields = [H_END, H_LIVE, H_DEAD, H_BASE_GEN, H_NEXT_GEN, H_HEAD, H_USED, H_WRITING];
    let mut undo = Undo {
        len: file_len,
        words: Vec::new(),
        hdr: fields.iter().map(|&h| (h, v.h(h))).collect(),
    };
    let generation = v.h(H_NEXT_GEN);
    if generation > GEN_MAX {
        return Err("generations used up: compact".into());
    }
    store(&v, H_WRITING, boot_tag());
    store(&v, H_NEXT_GEN, generation + 1);
    // A dead writer may have left bytes past `end`, so append past them.
    let base = v.h(H_END).max(file_len);
    let (mut live, mut dead) = v.accounting();
    let mut buf = Vec::with_capacity(2 << 20);
    let mut flushed = 0u64;
    let mut delta = Vec::with_capacity(picks.len());
    for &i in picks {
        let e = &real.values[i];
        let (h, h2) = hashes(e.kind, &e.key);
        let old = v.lookup(e.kind, &e.key).unwrap_or(0);
        if let Some(o) = v.resolve(old) {
            let sz = v.entry_bytes(o);
            live -= sz;
            dead += sz;
        }
        let off = base + flushed + buf.len() as u64;
        let at = put_entry(&mut buf, old, h2, real.inner(i));
        if slow && at < buf.len() {
            buf[at] ^= mark(generation);
        }
        live += entry_size(e.inner as usize, old != 0);
        delta.push((h, word(h, generation, off, if old != 0 { HAS_PREV } else { 0 })));
        if buf.len() >= 1 << 20 || slow {
            f.write_all_at(&buf, base + flushed).map_err(|e| e.to_string())?;
            flushed += buf.len() as u64;
            buf.clear();
            if slow {
                std::thread::sleep(std::time::Duration::from_micros(100));
            }
        }
    }
    f.write_all_at(&buf, base + flushed).map_err(|e| e.to_string())?;
    flushed += buf.len() as u64;
    delta.sort_unstable();
    delta.dedup_by_key(|d| d.0);
    let blk = (base + flushed).next_multiple_of(8);
    let chain_n = v.chain_n + delta.len() as u64;
    let mut b = Vec::with_capacity(BLK_HDR as usize + delta.len() * 16);
    for x in [v.head, generation, delta.len() as u64, live, dead, chain_n] {
        b.extend_from_slice(&x.to_le_bytes());
    }
    for &(h, w) in &delta {
        b.extend_from_slice(&h.to_le_bytes());
        b.extend_from_slice(&w.to_le_bytes());
    }
    f.write_all_at(&b, blk).map_err(|e| e.to_string())?;
    let end = blk + b.len() as u64;
    drop(v);
    // The new mapping covers the new block before the block becomes the head.
    let v = map_rw(&f)?;
    store(&v, H_END, end);
    store(&v, H_HEAD, blk);
    let tm = std::time::Instant::now();
    let (mut merged, mut pages) = (0, 0);
    if chain_n > merge_at {
        let v = map_rw(&f)?;
        (merged, pages) = merge(&v, keep_undo.then_some(&mut undo.words), slow)?;
    }
    let merge_ns = tm.elapsed().as_nanos();
    store(&v, H_WRITING, 0);
    let total_ns = t0.elapsed().as_nanos();
    Ok(Edit {
        lookup_ns,
        total_ns,
        save_ns: total_ns - lookup_ns,
        merge_ns,
        appended: end - base,
        merged_words: merged,
        dirty_index_pages: pages,
        undo: keep_undo.then_some(undo),
    })
}

/// Stores the chain's words into the index in slot order, then clears the chain.
/// A kill midway leaves the chain in place, and the next merge does it again.
fn merge(
    v: &View<MmapMut>,
    mut undo: Option<&mut Vec<(usize, u64)>>,
    slow: bool,
) -> Result<(u64, u64), String> {
    let off = v.h(H_INDEX_OFF);
    let cap = v.h(H_INDEX_CAP);
    let mut todo = Vec::with_capacity(v.delta.len());
    for (&h, &w) in &v.delta {
        let h2 = v.check_at(w).ok_or("delta word past end")?;
        let (slot, old) = v.main_slot(h, h2);
        todo.push((slot, w, old.is_none(), h, h2));
    }
    todo.sort_unstable();
    let mut pages = std::collections::HashSet::new();
    let mut inserted = 0;
    for (n, &(slot, w, new, h, h2)) in todo.iter().enumerate() {
        // An insert probes again: an earlier insert of this merge can hold its empty slot now.
        let slot = if new { v.main_slot(h, h2).0 } else { slot };
        let s = (off + slot * 8) as usize;
        let old = v.h(s);
        if old == 0 {
            inserted += 1;
        }
        if let Some(u) = undo.as_mut() {
            u.push((s, old));
        }
        store(v, s, w);
        pages.insert(s / 4096);
        if slow && n % 64 == 0 {
            std::thread::sleep(std::time::Duration::from_micros(100));
        }
    }
    let b = v.block(v.head);
    store(v, H_USED, v.h(H_USED) + inserted);
    if v.h(H_USED) * 100 > cap * 92 {
        return Err("index full: compact".into());
    }
    store(v, H_LIVE, b.live);
    store(v, H_DEAD, b.dead);
    store(v, H_BASE_GEN, b.generation);
    store(v, H_HEAD, 0);
    Ok((todo.len() as u64, pages.len() as u64))
}

pub fn undo(path: &Path, u: Undo) -> std::io::Result<()> {
    let f = OpenOptions::new().read(true).write(true).open(path)?;
    {
        let m = unsafe { MmapMut::map_mut(&f)? };
        let v = View { map: m, committed: 0, head: 0, delta: HashMap::new(), chain_n: 0 };
        for &(s, w) in u.words.iter().rev() {
            store(&v, s, w);
        }
        for &(h, x) in &u.hdr {
            store(&v, h, x);
        }
    }
    f.set_len(u.len)
}

pub fn ratio(path: &Path) -> Result<(u64, u64), String> {
    let f = File::open(path).map_err(|e| e.to_string())?;
    Ok(map_ro(&f)?.accounting())
}

/// Copies the live value of every current key into a fresh file and renames it over `path`.
/// rustc drives this from the keys of its current dep graph, which also drops the keys of
/// nodes that no longer exist.
pub fn compact(path: &Path, real: &Real) -> Result<u64, String> {
    let f = File::open(path).map_err(|e| e.to_string())?;
    let v = map_ro(&f)?;
    if let Open::Discard = check_open(&v) {
        return Err("written during an OS crash: discard".into());
    }
    let mut live = Vec::with_capacity(real.values.len());
    for e in &real.values {
        if let Some(w) = v.lookup(e.kind, &e.key).and_then(|w| v.resolve(w)) {
            live.push((off_of(w), e.kind, e.key, w));
        }
    }
    // File order keeps the copy sequential.
    live.sort_unstable_by_key(|x| x.0);
    let tmp = path.with_extension("compact");
    let n = write_fresh(
        &tmp,
        live.len(),
        live.iter().map(|&(_, k, key, w)| (k, key, v.value(w))),
        real.tables_bytes(),
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(n)
}

pub struct Verify {
    pub state: &'static str,
    pub found: usize,
    pub missing: usize,
    pub mismatched: usize,
    pub chain_n: u64,
    pub file_len: u64,
    pub end: u64,
}

/// Reads every value of `real` through the snapshot and compares it with the real bytes.
/// `marked` says that every generation after the first came from a `slow` writer.
pub fn verify(path: &Path, real: &Real, marked: bool) -> Result<Verify, String> {
    let f = File::open(path).map_err(|e| e.to_string())?;
    let v = map_ro(&f)?;
    let state = match check_open(&v) {
        Open::Clean => "clean",
        Open::KilledWriter => "writer killed mid-write (accepted)",
        Open::Discard => return Err("flag from another boot: discard".into()),
    };
    let (mut found, mut missing, mut mismatched) = (0, 0, 0);
    for (i, e) in real.values.iter().enumerate() {
        match v.lookup(e.kind, &e.key).and_then(|w| v.resolve(w)) {
            None => missing += 1,
            Some(w) => {
                let b = v.value(w);
                let want = real.inner(i);
                let g = gen_of(w);
                let m = if marked && g > 1 { mark(g) } else { 0 };
                let same = b.len() == want.len()
                    && (b.is_empty() || (b[0] == want[0] ^ m && b[1..] == want[1..]));
                if same {
                    found += 1;
                } else {
                    mismatched += 1;
                }
            }
        }
    }
    Ok(Verify {
        state,
        found,
        missing,
        mismatched,
        chain_n: v.chain_n,
        file_len: v.map.len() as u64,
        end: v.h(H_END),
    })
}
