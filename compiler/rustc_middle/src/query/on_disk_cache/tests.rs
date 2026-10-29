// Node indices rise inside a query and fall back at the start of the next one.

use super::*;

fn entry(index: u32, pos: u64, len: u32) -> IndexEntry {
    IndexEntry { index: SerializedDepNodeIndex::from_u32(index), pos: AbsoluteBytePos(pos), len }
}

fn round_trip_index(entries: Vec<IndexEntry>) {
    let mut e = MemEncoder::new();
    ValuesIndex(entries.clone()).encode(&mut e);
    let bytes = e.finish();
    let mut d = MemDecoder::new(&bytes, 0).unwrap();
    let decoded = ValuesIndex::decode(&mut d).0;
    assert_eq!(decoded.len(), entries.len());
    for (a, b) in decoded.iter().zip(&entries) {
        assert_eq!(a.span(), b.span());
    }
}

#[test]
fn values_index_round_trips() {
    round_trip_index(Vec::new());
    round_trip_index(vec![entry(7, 100, 0)]);
    // Node indices rise inside a query and fall back at the start of the next one.
    round_trip_index(vec![
        entry(5, 40, 3),
        entry(6, 43, 200),
        entry(4_000_000, 243, 1),
        entry(2, 244, 70_000),
        entry(u32::MAX, 70_244, 9),
        entry(0, 70_253, 1),
    ]);
}

#[test]
#[should_panic(expected = "not contiguous")]
fn values_index_refuses_a_gap() {
    let mut e = MemEncoder::new();
    ValuesIndex(vec![entry(1, 10, 5), entry(2, 16, 5)]).encode(&mut e);
}

#[test]
fn def_path_hashes_round_trip() {
    let hashes: Vec<DefPathHash> = [(1u64, 2u64), (1, 3), (u64::MAX, 0), (1, u64::MAX), (9, 9)]
        .into_iter()
        .map(|(krate, local)| DefPathHash(Fingerprint::new(krate, local)))
        .collect();
    let mut e = MemEncoder::new();
    DefPathHashes(hashes.clone()).encode(&mut e);
    let bytes = e.finish();
    let mut d = MemDecoder::new(&bytes, 0).unwrap();
    assert_eq!(DefPathHashes::decode(&mut d).0, hashes);
}
