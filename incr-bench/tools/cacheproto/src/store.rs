//! The proposed `query-cache.bin`: one RW-mmapped file with values, an in-file
//! hash index keyed by (kind, 16-byte key), append-only writes, 8-byte slot stores.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::{Mmap, MmapMut};

use crate::real::Real;

pub const HDR: u64 = 4096;
const MAGIC: u64 = u64::from_le_bytes(*b"QCINPLC1");
const H_MAGIC: usize = 0;
const H_WRITING: usize = 8;
const H_INDEX_OFF: usize = 16;
const H_INDEX_CAP: usize = 24;
const H_END: usize = 32;
const H_LIVE: usize = 40;
const H_DEAD: usize = 48;
const H_COMMITTED: usize = 56;
const H_NEXT_GEN: usize = 64;
const H_LOG_LEN: usize = 72;
const H_LOG: usize = 80;
const LOG_MAX: usize = (HDR as usize - H_LOG) / 16;
const SLOT: u64 = 32;
const ENTRY_HDR: usize = 32;
const OFF_BITS: u32 = 40;
/// A word with this bit names a tombstone entry, so a lookup never reads the entry to learn it.
#[allow(dead_code)]
const WORD_TOMBSTONE: u64 = 1 << (OFF_BITS - 1);
const OFF_MASK: u64 = (1 << (OFF_BITS - 1)) - 1;

pub fn word(generation: u64, off: u64) -> u64 {
	generation << OFF_BITS | off
}

fn hash(kind: u16, key: &[u8; 16]) -> u64 {
	let a = u64::from_le_bytes(key[..8].try_into().unwrap());
	let b = u64::from_le_bytes(key[8..].try_into().unwrap());
	(a ^ b.rotate_left(29) ^ (kind as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
		.wrapping_mul(0xFF51_AFD7_ED55_8CCD)
}

fn align8(v: u64) -> u64 {
	(v + 7) & !7
}

pub fn entry_size(len: u32) -> u64 {
	align8(ENTRY_HDR as u64 + len as u64)
}

fn entry_header(prev: u64, len: u32, kind: u16, flags: u16, key: &[u8; 16]) -> [u8; ENTRY_HDR] {
	let mut h = [0u8; ENTRY_HDR];
	h[0..8].copy_from_slice(&prev.to_le_bytes());
	h[8..12].copy_from_slice(&len.to_le_bytes());
	h[12..14].copy_from_slice(&kind.to_le_bytes());
	h[14..16].copy_from_slice(&flags.to_le_bytes());
	h[16..32].copy_from_slice(key);
	h
}

pub fn boot_tag() -> u64 {
	let id = std::fs::read("/proc/sys/kernel/random/boot_id").unwrap_or_default();
	let mut h = 0xcbf2_9ce4_8422_2325u64;
	for b in id {
		h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
	}
	h | 1
}

/// A mapping of the whole file. Atomics address the shared mapping directly.
pub struct View<M: AsRef<[u8]>> {
	pub map: M,
}

impl<M: AsRef<[u8]>> View<M> {
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
		u64::from_le_bytes(
			self.bytes()[off as usize..off as usize + 8]
				.try_into()
				.unwrap(),
		)
	}

	pub fn ancestors(&self) -> HashSet<u64> {
		let n = (self.h(H_LOG_LEN) as usize).min(LOG_MAX);
		let mut parent = std::collections::HashMap::new();
		for i in 0..n {
			parent.insert(self.h(H_LOG + i * 16), self.h(H_LOG + i * 16 + 8));
		}
		let mut set = HashSet::new();
		let mut g = self.h(H_COMMITTED);
		while g != 0 && set.insert(g) {
			g = parent.get(&g).copied().unwrap_or(0);
		}
		set
	}

	/// Returns the slot index and its current word.
	pub fn lookup(&self, kind: u16, key: &[u8; 16]) -> Option<(u64, u64)> {
		let off = self.h(H_INDEX_OFF);
		let cap = self.h(H_INDEX_CAP);
		let b = self.bytes();
		let mut i = hash(kind, key) & (cap - 1);
		loop {
			let s = (off + i * SLOT) as usize;
			let k = u32::from_le_bytes(b[s..s + 4].try_into().unwrap());
			if k == 0 {
				return None;
			}
			if k == kind as u32 + 1 && b[s + 8..s + 24] == key[..] {
				return Some((i, self.atom(s + 24).load(Ordering::Acquire)));
			}
			i = (i + 1) & (cap - 1);
		}
	}

	/// Follows `prev` until a word of the lineage. Returns the entry offset.
	pub fn resolve(&self, w: u64, anc: &HashSet<u64>) -> Option<u64> {
		self.resolve_gen(w, anc).map(|(o, _)| o)
	}

	/// Like `resolve`, and also returns the generation of the entry.
	pub fn resolve_gen(&self, mut w: u64, anc: &HashSet<u64>) -> Option<(u64, u64)> {
		let len = self.bytes().len() as u64;
		while w != 0 {
			let (g, off) = (w >> OFF_BITS, w & OFF_MASK);
			if off + ENTRY_HDR as u64 > len {
				return None;
			}
			if anc.contains(&g) {
				return (w & WORD_TOMBSTONE == 0).then_some((off, g));
			}
			w = self.u64_at(off);
		}
		None
	}

	/// Returns the value bytes of the entry at `off`.
	pub fn value(&self, off: u64) -> (u16, &[u8; 16], &[u8]) {
		let b = self.bytes();
		let o = off as usize;
		let len = u32::from_le_bytes(b[o + 8..o + 12].try_into().unwrap()) as usize;
		let kind = u16::from_le_bytes(b[o + 12..o + 14].try_into().unwrap());
		let key = b[o + 16..o + 32].try_into().unwrap();
		(kind, key, &b[o + ENTRY_HDR..o + ENTRY_HDR + len])
	}
}

fn store(v: &View<MmapMut>, f: usize, x: u64) {
	v.atom(f).store(x, Ordering::Release);
}

/// Writes `entries` (kind, key, bytes) into a fresh file with an index sized for them.
/// Returns the bytes written.
pub fn write_fresh<'a>(
	path: &Path,
	n: usize,
	entries: impl Iterator<Item = (u16, [u8; 16], &'a [u8])>,
) -> std::io::Result<u64> {
	let cap = (n as u64 * 2).next_power_of_two().max(1024);
	let data = HDR + cap * SLOT;
	let mut f = OpenOptions::new()
		.read(true)
		.write(true)
		.create(true)
		.truncate(true)
		.open(path)?;
	f.set_len(data)?;
	f.seek(SeekFrom::Start(data))?;
	let mut w = BufWriter::with_capacity(1 << 20, &mut f);
	let mut at = data;
	let mut keys = Vec::with_capacity(n);
	let mut live = 0;
	let pad = [0u8; 8];
	for (kind, key, bytes) in entries {
		let len = bytes.len() as u32;
		w.write_all(&entry_header(0, len, kind, 0, &key))?;
		w.write_all(bytes)?;
		let sz = entry_size(len);
		w.write_all(&pad[..(sz - ENTRY_HDR as u64 - len as u64) as usize])?;
		keys.push((kind, key, at));
		at += sz;
		live += sz;
	}
	w.flush()?;
	drop(w);
	let map = unsafe { MmapMut::map_mut(&f)? };
	let v = View { map };
	store(&v, H_INDEX_OFF, HDR);
	store(&v, H_INDEX_CAP, cap);
	let mut dup = 0;
	{
		let b = v.map.as_ref().as_ptr() as *mut u8;
		for (kind, key, off) in keys {
			let mut i = hash(kind, &key) & (cap - 1);
			loop {
				let s = (HDR + i * SLOT) as usize;
				let k = unsafe { std::ptr::read_unaligned(b.add(s) as *const u32) };
				if k == 0 {
					unsafe {
						std::ptr::write_unaligned(b.add(s) as *mut u32, kind as u32 + 1);
						std::ptr::copy_nonoverlapping(key.as_ptr(), b.add(s + 8), 16);
					}
					store(&v, s + 24, word(1, off));
					break;
				}
				if k == kind as u32 + 1 && v.map[s + 8..s + 24] == key[..] {
					dup += 1;
					break;
				}
				i = (i + 1) & (cap - 1);
			}
		}
	}
	assert_eq!(dup, 0, "duplicate keys");
	store(&v, H_END, at);
	store(&v, H_LIVE, live);
	store(&v, H_DEAD, 0);
	store(&v, H_LOG, 1);
	store(&v, H_LOG + 8, 0);
	store(&v, H_LOG_LEN, 1);
	store(&v, H_NEXT_GEN, 2);
	store(&v, H_COMMITTED, 1);
	store(&v, H_WRITING, 0);
	store(&v, H_MAGIC, MAGIC);
	Ok(at)
}

pub fn build(path: &Path, real: &Real) -> std::io::Result<u64> {
	write_fresh(
		path,
		real.values.len(),
		real.values
			.iter()
			.enumerate()
			.map(|(i, e)| (e.kind, e.key, real.payload(i))),
	)
}

pub enum Open {
	Clean,
	/// A writer of this boot died mid-write. Its bytes are dead, nothing else.
	KilledWriter,
	/// The flag names another boot: the OS crashed mid-write, so the pages may be torn.
	Discard,
}

pub fn check_open(v: &View<impl AsRef<[u8]>>) -> Result<Open, String> {
	if v.map.as_ref().len() < HDR as usize || v.h(H_MAGIC) != MAGIC {
		return Err("bad magic".into());
	}
	Ok(match v.h(H_WRITING) {
		0 => Open::Clean,
		t if t == boot_tag() => Open::KilledWriter,
		_ => Open::Discard,
	})
}

pub struct Edit {
	pub lookup_ns: u128,
	pub total_ns: u128,
	/// From the lock to the cleared flag: the appends, the slot stores and the end store.
	pub save_ns: u128,
	pub appended: u64,
	pub slot_stores: u64,
	pub dirty_index_pages: u64,
	pub undo: Option<Undo>,
}

pub struct Undo {
	len: u64,
	words: Vec<(usize, u64)>,
	hdr: Vec<(usize, u64)>,
}

/// The byte that `slow` writers XOR into the first byte of a value of generation `g`.
pub fn mark(g: u64) -> u8 {
	(g as u8) | 1
}

/// A session after a comment edit: open, look up every value, rewrite the
/// `picks`. `slow` adds a pause before each slot store so a kill can land
/// mid-publish, and marks each value with its generation. So a reader can
/// tell which it got.
pub fn edit(
	path: &Path,
	real: &Real,
	picks: &[usize],
	keep_undo: bool,
	slow: bool,
) -> Result<Edit, String> {
	let t0 = std::time::Instant::now();
	let f = OpenOptions::new()
		.read(true)
		.write(true)
		.open(path)
		.map_err(|e| e.to_string())?;
	let v = View {
		map: unsafe { MmapMut::map_mut(&f) }.map_err(|e| e.to_string())?,
	};
	if let Open::Discard = check_open(&v)? {
		return Err("written during an OS crash: discard".into());
	}
	let anc = v.ancestors();
	let mut sink = 0u64;
	for e in &real.values {
		if let Some((_, w)) = v.lookup(e.kind, &e.key) {
			sink = sink.wrapping_add(v.resolve(w, &anc).unwrap_or(0));
		}
	}
	std::hint::black_box(sink);
	let lookup_ns = t0.elapsed().as_nanos();
	if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
		return Err("cache locked by another writer: skip saving".into());
	}
	let hdr_fields = [
		H_END,
		H_LIVE,
		H_DEAD,
		H_COMMITTED,
		H_NEXT_GEN,
		H_LOG_LEN,
		H_WRITING,
	];
	let file_len = f.metadata().map_err(|e| e.to_string())?.len();
	let mut undo = Undo {
		len: file_len,
		words: Vec::new(),
		hdr: hdr_fields.iter().map(|&h| (h, v.h(h))).collect(),
	};
	let generation = v.h(H_NEXT_GEN);
	let committed = v.h(H_COMMITTED);
	store(&v, H_WRITING, boot_tag());
	store(&v, H_NEXT_GEN, generation + 1);
	// A dead writer may have left bytes past `end` that a slot names, so append past them.
	let base = align8(v.h(H_END).max(file_len));
	let mut buf = Vec::with_capacity(2 << 20);
	let mut flushed = 0u64;
	let mut updates = Vec::with_capacity(picks.len());
	let mut dead = 0;
	let pad = [0u8; 8];
	for &i in picks {
		let e = &real.values[i];
		let (slot, old) = v.lookup(e.kind, &e.key).ok_or("pick not in index")?;
		if let Some(o) = v.resolve(old, &anc) {
			dead += entry_size(v.value(o).2.len() as u32);
		}
		let bytes = real.payload(i);
		let off = base + flushed + buf.len() as u64;
		buf.extend_from_slice(&entry_header(old, e.len, e.kind, 0, &e.key));
		let at = buf.len();
		buf.extend_from_slice(bytes);
		if slow {
			buf[at] ^= mark(generation);
		}
		buf.extend_from_slice(
			&pad[..(entry_size(e.len) - ENTRY_HDR as u64 - e.len as u64) as usize],
		);
		updates.push((
			(v.h(H_INDEX_OFF) + slot * SLOT) as usize + 24,
			word(generation, off),
		));
		if buf.len() >= 1 << 20 {
			f.write_all_at(&buf, base + flushed)
				.map_err(|e| e.to_string())?;
			flushed += buf.len() as u64;
			buf.clear();
		}
	}
	f.write_all_at(&buf, base + flushed)
		.map_err(|e| e.to_string())?;
	flushed += buf.len() as u64;
	let mut pages = HashSet::new();
	for &(s, w) in &updates {
		if slow {
			std::thread::sleep(std::time::Duration::from_micros(200));
		}
		if keep_undo {
			undo.words.push((s, v.h(s)));
		}
		store(&v, s, w);
		pages.insert(s / 4096);
	}
	store(&v, H_END, base + flushed);
	store(&v, H_DEAD, v.h(H_DEAD) + dead);
	let n = v.h(H_LOG_LEN) as usize;
	if n < LOG_MAX {
		store(&v, H_LOG + n * 16, generation);
		store(&v, H_LOG + n * 16 + 8, committed);
		store(&v, H_LOG_LEN, n as u64 + 1);
	}
	store(&v, H_COMMITTED, generation);
	store(&v, H_WRITING, 0);
	let total_ns = t0.elapsed().as_nanos();
	Ok(Edit {
		lookup_ns,
		total_ns,
		save_ns: total_ns - lookup_ns,
		appended: flushed,
		slot_stores: updates.len() as u64,
		dirty_index_pages: pages.len() as u64,
		undo: keep_undo.then_some(undo),
	})
}

pub fn undo(path: &Path, u: Undo) -> std::io::Result<()> {
	let f = OpenOptions::new().read(true).write(true).open(path)?;
	{
		let v = View {
			map: unsafe { MmapMut::map_mut(&f)? },
		};
		for &(s, w) in u.words.iter().rev() {
			store(&v, s, w);
		}
		for &(h, x) in &u.hdr {
			store(&v, h, x);
		}
	}
	f.set_len(u.len)
}

pub fn ratio(path: &Path) -> std::io::Result<(u64, u64)> {
	let f = File::open(path)?;
	let v = View {
		map: unsafe { Mmap::map(&f)? },
	};
	Ok((v.h(H_LIVE), v.h(H_DEAD)))
}

/// Rewrites the live entries of the lineage into a fresh file, then renames it over `path`.
pub fn compact(path: &Path) -> Result<u64, String> {
	let f = File::open(path).map_err(|e| e.to_string())?;
	let v = View {
		map: unsafe { Mmap::map(&f) }.map_err(|e| e.to_string())?,
	};
	check_open(&v)?;
	let anc = v.ancestors();
	let off = v.h(H_INDEX_OFF);
	let cap = v.h(H_INDEX_CAP);
	let mut live = Vec::new();
	for i in 0..cap {
		let w = v
			.atom((off + i * SLOT) as usize + 24)
			.load(Ordering::Acquire);
		if let Some(o) = v.resolve(w, &anc) {
			live.push(o);
		}
	}
	// File order keeps the copy sequential.
	live.sort_unstable();
	let tmp = path.with_extension("compact");
	let n = write_fresh(
		&tmp,
		live.len(),
		live.iter().map(|&o| {
			let (k, key, b) = v.value(o);
			(k, *key, b)
		}),
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
	pub foreign_words: usize,
	pub file_len: u64,
	pub end: u64,
}

/// Reads every value of `real` through the lineage and compares it with the real bytes.
/// `marked` says that every generation after the first came from a `slow` writer.
pub fn verify(path: &Path, real: &Real, marked: bool) -> Result<Verify, String> {
	let f = File::open(path).map_err(|e| e.to_string())?;
	let v = View {
		map: unsafe { Mmap::map(&f) }.map_err(|e| e.to_string())?,
	};
	let state = match check_open(&v)? {
		Open::Clean => "clean",
		Open::KilledWriter => "writer killed mid-write (accepted)",
		Open::Discard => return Err("flag from another boot: discard".into()),
	};
	let anc = v.ancestors();
	let (mut found, mut missing, mut mismatched, mut foreign) = (0, 0, 0, 0);
	for (i, e) in real.values.iter().enumerate() {
		let Some((_, w)) = v.lookup(e.kind, &e.key) else {
			missing += 1;
			continue;
		};
		if !anc.contains(&(w >> OFF_BITS)) {
			foreign += 1;
		}
		match v.resolve_gen(w, &anc) {
			None => missing += 1,
			Some((o, g)) => {
				let (k, key, b) = v.value(o);
				let want = real.payload(i);
				let m = if marked && g > 1 { mark(g) } else { 0 };
				let same = b.len() == want.len()
					&& !b.is_empty()
					&& b[0] == want[0] ^ m
					&& b[1..] == want[1..];
				if k == e.kind && *key == e.key && same {
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
		foreign_words: foreign,
		file_len: v.map.len() as u64,
		end: v.h(H_END),
	})
}
