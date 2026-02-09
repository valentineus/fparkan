use super::*;
use std::any::Any;
use std::fs;
use std::panic::{catch_unwind, AssertUnwindSafe};

#[derive(Clone)]
struct SyntheticEntry<'a> {
    kind: u32,
    attr1: u32,
    attr2: u32,
    attr3: u32,
    name: &'a str,
    data: &'a [u8],
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

fn nres_test_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("testdata")
        .join("nres");
    let mut files = Vec::new();
    collect_files_recursive(&root, &mut files);
    files.sort();
    files
        .into_iter()
        .filter(|path| {
            fs::read(path)
                .map(|data| data.get(0..4) == Some(b"NRes"))
                .unwrap_or(false)
        })
        .collect()
}

fn make_temp_copy(original: &Path, bytes: &[u8]) -> PathBuf {
    let mut path = std::env::temp_dir();
    let file_name = original
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or("archive");
    path.push(format!(
        "nres-test-{}-{}-{}",
        std::process::id(),
        unix_time_nanos(),
        file_name
    ));
    fs::write(&path, bytes).expect("failed to create temp file");
    path
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

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    let slice = bytes
        .get(offset..offset + 4)
        .expect("u32 read out of bounds in test");
    let arr: [u8; 4] = slice.try_into().expect("u32 conversion failed in test");
    u32::from_le_bytes(arr)
}

fn build_nres_bytes(entries: &[SyntheticEntry<'_>]) -> Vec<u8> {
    let mut out = vec![0u8; 16];
    let mut offsets = Vec::with_capacity(entries.len());

    for entry in entries {
        offsets.push(u32::try_from(out.len()).expect("offset overflow"));
        out.extend_from_slice(entry.data);
        let padding = (8 - (out.len() % 8)) % 8;
        if padding > 0 {
            out.resize(out.len() + padding, 0);
        }
    }

    let mut sort_order: Vec<usize> = (0..entries.len()).collect();
    sort_order.sort_by(|a, b| {
        cmp_name_case_insensitive(entries[*a].name.as_bytes(), entries[*b].name.as_bytes())
    });

    for (index, entry) in entries.iter().enumerate() {
        let mut name_raw = [0u8; 36];
        let name_bytes = entry.name.as_bytes();
        assert!(name_bytes.len() <= 35, "name too long in fixture");
        name_raw[..name_bytes.len()].copy_from_slice(name_bytes);

        push_u32(&mut out, entry.kind);
        push_u32(&mut out, entry.attr1);
        push_u32(&mut out, entry.attr2);
        push_u32(
            &mut out,
            u32::try_from(entry.data.len()).expect("data size overflow"),
        );
        push_u32(&mut out, entry.attr3);
        out.extend_from_slice(&name_raw);
        push_u32(&mut out, offsets[index]);
        push_u32(
            &mut out,
            u32::try_from(sort_order[index]).expect("sort index overflow"),
        );
    }

    out[0..4].copy_from_slice(b"NRes");
    out[4..8].copy_from_slice(&0x100_u32.to_le_bytes());
    out[8..12].copy_from_slice(
        &u32::try_from(entries.len())
            .expect("count overflow")
            .to_le_bytes(),
    );
    let total_size = u32::try_from(out.len()).expect("size overflow");
    out[12..16].copy_from_slice(&total_size.to_le_bytes());
    out
}

#[test]
fn nres_read_and_roundtrip_all_files() {
    let files = nres_test_files();
    if files.is_empty() {
        eprintln!("skipping nres_read_and_roundtrip_all_files: no NRes archives in testdata/nres");
        return;
    }

    let checked = files.len();
    let mut success = 0usize;
    let mut failures = Vec::new();

    for path in files {
        let display_path = path.display().to_string();
        let result = catch_unwind(AssertUnwindSafe(|| {
            let original = fs::read(&path).expect("failed to read archive");
            let archive = Archive::open_path(&path)
                .unwrap_or_else(|err| panic!("failed to open {}: {err}", path.display()));

            let count = archive.entry_count();
            assert_eq!(
                count,
                archive.entries().count(),
                "entry count mismatch: {}",
                path.display()
            );

            for idx in 0..count {
                let id = EntryId(idx as u32);
                let entry = archive
                    .get(id)
                    .unwrap_or_else(|| panic!("missing entry #{idx} in {}", path.display()));

                let payload = archive.read(id).unwrap_or_else(|err| {
                    panic!("read failed for {} entry #{idx}: {err}", path.display())
                });

                let mut out = Vec::new();
                let written = archive.read_into(id, &mut out).unwrap_or_else(|err| {
                    panic!(
                        "read_into failed for {} entry #{idx}: {err}",
                        path.display()
                    )
                });
                assert_eq!(
                    written,
                    payload.as_slice().len(),
                    "size mismatch in {} entry #{idx}",
                    path.display()
                );
                assert_eq!(
                    out.as_slice(),
                    payload.as_slice(),
                    "payload mismatch in {} entry #{idx}",
                    path.display()
                );

                let raw = archive
                    .raw_slice(id)
                    .unwrap_or_else(|err| {
                        panic!(
                            "raw_slice failed for {} entry #{idx}: {err}",
                            path.display()
                        )
                    })
                    .expect("raw_slice must return Some for file-backed archive");
                assert_eq!(
                    raw,
                    payload.as_slice(),
                    "raw slice mismatch in {} entry #{idx}",
                    path.display()
                );

                let found = archive.find(&entry.meta.name).unwrap_or_else(|| {
                    panic!(
                        "find failed for name '{}' in {}",
                        entry.meta.name,
                        path.display()
                    )
                });
                let found_meta = archive.get(found).expect("find returned invalid id");
                assert!(
                    found_meta.meta.name.eq_ignore_ascii_case(&entry.meta.name),
                    "find returned unrelated entry in {}",
                    path.display()
                );
            }

            let temp_copy = make_temp_copy(&path, &original);
            let mut editor = Archive::edit_path(&temp_copy)
                .unwrap_or_else(|err| panic!("edit_path failed for {}: {err}", path.display()));

            for idx in 0..count {
                let data = archive
                    .read(EntryId(idx as u32))
                    .unwrap_or_else(|err| {
                        panic!(
                            "read before replace failed for {} entry #{idx}: {err}",
                            path.display()
                        )
                    })
                    .into_owned();
                editor
                    .replace_data(EntryId(idx as u32), &data)
                    .unwrap_or_else(|err| {
                        panic!(
                            "replace_data failed for {} entry #{idx}: {err}",
                            path.display()
                        )
                    });
            }

            editor
                .commit()
                .unwrap_or_else(|err| panic!("commit failed for {}: {err}", path.display()));
            let rebuilt = fs::read(&temp_copy).expect("failed to read rebuilt archive");
            let _ = fs::remove_file(&temp_copy);

            assert_eq!(
                original,
                rebuilt,
                "byte-to-byte roundtrip mismatch for {}",
                path.display()
            );
        }));

        match result {
            Ok(()) => success += 1,
            Err(payload) => {
                failures.push(format!("{}: {}", display_path, panic_message(payload)));
            }
        }
    }

    let failed = failures.len();
    eprintln!(
        "NRes summary: checked={}, success={}, failed={}",
        checked, success, failed
    );
    if !failures.is_empty() {
        panic!(
            "NRes validation failed.\nsummary: checked={}, success={}, failed={}\n{}",
            checked,
            success,
            failed,
            failures.join("\n")
        );
    }
}

#[test]
fn nres_raw_mode_exposes_whole_file() {
    let files = nres_test_files();
    let Some(first) = files.first() else {
        eprintln!("skipping nres_raw_mode_exposes_whole_file: no NRes archives in testdata/nres");
        return;
    };
    let original = fs::read(first).expect("failed to read archive");
    let arc: Arc<[u8]> = Arc::from(original.clone().into_boxed_slice());

    let archive = Archive::open_bytes(
        arc,
        OpenOptions {
            raw_mode: true,
            sequential_hint: false,
            prefetch_pages: false,
        },
    )
    .expect("raw mode open failed");

    assert_eq!(archive.entry_count(), 1);
    let data = archive.read(EntryId(0)).expect("raw read failed");
    assert_eq!(data.as_slice(), original.as_slice());
}

#[test]
fn nres_raw_mode_accepts_non_nres_bytes() {
    let payload = b"not-an-nres-archive".to_vec();
    let bytes: Arc<[u8]> = Arc::from(payload.clone().into_boxed_slice());

    match Archive::open_bytes(bytes.clone(), OpenOptions::default()) {
        Err(Error::InvalidMagic { .. }) => {}
        other => panic!("expected InvalidMagic without raw_mode, got {other:?}"),
    }

    let archive = Archive::open_bytes(
        bytes,
        OpenOptions {
            raw_mode: true,
            sequential_hint: false,
            prefetch_pages: false,
        },
    )
    .expect("raw_mode should accept any bytes");

    assert_eq!(archive.entry_count(), 1);
    assert_eq!(archive.find("raw"), Some(EntryId(0)));
    assert_eq!(
        archive
            .read(EntryId(0))
            .expect("raw read failed")
            .as_slice(),
        payload.as_slice()
    );
}

#[test]
fn nres_open_options_hints_do_not_change_payload() {
    let payload: Vec<u8> = (0..70_000u32).map(|v| (v % 251) as u8).collect();
    let src = build_nres_bytes(&[SyntheticEntry {
        kind: 7,
        attr1: 70,
        attr2: 700,
        attr3: 7000,
        name: "big.bin",
        data: &payload,
    }]);
    let arc: Arc<[u8]> = Arc::from(src.into_boxed_slice());

    let baseline = Archive::open_bytes(arc.clone(), OpenOptions::default())
        .expect("baseline open should succeed");
    let hinted = Archive::open_bytes(
        arc,
        OpenOptions {
            raw_mode: false,
            sequential_hint: true,
            prefetch_pages: true,
        },
    )
    .expect("open with hints should succeed");

    assert_eq!(baseline.entry_count(), 1);
    assert_eq!(hinted.entry_count(), 1);
    assert_eq!(baseline.find("BIG.BIN"), Some(EntryId(0)));
    assert_eq!(hinted.find("big.bin"), Some(EntryId(0)));
    assert_eq!(
        baseline
            .read(EntryId(0))
            .expect("baseline read failed")
            .as_slice(),
        hinted
            .read(EntryId(0))
            .expect("hinted read failed")
            .as_slice()
    );
}

#[test]
fn nres_commit_empty_archive_has_minimal_layout() {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "nres-empty-commit-{}-{}.lib",
        std::process::id(),
        unix_time_nanos()
    ));
    fs::write(&path, build_nres_bytes(&[])).expect("write empty archive failed");

    Archive::edit_path(&path)
        .expect("edit_path failed for empty archive")
        .commit()
        .expect("commit failed for empty archive");

    let bytes = fs::read(&path).expect("failed to read committed archive");
    assert_eq!(bytes.len(), 16, "empty archive must contain only header");
    assert_eq!(&bytes[0..4], b"NRes");
    assert_eq!(read_u32_le(&bytes, 4), 0x100);
    assert_eq!(read_u32_le(&bytes, 8), 0);
    assert_eq!(read_u32_le(&bytes, 12), 16);

    let _ = fs::remove_file(&path);
}

#[test]
fn nres_commit_recomputes_header_directory_and_sort_table() {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "nres-commit-layout-{}-{}.lib",
        std::process::id(),
        unix_time_nanos()
    ));
    fs::write(&path, build_nres_bytes(&[])).expect("write empty archive failed");

    let mut editor = Archive::edit_path(&path).expect("edit_path failed");
    editor
        .add(NewEntry {
            kind: 10,
            attr1: 1,
            attr2: 2,
            attr3: 3,
            name: "Zulu",
            data: b"aaaaa",
        })
        .expect("add #0 failed");
    editor
        .add(NewEntry {
            kind: 11,
            attr1: 4,
            attr2: 5,
            attr3: 6,
            name: "alpha",
            data: b"bbbbbbbb",
        })
        .expect("add #1 failed");
    editor
        .add(NewEntry {
            kind: 12,
            attr1: 7,
            attr2: 8,
            attr3: 9,
            name: "Beta",
            data: b"cccc",
        })
        .expect("add #2 failed");
    editor.commit().expect("commit failed");

    let bytes = fs::read(&path).expect("failed to read committed archive");
    assert_eq!(&bytes[0..4], b"NRes");
    assert_eq!(read_u32_le(&bytes, 4), 0x100);

    let entry_count = usize::try_from(read_u32_le(&bytes, 8)).expect("entry_count overflow");
    let total_size = usize::try_from(read_u32_le(&bytes, 12)).expect("total_size overflow");
    assert_eq!(entry_count, 3);
    assert_eq!(total_size, bytes.len());

    let directory_offset = total_size
        .checked_sub(entry_count * 64)
        .expect("invalid directory offset");
    assert!(directory_offset >= 16);

    let mut sort_indices = Vec::new();
    let mut prev_data_end = 16usize;
    for idx in 0..entry_count {
        let base = directory_offset + idx * 64;
        let data_size = usize::try_from(read_u32_le(&bytes, base + 12)).expect("size overflow");
        let data_offset = usize::try_from(read_u32_le(&bytes, base + 56)).expect("offset overflow");
        let sort_index =
            usize::try_from(read_u32_le(&bytes, base + 60)).expect("sort index overflow");

        assert_eq!(
            data_offset % 8,
            0,
            "entry #{idx} data offset must be 8-byte aligned"
        );
        assert!(
            data_offset >= prev_data_end,
            "entry #{idx} offset regressed"
        );
        assert!(
            data_offset + data_size <= directory_offset,
            "entry #{idx} overlaps directory"
        );
        prev_data_end = data_offset + data_size;
        sort_indices.push(sort_index);
    }

    let names = ["Zulu", "alpha", "Beta"];
    let mut expected_sort: Vec<usize> = (0..names.len()).collect();
    expected_sort
        .sort_by(|a, b| cmp_name_case_insensitive(names[*a].as_bytes(), names[*b].as_bytes()));
    assert_eq!(
        sort_indices, expected_sort,
        "sort table must contain original indexes in case-insensitive alphabetical order"
    );

    let archive = Archive::open_path(&path).expect("re-open failed");
    assert_eq!(archive.find("zulu"), Some(EntryId(0)));
    assert_eq!(archive.find("ALPHA"), Some(EntryId(1)));
    assert_eq!(archive.find("beta"), Some(EntryId(2)));

    let _ = fs::remove_file(&path);
}

#[test]
fn nres_synthetic_read_find_and_edit() {
    let payload_a = b"alpha";
    let payload_b = b"B";
    let payload_c = b"";
    let src = build_nres_bytes(&[
        SyntheticEntry {
            kind: 1,
            attr1: 10,
            attr2: 20,
            attr3: 30,
            name: "Alpha.TXT",
            data: payload_a,
        },
        SyntheticEntry {
            kind: 2,
            attr1: 11,
            attr2: 21,
            attr3: 31,
            name: "beta.bin",
            data: payload_b,
        },
        SyntheticEntry {
            kind: 3,
            attr1: 12,
            attr2: 22,
            attr3: 32,
            name: "Gamma",
            data: payload_c,
        },
    ]);

    let archive = Archive::open_bytes(
        Arc::from(src.clone().into_boxed_slice()),
        OpenOptions::default(),
    )
    .expect("open synthetic nres failed");

    assert_eq!(archive.entry_count(), 3);
    assert_eq!(archive.find("alpha.txt"), Some(EntryId(0)));
    assert_eq!(archive.find("BETA.BIN"), Some(EntryId(1)));
    assert_eq!(archive.find("gAmMa"), Some(EntryId(2)));
    assert_eq!(archive.find("missing"), None);

    assert_eq!(
        archive.read(EntryId(0)).expect("read #0 failed").as_slice(),
        payload_a
    );
    assert_eq!(
        archive.read(EntryId(1)).expect("read #1 failed").as_slice(),
        payload_b
    );
    assert_eq!(
        archive.read(EntryId(2)).expect("read #2 failed").as_slice(),
        payload_c
    );

    let mut path = std::env::temp_dir();
    path.push(format!(
        "nres-synth-edit-{}-{}.lib",
        std::process::id(),
        unix_time_nanos()
    ));
    fs::write(&path, &src).expect("write temp synthetic archive failed");

    let mut editor = Archive::edit_path(&path).expect("edit_path on synthetic archive failed");
    editor
        .replace_data(EntryId(1), b"replaced")
        .expect("replace_data failed");
    let added = editor
        .add(NewEntry {
            kind: 4,
            attr1: 13,
            attr2: 23,
            attr3: 33,
            name: "delta",
            data: b"new payload",
        })
        .expect("add failed");
    assert_eq!(added, EntryId(3));
    editor.remove(EntryId(2)).expect("remove failed");
    editor.commit().expect("commit failed");

    let edited = Archive::open_path(&path).expect("re-open edited archive failed");
    assert_eq!(edited.entry_count(), 3);
    assert_eq!(
        edited
            .read(edited.find("beta.bin").expect("find beta.bin failed"))
            .expect("read beta.bin failed")
            .as_slice(),
        b"replaced"
    );
    assert_eq!(
        edited
            .read(edited.find("delta").expect("find delta failed"))
            .expect("read delta failed")
            .as_slice(),
        b"new payload"
    );
    assert_eq!(edited.find("gamma"), None);

    let _ = fs::remove_file(&path);
}

#[test]
fn nres_validation_error_cases() {
    let valid = build_nres_bytes(&[SyntheticEntry {
        kind: 1,
        attr1: 2,
        attr2: 3,
        attr3: 4,
        name: "ok",
        data: b"1234",
    }]);

    let mut invalid_magic = valid.clone();
    invalid_magic[0..4].copy_from_slice(b"FAIL");
    match Archive::open_bytes(
        Arc::from(invalid_magic.into_boxed_slice()),
        OpenOptions::default(),
    ) {
        Err(Error::InvalidMagic { .. }) => {}
        other => panic!("expected InvalidMagic, got {other:?}"),
    }

    let mut invalid_version = valid.clone();
    invalid_version[4..8].copy_from_slice(&0x200_u32.to_le_bytes());
    match Archive::open_bytes(
        Arc::from(invalid_version.into_boxed_slice()),
        OpenOptions::default(),
    ) {
        Err(Error::UnsupportedVersion { got }) => assert_eq!(got, 0x200),
        other => panic!("expected UnsupportedVersion, got {other:?}"),
    }

    let mut bad_total = valid.clone();
    bad_total[12..16].copy_from_slice(&0_u32.to_le_bytes());
    match Archive::open_bytes(
        Arc::from(bad_total.into_boxed_slice()),
        OpenOptions::default(),
    ) {
        Err(Error::TotalSizeMismatch { .. }) => {}
        other => panic!("expected TotalSizeMismatch, got {other:?}"),
    }

    let mut bad_count = valid.clone();
    bad_count[8..12].copy_from_slice(&(-1_i32).to_le_bytes());
    match Archive::open_bytes(
        Arc::from(bad_count.into_boxed_slice()),
        OpenOptions::default(),
    ) {
        Err(Error::InvalidEntryCount { got }) => assert_eq!(got, -1),
        other => panic!("expected InvalidEntryCount, got {other:?}"),
    }

    let mut bad_dir = valid.clone();
    bad_dir[8..12].copy_from_slice(&1000_u32.to_le_bytes());
    match Archive::open_bytes(
        Arc::from(bad_dir.into_boxed_slice()),
        OpenOptions::default(),
    ) {
        Err(Error::DirectoryOutOfBounds { .. }) => {}
        other => panic!("expected DirectoryOutOfBounds, got {other:?}"),
    }

    let mut long_name = valid.clone();
    let entry_base = long_name.len() - 64;
    for b in &mut long_name[entry_base + 20..entry_base + 56] {
        *b = b'X';
    }
    match Archive::open_bytes(
        Arc::from(long_name.into_boxed_slice()),
        OpenOptions::default(),
    ) {
        Err(Error::NameTooLong { .. }) => {}
        other => panic!("expected NameTooLong, got {other:?}"),
    }

    let mut bad_data = valid.clone();
    bad_data[entry_base + 56..entry_base + 60].copy_from_slice(&12_u32.to_le_bytes());
    bad_data[entry_base + 12..entry_base + 16].copy_from_slice(&32_u32.to_le_bytes());
    match Archive::open_bytes(
        Arc::from(bad_data.into_boxed_slice()),
        OpenOptions::default(),
    ) {
        Err(Error::EntryDataOutOfBounds { .. }) => {}
        other => panic!("expected EntryDataOutOfBounds, got {other:?}"),
    }

    let archive = Archive::open_bytes(Arc::from(valid.into_boxed_slice()), OpenOptions::default())
        .expect("open valid archive failed");
    match archive.read(EntryId(99)) {
        Err(Error::EntryIdOutOfRange { .. }) => {}
        other => panic!("expected EntryIdOutOfRange, got {other:?}"),
    }
}

#[test]
fn nres_editor_validation_error_cases() {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "nres-editor-errors-{}-{}.lib",
        std::process::id(),
        unix_time_nanos()
    ));
    let src = build_nres_bytes(&[]);
    fs::write(&path, src).expect("write empty archive failed");

    let mut editor = Archive::edit_path(&path).expect("edit_path failed");

    let long_name = "X".repeat(36);
    match editor.add(NewEntry {
        kind: 0,
        attr1: 0,
        attr2: 0,
        attr3: 0,
        name: &long_name,
        data: b"",
    }) {
        Err(Error::NameTooLong { .. }) => {}
        other => panic!("expected NameTooLong, got {other:?}"),
    }

    match editor.add(NewEntry {
        kind: 0,
        attr1: 0,
        attr2: 0,
        attr3: 0,
        name: "bad\0name",
        data: b"",
    }) {
        Err(Error::NameContainsNul) => {}
        other => panic!("expected NameContainsNul, got {other:?}"),
    }

    match editor.replace_data(EntryId(0), b"x") {
        Err(Error::EntryIdOutOfRange { .. }) => {}
        other => panic!("expected EntryIdOutOfRange, got {other:?}"),
    }

    match editor.remove(EntryId(0)) {
        Err(Error::EntryIdOutOfRange { .. }) => {}
        other => panic!("expected EntryIdOutOfRange, got {other:?}"),
    }

    let _ = fs::remove_file(&path);
}
