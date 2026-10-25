//! Read-only views of a real rustc session: `query-cache.bin` and `dep-graph.bin`.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use memmap2::Mmap;

pub struct Cur<'a> {
	pub b: &'a [u8],
	pub p: usize,
	pub end: usize,
}

impl<'a> Cur<'a> {
	pub fn uleb(&mut self) -> Result<u64, String> {
		let mut r = 0u64;
		let mut s = 0;
		loop {
			if self.p >= self.end || s > 63 {
				return Err(format!("bad uleb at {}", self.p));
			}
			let x = self.b[self.p];
			self.p += 1;
			r |= ((x & 0x7f) as u64) << s;
			if x & 0x80 == 0 {
				return Ok(r);
			}
			s += 7;
		}
	}

	pub fn sleb(&mut self) -> Result<i64, String> {
		let mut r = 0i64;
		let mut s = 0;
		loop {
			if self.p >= self.end || s > 63 {
				return Err(format!("bad sleb at {}", self.p));
			}
			let x = self.b[self.p];
			self.p += 1;
			r |= ((x & 0x7f) as i64) << s;
			s += 7;
			if x & 0x80 == 0 {
				if s < 64 && x & 0x40 != 0 {
					r |= -1i64 << s;
				}
				return Ok(r);
			}
		}
	}

	/// Skips a leb128 of any width, such as the u128 footer tag.
	pub fn skip_leb(&mut self) -> Result<(), String> {
		loop {
			let x = *self.b[..self.end].get(self.p).ok_or("leb past end")?;
			self.p += 1;
			if x & 0x80 == 0 {
				return Ok(());
			}
		}
	}

	pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
		if self.p + n > self.end {
			return Err(format!("read of {n} past end at {}", self.p));
		}
		let s = &self.b[self.p..self.p + n];
		self.p += n;
		Ok(s)
	}
}

/// One cached query value of the real file.
#[derive(Clone, Copy)]
pub struct Ent {
	pub index: u32,
	pub kind: u16,
	pub key: [u8; 16],
	pub pos: u64,
	pub len: u32,
}

pub struct Real {
	pub qc: Mmap,
	pub qf: File,
	pub gf: File,
	pub qc_path: PathBuf,
	pub dg_path: PathBuf,
	pub values: Vec<Ent>,
	pub side_effects: usize,
	pub allocs: usize,
	pub hygiene: usize,
	pub values_without_node: usize,
	pub kind_bits: u32,
}

impl Real {
	pub fn payload(&self, i: usize) -> &[u8] {
		let e = &self.values[i];
		&self.qc[e.pos as usize..e.pos as usize + e.len as usize]
	}

	pub fn data_start(&self) -> usize {
		7 + self.qc[6] as usize
	}
}

/// Returns `(kind, key)` per node index. Kind a value of zero marks an index with no node.
/// `FileEncoder::finish` ends every file with this marker.
const END_MARKER: &[u8] = b"rust-end-file";

fn strip_end(b: &[u8]) -> Result<&[u8], String> {
	b.strip_suffix(END_MARKER)
		.ok_or_else(|| "no rust-end-file marker".into())
}

fn parse_graph(b: &[u8], kind_bits: u32) -> Result<Vec<(u16, [u8; 16])>, String> {
	let b = strip_end(b)?;
	if b.len() < 7 + 24 || b.len() < 7 + b[6] as usize + 8 + 24 {
		return Err("too short".into());
	}
	let start = 7 + b[6] as usize + 8;
	let end = b.len() - 24;
	let rd = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
	let (node_max, node_count, edge_count) = (rd(end), rd(end + 8), rd(end + 16));
	if node_max > end as u64 || node_count > node_max {
		return Err("bad counts".into());
	}
	let mut nodes = vec![(0u16, [0u8; 16]); node_max as usize];
	let mut c = Cur { b, p: start, end };
	let kmask = (1u16 << kind_bits) - 1;
	let mut prev: Option<u32> = None;
	let mut edges = 0u64;
	for _ in 0..node_count {
		let h = c.bytes(3)?;
		let head = u16::from_le_bytes([h[0], h[1]]);
		let flags = h[2];
		let index = if flags & 1 != 0 {
			prev.ok_or("index-is-next without previous")? + 1
		} else {
			u32::from_le_bytes(c.bytes(4)?.try_into().unwrap())
		};
		if index as u64 >= node_max || nodes[index as usize].0 != 0 {
			return Err(format!("bad index {index}"));
		}
		let key: [u8; 16] = if flags & 2 != 0 {
			let d = c.uleb()? as u32;
			let t = index
				.checked_sub(d)
				.filter(|_| d != 0)
				.ok_or("bad key ref")?;
			if nodes[t as usize].0 == 0 {
				return Err("key ref to missing node".into());
			}
			nodes[t as usize].1
		} else {
			c.bytes(16)?.try_into().unwrap()
		};
		if flags & 8 == 0 {
			if flags & 4 != 0 {
				c.uleb()?;
			} else {
				c.bytes(16)?;
			}
		}
		let kind = head & kmask;
		if kind == 0 {
			return Err("null kind".into());
		}
		let lenfield = (head as u32) >> (kind_bits + 2);
		let n = if lenfield == 0 {
			c.uleb()?
		} else {
			(lenfield - 1) as u64
		};
		for _ in 0..n {
			c.sleb()?;
		}
		edges += n;
		nodes[index as usize] = (kind, key);
		prev = Some(index);
	}
	if edges != edge_count {
		return Err("edge count mismatch".into());
	}
	let mut nums = Vec::new();
	while c.p < end {
		nums.push(c.uleb()?);
	}
	if nums.len() < 2 || nums[..nums.len() - 1].iter().sum::<u64>() != node_count {
		return Err("kind count mismatch".into());
	}
	Ok(nodes)
}

/// The footer's `query_values_index` as rustc loads it, plus every region boundary.
pub struct Footer {
	pub values: Vec<(u32, u64)>,
	pub side: Vec<(u32, u64)>,
	pub allocs: Vec<u64>,
	pub hygiene: Vec<u64>,
	pub footer_pos: u64,
}

pub fn parse_footer(b: &[u8]) -> Result<Footer, String> {
	let b = strip_end(b)?;
	let footer_pos = u64::from_le_bytes(b[b.len() - 8..].try_into().unwrap());
	let mut c = Cur {
		b,
		p: footer_pos as usize,
		end: b.len() - 8,
	};
	c.skip_leb()?;
	let nf = c.uleb()?;
	for _ in 0..nf {
		c.uleb()?;
		c.bytes(24)?;
	}
	let mut lists = [Vec::new(), Vec::new()];
	for l in &mut lists {
		let n = c.uleb()?;
		l.reserve(n as usize);
		for _ in 0..n {
			let i = c.uleb()? as u32;
			let p = c.uleb()?;
			l.push((i, p));
		}
	}
	let [values, side] = lists;
	let n = c.uleb()?;
	let mut allocs = Vec::with_capacity(n as usize);
	for _ in 0..n {
		allocs.push(c.uleb()?);
	}
	let mut hygiene = Vec::new();
	let n = c.uleb()?;
	for _ in 0..n {
		c.uleb()?;
		hygiene.push(c.uleb()?);
	}
	let n = c.uleb()?;
	for _ in 0..n {
		c.bytes(16)?;
		hygiene.push(c.uleb()?);
	}
	let n = c.uleb()?;
	for _ in 0..n {
		c.bytes(16)?;
		c.uleb()?;
	}
	c.uleb()?;
	if c.p != b.len() - 8 {
		return Err(format!("footer ended at {}, want {}", c.p, b.len() - 8));
	}
	Ok(Footer {
		values,
		side,
		allocs,
		hygiene,
		footer_pos,
	})
}

/// Today's load step: decode the footer and build the index map rustc keeps.
pub fn today_index(b: &[u8]) -> HashMap<u32, u64> {
	parse_footer(b).unwrap().values.into_iter().collect()
}

pub fn find_session(crate_dir_glob: &Path) -> Result<PathBuf, String> {
	let parent = crate_dir_glob.parent().ok_or("no parent")?;
	let prefix = crate_dir_glob
		.file_name()
		.unwrap()
		.to_string_lossy()
		.into_owned();
	let mut best = None;
	for d in std::fs::read_dir(parent).map_err(|e| e.to_string())? {
		let d = d.map_err(|e| e.to_string())?;
		let name = d.file_name().to_string_lossy().into_owned();
		if !name.starts_with(&prefix) || !d.path().is_dir() {
			continue;
		}
		for s in std::fs::read_dir(d.path()).map_err(|e| e.to_string())? {
			let s = s.map_err(|e| e.to_string())?.path();
			if s.is_dir() && s.join("query-cache.bin").exists() && s.join("dep-graph.bin").exists()
			{
				best = Some(s);
			}
		}
	}
	best.ok_or_else(|| format!("no finalized session under {}*", crate_dir_glob.display()))
}

pub fn load(session: &Path) -> Result<Real, String> {
	load_files(
		&session.join("query-cache.bin"),
		&session.join("dep-graph.bin"),
	)
}

/// The open files stay in `Real`, so a child process can read them via `/proc/<pid>/fd`
/// after rustc deletes the session dir.
pub fn load_files(qc_path: &Path, dg_path: &Path) -> Result<Real, String> {
	let (qc_path, dg_path) = (qc_path.to_path_buf(), dg_path.to_path_buf());
	// Both files open before either is read, so a replaced session dir cannot mix sessions.
	let qf = File::open(&qc_path).map_err(|e| format!("{}: {e}", qc_path.display()))?;
	let gf = File::open(&dg_path).map_err(|e| format!("{}: {e}", dg_path.display()))?;
	let qc = unsafe { Mmap::map(&qf) }.map_err(|e| e.to_string())?;
	let dg = unsafe { Mmap::map(&gf) }.map_err(|e| e.to_string())?;
	let mut errs = Vec::new();
	let mut found = None;
	for kb in 7..=12 {
		match parse_graph(&dg, kb) {
			Ok(n) => {
				found = Some((kb, n));
				break;
			}
			Err(e) => errs.push(format!("kind bits {kb}: {e}")),
		}
	}
	let (kind_bits, nodes) = found.ok_or_else(|| errs.join("; "))?;
	drop(dg);
	let f = parse_footer(&qc)?;
	let mut marks: Vec<u64> = f.values.iter().chain(&f.side).map(|e| e.1).collect();
	marks.extend(&f.allocs);
	marks.extend(&f.hygiene);
	marks.sort_unstable();
	marks.dedup();
	let next = |p: u64| -> u64 {
		match marks.binary_search(&p) {
			Ok(i) if i + 1 < marks.len() => marks[i + 1],
			_ => f.footer_pos,
		}
	};
	let mut values = Vec::with_capacity(f.values.len());
	let mut without = 0;
	for &(index, pos) in &f.values {
		let mut c = Cur {
			b: &qc,
			p: pos as usize,
			end: qc.len(),
		};
		if c.uleb()? != index as u64 {
			return Err(format!(
				"value at {pos} does not start with its index {index}"
			));
		}
		let (kind, key) = nodes.get(index as usize).copied().unwrap_or((0, [0; 16]));
		if kind == 0 {
			without += 1;
			continue;
		}
		let len = (next(pos) - pos) as u32;
		values.push(Ent {
			index,
			kind,
			key,
			pos,
			len,
		});
	}
	Ok(Real {
		qc,
		qf,
		gf,
		qc_path,
		dg_path,
		values,
		side_effects: f.side.len(),
		allocs: f.allocs.len(),
		hygiene: f.hygiene.len(),
		values_without_node: without,
		kind_bits,
	})
}
