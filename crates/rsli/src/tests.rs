use super::*;
use crate::compress::lzh::{LZH_MAX_FREQ, LZH_N_CHAR, LZH_R, LZH_T};
use crate::compress::xor::xor_stream;
use flate2::write::DeflateEncoder;
use flate2::Compression;
use std::any::Any;
use std::fs;
use std::io::Write as _;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;

#[derive(Clone, Debug)]
struct SyntheticRsliEntry {
    name: String,
    method_raw: u16,
    plain: Vec<u8>,
    declared_packed_size: Option<u32>,
}

#[derive(Clone, Debug)]
struct RsliBuildOptions {
    seed: u32,
    presorted: bool,
    overlay: u32,
    add_ao_trailer: bool,
}

impl Default for RsliBuildOptions {
    fn default() -> Self {
        Self {
            seed: 0x1234_5678,
            presorted: true,
            overlay: 0,
            add_ao_trailer: false,
        }
    }
}

fn collect_files_recursive(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files_recursive(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

fn rsli_test_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata")
        .join("rsli");
    let mut files = Vec::new();
    collect_files_recursive(&root, &mut files);
    files.sort();
    files
        .into_iter()
        .filter(|path| {
            fs::read(path)
                .map(|data| data.get(0..4) == Some(b"NL\0\x01"))
                .unwrap_or(false)
        })
        .collect()
}

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    let any = payload.as_ref();
    if let Some(message) = any.downcast_ref::<String>() {
        return message.clone();
    }
    if let Some(message) = any.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    String::from("panic without message")
}

fn write_temp_file(prefix: &str, bytes: &[u8]) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "{}-{}-{}.bin",
        prefix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    fs::write(&path, bytes).expect("failed to write temp archive");
    path
}

fn deflate_raw(data: &[u8]) -> Vec<u8> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .expect("deflate encoder write failed");
    encoder.finish().expect("deflate encoder finish failed")
}

fn lzss_pack_literals(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for chunk in data.chunks(8) {
        let mask = if chunk.len() == 8 {
            0xFF
        } else {
            (1u16
                .checked_shl(u32::try_from(chunk.len()).expect("chunk len overflow"))
                .expect("shift overflow")
                - 1) as u8
        };
        out.push(mask);
        out.extend_from_slice(chunk);
    }
    out
}

struct BitWriter {
    bytes: Vec<u8>,
    current: u8,
    mask: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            current: 0,
            mask: 0x80,
        }
    }

    fn write_bit(&mut self, bit: u8) {
        if bit != 0 {
            self.current |= self.mask;
        }
        self.mask >>= 1;
        if self.mask == 0 {
            self.bytes.push(self.current);
            self.current = 0;
            self.mask = 0x80;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.mask != 0x80 {
            self.bytes.push(self.current);
        }
        self.bytes
    }
}

struct LzhLiteralModel {
    freq: [u16; LZH_T + 1],
    parent: [usize; LZH_T + LZH_N_CHAR],
    son: [usize; LZH_T + 1],
}

impl LzhLiteralModel {
    fn new() -> Self {
        let mut model = Self {
            freq: [0; LZH_T + 1],
            parent: [0; LZH_T + LZH_N_CHAR],
            son: [0; LZH_T + 1],
        };
        model.start_huff();
        model
    }

    fn encode_literal(&mut self, literal: u8, writer: &mut BitWriter) {
        let target = usize::from(literal) + LZH_T;
        let mut path = Vec::new();
        let mut visited = [false; LZH_T + 1];
        let found = self.find_path(self.son[LZH_R], target, &mut path, &mut visited);
        assert!(found, "failed to encode literal {literal}");
        for bit in path {
            writer.write_bit(bit);
        }

        self.update(usize::from(literal));
    }

    fn find_path(
        &self,
        node: usize,
        target: usize,
        path: &mut Vec<u8>,
        visited: &mut [bool; LZH_T + 1],
    ) -> bool {
        if node == target {
            return true;
        }
        if node >= LZH_T {
            return false;
        }
        if visited[node] {
            return false;
        }
        visited[node] = true;

        for bit in [0u8, 1u8] {
            let child = self.son[node + usize::from(bit)];
            path.push(bit);
            if self.find_path(child, target, path, visited) {
                visited[node] = false;
                return true;
            }
            path.pop();
        }

        visited[node] = false;
        false
    }

    fn start_huff(&mut self) {
        for i in 0..LZH_N_CHAR {
            self.freq[i] = 1;
            self.son[i] = i + LZH_T;
            self.parent[i + LZH_T] = i;
        }

        let mut i = 0usize;
        let mut j = LZH_N_CHAR;
        while j <= LZH_R {
            self.freq[j] = self.freq[i].saturating_add(self.freq[i + 1]);
            self.son[j] = i;
            self.parent[i] = j;
            self.parent[i + 1] = j;
            i += 2;
            j += 1;
        }

        self.freq[LZH_T] = u16::MAX;
        self.parent[LZH_R] = 0;
    }

    fn update(&mut self, c: usize) {
        if self.freq[LZH_R] == LZH_MAX_FREQ {
            self.reconstruct();
        }

        let mut current = self.parent[c + LZH_T];
        loop {
            self.freq[current] = self.freq[current].saturating_add(1);
            let freq = self.freq[current];

            if current + 1 < self.freq.len() && freq > self.freq[current + 1] {
                let mut swap_idx = current + 1;
                while swap_idx + 1 < self.freq.len() && freq > self.freq[swap_idx + 1] {
                    swap_idx += 1;
                }

                self.freq.swap(current, swap_idx);

                let left = self.son[current];
                let right = self.son[swap_idx];
                self.son[current] = right;
                self.son[swap_idx] = left;

                self.parent[left] = swap_idx;
                if left < LZH_T {
                    self.parent[left + 1] = swap_idx;
                }

                self.parent[right] = current;
                if right < LZH_T {
                    self.parent[right + 1] = current;
                }

                current = swap_idx;
            }

            current = self.parent[current];
            if current == 0 {
                break;
            }
        }
    }

    fn reconstruct(&mut self) {
        let mut j = 0usize;
        for i in 0..LZH_T {
            if self.son[i] >= LZH_T {
                self.freq[j] = self.freq[i].div_ceil(2);
                self.son[j] = self.son[i];
                j += 1;
            }
        }

        let mut i = 0usize;
        let mut current = LZH_N_CHAR;
        while current < LZH_T {
            let sum = self.freq[i].saturating_add(self.freq[i + 1]);
            self.freq[current] = sum;

            let mut insert_at = current;
            while insert_at > 0 && sum < self.freq[insert_at - 1] {
                insert_at -= 1;
            }

            for move_idx in (insert_at..current).rev() {
                self.freq[move_idx + 1] = self.freq[move_idx];
                self.son[move_idx + 1] = self.son[move_idx];
            }

            self.freq[insert_at] = sum;
            self.son[insert_at] = i;
            i += 2;
            current += 1;
        }

        for idx in 0..LZH_T {
            let node = self.son[idx];
            self.parent[node] = idx;
            if node < LZH_T {
                self.parent[node + 1] = idx;
            }
        }

        self.freq[LZH_T] = u16::MAX;
        self.parent[LZH_R] = 0;
    }
}

fn lzh_pack_literals(data: &[u8]) -> Vec<u8> {
    let mut writer = BitWriter::new();
    let mut model = LzhLiteralModel::new();
    for byte in data {
        model.encode_literal(*byte, &mut writer);
    }
    writer.finish()
}

fn packed_for_method(method_raw: u16, plain: &[u8], key16: u16) -> Vec<u8> {
    match (u32::from(method_raw)) & 0x1E0 {
        0x000 => plain.to_vec(),
        0x020 => xor_stream(plain, key16),
        0x040 => lzss_pack_literals(plain),
        0x060 => xor_stream(&lzss_pack_literals(plain), key16),
        0x080 => lzh_pack_literals(plain),
        0x0A0 => xor_stream(&lzh_pack_literals(plain), key16),
        0x100 => deflate_raw(plain),
        _ => plain.to_vec(),
    }
}

fn build_rsli_bytes(entries: &[SyntheticRsliEntry], opts: &RsliBuildOptions) -> Vec<u8> {
    let count = entries.len();
    let mut rows_plain = vec![0u8; count * 32];
    let table_end = 32 + rows_plain.len();

    let mut sort_lookup: Vec<usize> = (0..count).collect();
    sort_lookup.sort_by(|a, b| entries[*a].name.as_bytes().cmp(entries[*b].name.as_bytes()));

    let mut packed_blobs = Vec::with_capacity(count);
    for index in 0..count {
        let key16 = u16::try_from(sort_lookup[index]).expect("sort index overflow");
        let packed = packed_for_method(entries[index].method_raw, &entries[index].plain, key16);
        packed_blobs.push(packed);
    }

    let overlay = usize::try_from(opts.overlay).expect("overlay overflow");
    let mut cursor = table_end + overlay;
    let mut output = vec![0u8; cursor];

    let mut data_offsets = Vec::with_capacity(count);
    for (index, packed) in packed_blobs.iter().enumerate() {
        let raw_offset = cursor
            .checked_sub(overlay)
            .expect("overlay larger than cursor");
        data_offsets.push(raw_offset);

        let end = cursor.checked_add(packed.len()).expect("cursor overflow");
        if output.len() < end {
            output.resize(end, 0);
        }
        output[cursor..end].copy_from_slice(packed);
        cursor = end;

        let base = index * 32;
        let mut name_raw = [0u8; 12];
        let uppercase = entries[index].name.to_ascii_uppercase();
        let name_bytes = uppercase.as_bytes();
        assert!(name_bytes.len() <= 12, "name too long in synthetic fixture");
        name_raw[..name_bytes.len()].copy_from_slice(name_bytes);

        rows_plain[base..base + 12].copy_from_slice(&name_raw);

        let sort_field: i16 = if opts.presorted {
            i16::try_from(sort_lookup[index]).expect("sort field overflow")
        } else {
            0
        };

        let packed_size = entries[index]
            .declared_packed_size
            .unwrap_or_else(|| u32::try_from(packed.len()).expect("packed size overflow"));

        rows_plain[base + 16..base + 18].copy_from_slice(&entries[index].method_raw.to_le_bytes());
        rows_plain[base + 18..base + 20].copy_from_slice(&sort_field.to_le_bytes());
        rows_plain[base + 20..base + 24].copy_from_slice(
            &u32::try_from(entries[index].plain.len())
                .expect("unpacked size overflow")
                .to_le_bytes(),
        );
        rows_plain[base + 24..base + 28].copy_from_slice(
            &u32::try_from(data_offsets[index])
                .expect("data offset overflow")
                .to_le_bytes(),
        );
        rows_plain[base + 28..base + 32].copy_from_slice(&packed_size.to_le_bytes());
    }

    if output.len() < table_end {
        output.resize(table_end, 0);
    }

    output[0..2].copy_from_slice(b"NL");
    output[2] = 0;
    output[3] = 1;
    output[4..6].copy_from_slice(
        &i16::try_from(count)
            .expect("entry count overflow")
            .to_le_bytes(),
    );

    let presorted_flag = if opts.presorted { 0xABBA_u16 } else { 0_u16 };
    output[14..16].copy_from_slice(&presorted_flag.to_le_bytes());
    output[20..24].copy_from_slice(&opts.seed.to_le_bytes());

    let encrypted_table = xor_stream(&rows_plain, (opts.seed & 0xFFFF) as u16);
    output[32..table_end].copy_from_slice(&encrypted_table);

    if opts.add_ao_trailer {
        output.extend_from_slice(b"AO");
        output.extend_from_slice(&opts.overlay.to_le_bytes());
    }

    output
}

#[test]
fn rsli_read_unpack_and_repack_all_files() {
    let files = rsli_test_files();
    if files.is_empty() {
        eprintln!(
            "skipping rsli_read_unpack_and_repack_all_files: no RsLi archives in testdata/rsli"
        );
        return;
    }

    let checked = files.len();
    let mut success = 0usize;
    let mut failures = Vec::new();

    for path in files {
        let display_path = path.display().to_string();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let original = fs::read(&path).expect("failed to read archive");
            let library = Library::open_path(&path)
                .unwrap_or_else(|err| panic!("failed to open {}: {err}", path.display()));

            let count = library.entry_count();
            assert_eq!(
                count,
                library.entries().count(),
                "entry count mismatch: {}",
                path.display()
            );

            for idx in 0..count {
                let id = EntryId(idx as u32);
                let meta_ref = library
                    .get(id)
                    .unwrap_or_else(|| panic!("missing entry #{idx} in {}", path.display()));

                let loaded = library.load(id).unwrap_or_else(|err| {
                    panic!("load failed for {} entry #{idx}: {err}", path.display())
                });

                let packed = library.load_packed(id).unwrap_or_else(|err| {
                    panic!(
                        "load_packed failed for {} entry #{idx}: {err}",
                        path.display()
                    )
                });
                let unpacked = library.unpack(&packed).unwrap_or_else(|err| {
                    panic!("unpack failed for {} entry #{idx}: {err}", path.display())
                });
                assert_eq!(
                    loaded,
                    unpacked,
                    "load != unpack in {} entry #{idx}",
                    path.display()
                );

                let mut out = Vec::new();
                let written = library.load_into(id, &mut out).unwrap_or_else(|err| {
                    panic!(
                        "load_into failed for {} entry #{idx}: {err}",
                        path.display()
                    )
                });
                assert_eq!(
                    written,
                    loaded.len(),
                    "load_into size mismatch in {} entry #{idx}",
                    path.display()
                );
                assert_eq!(
                    out,
                    loaded,
                    "load_into payload mismatch in {} entry #{idx}",
                    path.display()
                );

                let fast = library.load_fast(id).unwrap_or_else(|err| {
                    panic!(
                        "load_fast failed for {} entry #{idx}: {err}",
                        path.display()
                    )
                });
                assert_eq!(
                    fast.as_slice(),
                    loaded.as_slice(),
                    "load_fast mismatch in {} entry #{idx}",
                    path.display()
                );

                let found = library.find(&meta_ref.meta.name).unwrap_or_else(|| {
                    panic!(
                        "find failed for '{}' in {}",
                        meta_ref.meta.name,
                        path.display()
                    )
                });
                let found_meta = library.get(found).expect("find returned invalid entry id");
                assert_eq!(
                    found_meta.meta.name,
                    meta_ref.meta.name,
                    "find returned a different entry in {}",
                    path.display()
                );
            }

            let rebuilt = library
                .rebuild_from_parsed_metadata()
                .unwrap_or_else(|err| panic!("rebuild failed for {}: {err}", path.display()));
            assert_eq!(
                rebuilt,
                original,
                "byte-to-byte roundtrip mismatch for {}",
                path.display()
            );
        }));

        match result {
            Ok(()) => success += 1,
            Err(payload) => failures.push(format!("{}: {}", display_path, panic_message(payload))),
        }
    }

    let failed = failures.len();
    eprintln!(
        "RsLi summary: checked={}, success={}, failed={}",
        checked, success, failed
    );
    if !failures.is_empty() {
        panic!(
            "RsLi validation failed.\nsummary: checked={}, success={}, failed={}\n{}",
            checked,
            success,
            failed,
            failures.join("\n")
        );
    }
}

#[test]
fn rsli_synthetic_all_methods_roundtrip() {
    let entries = vec![
        SyntheticRsliEntry {
            name: "M_NONE".to_string(),
            method_raw: 0x000,
            plain: b"plain-data".to_vec(),
            declared_packed_size: None,
        },
        SyntheticRsliEntry {
            name: "M_XOR".to_string(),
            method_raw: 0x020,
            plain: b"xor-only".to_vec(),
            declared_packed_size: None,
        },
        SyntheticRsliEntry {
            name: "M_LZSS".to_string(),
            method_raw: 0x040,
            plain: b"lzss literals payload".to_vec(),
            declared_packed_size: None,
        },
        SyntheticRsliEntry {
            name: "M_XLZS".to_string(),
            method_raw: 0x060,
            plain: b"xor lzss payload".to_vec(),
            declared_packed_size: None,
        },
        SyntheticRsliEntry {
            name: "M_LZHU".to_string(),
            method_raw: 0x080,
            plain: b"huffman literals payload".to_vec(),
            declared_packed_size: None,
        },
        SyntheticRsliEntry {
            name: "M_XLZH".to_string(),
            method_raw: 0x0A0,
            plain: b"xor huffman payload".to_vec(),
            declared_packed_size: None,
        },
        SyntheticRsliEntry {
            name: "M_DEFL".to_string(),
            method_raw: 0x100,
            plain: b"deflate payload with repetition repetition repetition".to_vec(),
            declared_packed_size: None,
        },
    ];

    let bytes = build_rsli_bytes(
        &entries,
        &RsliBuildOptions {
            seed: 0xA1B2_C3D4,
            presorted: false,
            overlay: 0,
            add_ao_trailer: false,
        },
    );
    let path = write_temp_file("rsli-all-methods", &bytes);

    let library = Library::open_path(&path).expect("open synthetic rsli failed");
    assert_eq!(library.entry_count(), entries.len());

    for entry in &entries {
        let id = library
            .find(&entry.name)
            .unwrap_or_else(|| panic!("find failed for {}", entry.name));
        let loaded = library
            .load(id)
            .unwrap_or_else(|err| panic!("load failed for {}: {err}", entry.name));
        assert_eq!(
            loaded, entry.plain,
            "decoded payload mismatch for {}",
            entry.name
        );

        let packed = library
            .load_packed(id)
            .unwrap_or_else(|err| panic!("load_packed failed for {}: {err}", entry.name));
        let unpacked = library
            .unpack(&packed)
            .unwrap_or_else(|err| panic!("unpack failed for {}: {err}", entry.name));
        assert_eq!(unpacked, entry.plain, "unpack mismatch for {}", entry.name);
    }

    let _ = fs::remove_file(&path);
}

#[test]
fn rsli_synthetic_overlay_and_ao_trailer() {
    let entries = vec![SyntheticRsliEntry {
        name: "OVERLAY".to_string(),
        method_raw: 0x040,
        plain: b"overlay-data".to_vec(),
        declared_packed_size: None,
    }];

    let bytes = build_rsli_bytes(
        &entries,
        &RsliBuildOptions {
            seed: 0x4433_2211,
            presorted: true,
            overlay: 128,
            add_ao_trailer: true,
        },
    );
    let path = write_temp_file("rsli-overlay", &bytes);

    let library = Library::open_path_with(
        &path,
        OpenOptions {
            allow_ao_trailer: true,
            allow_deflate_eof_plus_one: true,
        },
    )
    .expect("open with AO trailer enabled failed");

    let id = library.find("OVERLAY").expect("find overlay entry failed");
    let payload = library.load(id).expect("load overlay entry failed");
    assert_eq!(payload, b"overlay-data");

    let _ = fs::remove_file(&path);
}

#[test]
fn rsli_deflate_eof_plus_one_quirk() {
    let plain = b"quirk deflate payload".to_vec();
    let packed = deflate_raw(&plain);
    let declared = u32::try_from(packed.len() + 1).expect("declared size overflow");

    let entries = vec![SyntheticRsliEntry {
        name: "QUIRK".to_string(),
        method_raw: 0x100,
        plain,
        declared_packed_size: Some(declared),
    }];
    let bytes = build_rsli_bytes(&entries, &RsliBuildOptions::default());
    let path = write_temp_file("rsli-deflate-quirk", &bytes);

    let lib_ok = Library::open_path_with(
        &path,
        OpenOptions {
            allow_ao_trailer: true,
            allow_deflate_eof_plus_one: true,
        },
    )
    .expect("open with EOF+1 quirk enabled failed");
    let loaded = lib_ok
        .load(lib_ok.find("QUIRK").expect("find quirk entry failed"))
        .expect("load quirk entry failed");
    assert_eq!(loaded, b"quirk deflate payload");

    match Library::open_path_with(
        &path,
        OpenOptions {
            allow_ao_trailer: true,
            allow_deflate_eof_plus_one: false,
        },
    ) {
        Err(Error::DeflateEofPlusOneQuirkRejected { id }) => assert_eq!(id, 0),
        other => panic!("expected DeflateEofPlusOneQuirkRejected, got {other:?}"),
    }

    let _ = fs::remove_file(&path);
}

#[test]
fn rsli_validation_error_cases() {
    let valid = build_rsli_bytes(
        &[SyntheticRsliEntry {
            name: "BASE".to_string(),
            method_raw: 0x000,
            plain: b"abc".to_vec(),
            declared_packed_size: None,
        }],
        &RsliBuildOptions::default(),
    );

    let mut bad_magic = valid.clone();
    bad_magic[0..2].copy_from_slice(b"XX");
    let path = write_temp_file("rsli-bad-magic", &bad_magic);
    match Library::open_path(&path) {
        Err(Error::InvalidMagic { .. }) => {}
        other => panic!("expected InvalidMagic, got {other:?}"),
    }
    let _ = fs::remove_file(&path);

    let mut bad_version = valid.clone();
    bad_version[3] = 2;
    let path = write_temp_file("rsli-bad-version", &bad_version);
    match Library::open_path(&path) {
        Err(Error::UnsupportedVersion { got }) => assert_eq!(got, 2),
        other => panic!("expected UnsupportedVersion, got {other:?}"),
    }
    let _ = fs::remove_file(&path);

    let mut bad_count = valid.clone();
    bad_count[4..6].copy_from_slice(&(-1_i16).to_le_bytes());
    let path = write_temp_file("rsli-bad-count", &bad_count);
    match Library::open_path(&path) {
        Err(Error::InvalidEntryCount { got }) => assert_eq!(got, -1),
        other => panic!("expected InvalidEntryCount, got {other:?}"),
    }
    let _ = fs::remove_file(&path);

    let mut bad_table = valid.clone();
    bad_table[4..6].copy_from_slice(&100_i16.to_le_bytes());
    let path = write_temp_file("rsli-bad-table", &bad_table);
    match Library::open_path(&path) {
        Err(Error::EntryTableOutOfBounds { .. }) => {}
        other => panic!("expected EntryTableOutOfBounds, got {other:?}"),
    }
    let _ = fs::remove_file(&path);

    let mut unknown_method = build_rsli_bytes(
        &[SyntheticRsliEntry {
            name: "UNK".to_string(),
            method_raw: 0x120,
            plain: b"x".to_vec(),
            declared_packed_size: None,
        }],
        &RsliBuildOptions::default(),
    );
    // Force truly unknown method by writing 0x1C0 mask bits.
    let row = 32;
    unknown_method[row + 16..row + 18].copy_from_slice(&(0x1C0_u16).to_le_bytes());
    // Re-encrypt table with the same seed.
    let seed = u32::from_le_bytes([
        unknown_method[20],
        unknown_method[21],
        unknown_method[22],
        unknown_method[23],
    ]);
    let mut plain_row = vec![0u8; 32];
    plain_row.copy_from_slice(&unknown_method[32..64]);
    plain_row = xor_stream(&plain_row, (seed & 0xFFFF) as u16);
    plain_row[16..18].copy_from_slice(&(0x1C0_u16).to_le_bytes());
    let encrypted_row = xor_stream(&plain_row, (seed & 0xFFFF) as u16);
    unknown_method[32..64].copy_from_slice(&encrypted_row);

    let path = write_temp_file("rsli-unknown-method", &unknown_method);
    let lib = Library::open_path(&path).expect("open archive with unknown method failed");
    match lib.load(EntryId(0)) {
        Err(Error::UnsupportedMethod { raw }) => assert_eq!(raw, 0x1C0),
        other => panic!("expected UnsupportedMethod, got {other:?}"),
    }
    let _ = fs::remove_file(&path);

    let mut bad_packed = valid.clone();
    bad_packed[32 + 28..32 + 32].copy_from_slice(&0xFFFF_FFF0_u32.to_le_bytes());
    let path = write_temp_file("rsli-bad-packed", &bad_packed);
    match Library::open_path(&path) {
        Err(Error::PackedSizePastEof { .. }) => {}
        other => panic!("expected PackedSizePastEof, got {other:?}"),
    }
    let _ = fs::remove_file(&path);

    let mut with_bad_overlay = valid;
    with_bad_overlay.extend_from_slice(b"AO");
    with_bad_overlay.extend_from_slice(&0xFFFF_FFFF_u32.to_le_bytes());
    let path = write_temp_file("rsli-bad-overlay", &with_bad_overlay);
    match Library::open_path_with(
        &path,
        OpenOptions {
            allow_ao_trailer: true,
            allow_deflate_eof_plus_one: true,
        },
    ) {
        Err(Error::MediaOverlayOutOfBounds { .. }) => {}
        other => panic!("expected MediaOverlayOutOfBounds, got {other:?}"),
    }
    let _ = fs::remove_file(&path);
}
