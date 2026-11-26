use super::*;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceW as NativeDiskFreeSpaceW, GetVolumePathNameW,
};

fn wides(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain([0]).collect()
}

fn from_wide(path: &[u16]) -> String {
    String::from_utf16(path.split(|&u| u == 0).next().unwrap()).unwrap()
}

fn last_error() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap()
}

fn read_title(path: &[u16]) -> Result<Vec<u8>, i32> {
    let handle = CreateFileW(path.as_ptr(), 0x8000_0000, 1, null(), 3, 0, null_mut());
    if handle as isize == -1 {
        return Err(last_error());
    }
    let close = host!("kernel32.dll", "CloseHandle": extern "system" fn(Handle) -> i32);
    let (mut bytes, mut n) = ([0x5a; 64], 0);
    let ok = ReadFile(
        handle,
        bytes.as_mut_ptr(),
        bytes.len() as u32,
        &mut n,
        null_mut(),
    );
    let error = (ok == 0).then(last_error);
    assert_ne!(close(handle), 0);
    match error {
        Some(code) => Err(code),
        None => Ok(bytes[..n as usize].to_vec()),
    }
}

fn disk(path: &[u16], hle: bool) -> Result<[u32; 4], i32> {
    let [mut spc, mut bps, mut free, mut total] = [0xdead_beef; 4];
    let ok = if hle {
        GetDiskFreeSpaceW(path.as_ptr(), &mut spc, &mut bps, &mut free, &mut total)
    } else {
        // SAFETY: NUL-terminated path and four valid outputs.
        unsafe { NativeDiskFreeSpaceW(path.as_ptr(), &mut spc, &mut bps, &mut free, &mut total) }
    };
    if ok == 0 {
        Err(last_error())
    } else {
        Ok([spc, bps, free, total])
    }
}

#[test]
fn real_mount_and_volume_api() {
    const DATA: &[u8] = b"TEN-05 real Win32 data \xa5\x19\xfe";
    let cwd = std::env::current_dir().unwrap();
    let temp = tempfile::Builder::new()
        .prefix("ten05-")
        .tempdir_in(&cwd)
        .unwrap();
    let folder = temp.path().join("nested");
    std::fs::create_dir(&folder).unwrap();
    let real_file = folder.join("probe.bin");
    std::fs::write(&real_file, DATA).unwrap();
    assert_eq!(std::fs::read(&real_file).unwrap(), DATA);

    let mut volume = [0_u16; 512];
    let input = wides(&folder);
    // SAFETY: NUL-terminated input and the output buffer's real length.
    assert_ne!(
        unsafe { GetVolumePathNameW(input.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) },
        0
    );
    let volume = from_wide(&volume);
    let verbatim_volume = format!(r"\\?\{volume}");
    let volume_root = Path::new(&volume);
    let verbose_folder = format!(r"\\?\{}", folder.display());
    let letter = mount(folder.as_path()).unwrap();
    let expect_head = std::env::var("TEN_PATH_REVISION").as_deref() != Ok("parent");
    let mut failed = Vec::new();

    // Each row has the same on-disk file, reached via a different root.
    for (name, mount_root, tail) in [
        ("folder", folder.as_path(), Path::new("probe.bin")),
        (
            "verbatim_folder",
            Path::new(&verbose_folder),
            Path::new("probe.bin"),
        ),
        (
            "drive_root",
            volume_root,
            real_file.strip_prefix(volume_root).unwrap(),
        ),
        (
            "verbatim_drive_root",
            Path::new(&verbatim_volume),
            real_file.strip_prefix(volume_root).unwrap(),
        ),
    ] {
        mount_at(letter, mount_root);
        for (kind, prefix, sep) in [
            ("plain", "", '\\'),
            ("plain_slash", "", '/'),
            ("verbatim", r"\\?\", '\\'),
            ("verbatim_slash", r"\\?\", '/'),
        ] {
            let tail = tail.to_str().unwrap().replace('\\', &sep.to_string());
            let file_path = format!("{prefix}{letter}:{sep}{tail}");
            let title = wides(Path::new(&file_path));
            let translated = translate(&title[..title.len() - 1]).unwrap();
            let read = read_title(&title);
            let read_ok = read.as_deref() == Ok(DATA);
            eprintln!(
                "TEN_PATH_FILE,{name},{kind},title={file_path:?},host={:?},read={read:?},matches={read_ok}",
                from_wide(&translated)
            );
            if expect_head && kind != "plain_slash" && !read_ok {
                failed.push(format!("{name}/{kind}: {read:?}"));
            }
            if kind == "plain_slash" && !name.starts_with("verbatim") {
                assert!(read_ok, "{name}/{kind}");
            }

            let virtual_root = format!("{prefix}{letter}:{sep}");
            let user_root = wides(Path::new(&virtual_root));
            let native_root = if prefix.is_empty() {
                &volume
            } else {
                &verbatim_volume
            };
            let native_root = wides(Path::new(native_root));
            let baseline_before = disk(&native_root, false).unwrap();
            let actual = disk(&user_root, true);
            let baseline_after = disk(&native_root, false).unwrap();
            let translated_root = translate(&user_root[..user_root.len() - 1]).unwrap();
            let direct_translated = disk(&translated_root, false);
            eprintln!(
                "TEN_PATH_DISK,{name},{kind},host={:?},win32_before={baseline_before:?},hle={actual:?},win32_after={baseline_after:?},win32_on_untrimmed={direct_translated:?}",
                from_wide(&translated_root)
            );
            if let Ok(values) = actual {
                for field in [0, 1, 3] {
                    assert_eq!(
                        values[field], baseline_before[field],
                        "{name}/{kind} disk field {field}"
                    );
                    assert_eq!(
                        values[field], baseline_after[field],
                        "{name}/{kind} disk field {field}"
                    );
                }
                assert!(values[2] <= values[3], "{name}/{kind} free clusters");
            } else if expect_head {
                failed.push(format!("{name}/{kind}: disk {actual:?}"));
            }
        }
    }
    let long_part_a = "a".repeat(130);
    let long_part_b = "b".repeat(135);
    let long_folder = folder.join(&long_part_a).join(&long_part_b);
    std::fs::create_dir_all(&long_folder).unwrap();
    let long_file = long_folder.join("long.bin");
    std::fs::write(&long_file, DATA).unwrap();
    for (name, root) in [
        ("folder", folder.as_path()),
        ("verbatim_folder", Path::new(&verbose_folder)),
    ] {
        mount_at(letter, root);
        let title = wides(Path::new(&format!(
            "{letter}:/{long_part_a}/./{long_part_b}/long.bin"
        )));
        let host = translate(&title[..title.len() - 1]).unwrap();
        let read = read_title(&title);
        let read_ok = read.as_deref() == Ok(DATA);
        eprintln!(
            "TEN_PATH_LONG,{name},host={:?},read={read:?},matches={read_ok}",
            from_wide(&host)
        );
        if (name == "folder" || expect_head) && !read_ok {
            failed.push(format!("{name}/long: {read:?}"));
        }
    }
    assert!(failed.is_empty(), "{failed:?}");
}
