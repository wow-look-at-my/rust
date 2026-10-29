// Encodes `(index, node, value, edges)` in order, with a flush wherever `flush_before` names the index, then decodes the stream and checks that every node comes back unchanged.

use super::*;

fn fp(n: u64) -> Fingerprint {
    Fingerprint::new(n.wrapping_mul(0x9E37_79B9_7F4A_7C15), n ^ 0xA5A5)
}

/// A result fingerprint as the graph records it.
fn val(n: u64) -> Fingerprint {
    short_value_fingerprint(fp(n))
}

fn node(kind: u16, key: u64) -> DepNode {
    DepNode { kind: DepKind::from_u16(kind), key_fingerprint: fp(key).into() }
}

/// Encodes `(index, node, value, edges)` in order, with a flush wherever `flush_before` names
/// the index, then decodes the stream and checks that every node comes back unchanged.
fn round_trip(entries: &[(u32, DepNode, Fingerprint, Vec<u32>)], flush_before: &[u32]) -> usize {
    let mut e = MemEncoder::new();
    let mut refs = EncoderRefs::new();
    for (index, node, value, edges) in entries {
        if flush_before.contains(index) {
            refs.prev_index = None;
        }
        let edges: Vec<DepNodeIndex> = edges.iter().map(|&i| DepNodeIndex::from_u32(i)).collect();
        let info = NodeInfo { node: *node, value_fingerprint: *value, edges: &edges };
        info.encode(&mut e, DepNodeIndex::from_u32(*index), &mut refs);
    }
    let len = e.data.len();
    let mut data = e.finish();
    data.extend_from_slice(rustc_serialize::opaque::MAGIC_END_BYTES);

    let max = entries.iter().map(|(i, ..)| *i).max().unwrap() as usize + 1;
    let null = DepNode { kind: DepKind::Null, key_fingerprint: Fingerprint::ZERO.into() };
    let mut nodes = IndexVec::<SerializedDepNodeIndex, _>::from_elem_n(null, max);
    let mut values = IndexVec::<SerializedDepNodeIndex, _>::from_elem_n(Fingerprint::ZERO, max);
    let mut d = MemDecoder::new(&data, 0).unwrap();
    let mut prev_index = None;
    let mut history = Vec::new();
    for (index, node, value, edges) in entries {
        let record = NodeRecord::decode(&mut d, prev_index, &nodes, &values);
        assert_eq!(record.index.as_u32(), *index);
        if node.kind.is_anon() {
            assert_eq!(record.node.kind, node.kind);
            assert_eq!(record.node.key_fingerprint, Fingerprint::ZERO.into());
        } else {
            assert_eq!(record.node, *node);
        }
        assert_eq!(record.value_fingerprint, *value);
        assert_eq!(record.num_edges as usize, edges.len());
        let width = record.head.bytes_per_index();
        let mut raw = Vec::new();
        record.decode_edges(&mut d, &mut raw, &mut history);
        assert_eq!(raw.len(), width * edges.len());
        for (chunk, &edge) in raw.chunks(width).zip(edges) {
            let mut bytes = [0u8; 4];
            bytes[..width].copy_from_slice(chunk);
            assert_eq!(u32::from_le_bytes(bytes), edge);
        }
        nodes[record.index] = record.node;
        values[record.index] = record.value_fingerprint;
        prev_index = Some(record.index);
    }
    assert_eq!(d.position(), len, "the decoder must consume the whole stream");
    len
}

#[test]
fn round_trip_refs_gaps_and_flushes() {
    let shared_value = val(1000);
    let entries = vec![
        (0, node(1, 1), val(10), vec![]),
        // Same key under another kind, shared value, index is next.
        (1, node(2, 1), shared_value, vec![0]),
        (2, node(3, 1), shared_value, vec![0, 1]),
        // Zero value, new key.
        (3, node(1, 2), Fingerprint::ZERO, vec![2, 1, 0]),
        // A gap in the indices, as when a thread takes a new batch.
        (300, node(2, 2), val(11), vec![3, 0x1_0000]),
        // A flush resets the implied index but references to earlier nodes stay valid.
        (301, node(3, 2), shared_value, vec![300]),
        (302, node(4, 1), val(10), vec![0x0100_0000]),
    ];
    let len = round_trip(&entries, &[301]);
    // The fixed header alone was many bytes per node.
    assert!(len < entries.len() * 38, "encoded {len} bytes");
}

#[test]
fn edge_back_references_restart_at_a_flush() {
    let far = 0x0100_0000;
    let entries = vec![
        (0, node(1, 1), val(1), vec![]),
        (1, node(1, 2), val(2), vec![far, 0, far + 7]),
        // Each far target repeats, so its edge is a short back-reference.
        (2, node(1, 3), val(3), vec![far + 7, far, 0, far]),
        // After the flush a back-reference to the block before is not valid.
        (3, node(1, 4), val(4), vec![far, far + 7]),
        (4, node(1, 5), val(5), vec![far + 7, far]),
    ];
    round_trip(&entries, &[3]);
    let without_repeats = vec![
        (0, node(1, 1), val(1), vec![]),
        (1, node(1, 2), val(2), vec![far, 0, far + 7]),
        (2, node(1, 3), val(3), vec![far + 20, far + 30, 0, far + 40]),
    ];
    let repeats = round_trip(&entries[..3], &[]);
    assert!(repeats < round_trip(&without_repeats, &[]), "back-references saved nothing");
}

#[test]
fn round_trip_long_edge_list() {
    let edges: Vec<u32> = (0..(NodeHead::MAX_INLINE_LEN as u32 + 10)).collect();
    let entries = vec![(0, node(1, 1), val(1), vec![]), (1, node(2, 2), val(2), edges)];
    round_trip(&entries, &[]);
}

#[test]
fn anon_key_is_not_stored() {
    let trait_select = DepKind::TraitSelect.as_u16();
    let red = DepKind::Red.as_u16();
    let anon = round_trip(&[(0, node(trait_select, 7), Fingerprint::ZERO, vec![])], &[]);
    let keyed = round_trip(&[(0, node(red, 7), Fingerprint::ZERO, vec![])], &[]);
    assert_eq!(keyed - anon, size_of::<Fingerprint>(), "{anon} vs {keyed} bytes");
}

#[test]
fn key_that_shares_its_crate_half_is_stored_short() {
    let red = DepKind::Red.as_u16();
    let crate_key = |local: u64| DepNode {
        kind: DepKind::from_u16(red),
        key_fingerprint: Fingerprint::new(0x1234_5678_9ABC_DEF0u64, local).into(),
    };
    let shared = vec![
        (0, crate_key(1), Fingerprint::ZERO, vec![]),
        (1, crate_key(2), Fingerprint::ZERO, vec![]),
        // An unrelated key in between does not break the reference.
        (2, node(red, 9), Fingerprint::ZERO, vec![]),
        (3, crate_key(3), Fingerprint::ZERO, vec![]),
    ];
    let unshared = vec![
        (0, node(red, 1), Fingerprint::ZERO, vec![]),
        (1, node(red, 2), Fingerprint::ZERO, vec![]),
        (2, node(red, 9), Fingerprint::ZERO, vec![]),
        (3, node(red, 3), Fingerprint::ZERO, vec![]),
    ];
    let shared_len = round_trip(&shared, &[]);
    let unshared_len = round_trip(&unshared, &[]);
    // Each of those later crate keys keeps several bytes and writes a one-byte distance.
    assert_eq!(unshared_len - shared_len, 2 * 7, "{shared_len} vs {unshared_len} bytes");
}

#[test]
fn value_is_stored_in_eight_bytes() {
    let red = DepKind::Red.as_u16();
    let zero = round_trip(&[(0, node(red, 7), Fingerprint::ZERO, vec![])], &[]);
    let stored = round_trip(&[(0, node(red, 7), val(9), vec![])], &[]);
    assert_eq!(stored - zero, 8, "{zero} vs {stored} bytes");
}

#[test]
fn short_value_fingerprint_keeps_the_low_half() {
    let (low, high) = fp(3).split();
    assert_ne!(high.as_u64(), 0);
    assert_eq!(val(3), Fingerprint::new(low, 0u64));
    assert_eq!(short_value_fingerprint(Fingerprint::ZERO), Fingerprint::ZERO);
}

#[test]
fn fingerprint_refs_only_point_back() {
    let mut refs = FingerprintRefs::new();
    assert_eq!(refs.replace(fp(1), 5), None);
    assert_eq!(refs.replace(fp(1), 7), Some(5));
    assert_eq!(refs.replace(fp(1), 9), Some(7));
    // An index that is not after the recorded one is never a reference.
    assert_eq!(refs.replace(fp(1), 3), None);
}
