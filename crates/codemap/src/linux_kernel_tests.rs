use super::*;
use std::collections::BTreeMap;

const LIVE_LABELS: &[&str] = &[
    "guarded_real_kernel_image",
    "kernel_fingerprint_reproducible",
    "unguarded_real_child",
    "real_vdso_wrong_digest",
    "real_vdso_wrong_size",
    "real_vdso_memory_tampered",
    "unaccepted_anonymous_policy",
    "earlier_exec_callback",
];

fn auxv(address: u64) -> Vec<u8> {
    [33u64, address, 0, 0]
        .into_iter()
        .flat_map(u64::to_le_bytes)
        .collect()
}

fn image() -> Vec<u8> {
    let mut bytes = vec![0; 4096];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&3u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
    bytes[64..68].copy_from_slice(&1u32.to_le_bytes());
    bytes[68..72].copy_from_slice(&5u32.to_le_bytes());
    bytes[96..104].copy_from_slice(&4096u64.to_le_bytes());
    bytes[104..112].copy_from_slice(&4096u64.to_le_bytes());
    bytes
}

fn pins(bytes: &[u8]) -> LinuxKernelPins {
    LinuxKernelPins {
        architecture: LinuxArchitecture::X86_64,
        vdso_size: bytes.len() as u64,
        vdso_sha256: sha256_hex(bytes),
        vsyscall: LinuxVsyscallPolicy::Absent,
    }
}

fn record(
    observed: &mut BTreeMap<&'static str, &'static str>,
    label: &'static str,
    result: Result<()>,
    reason: Option<&str>,
) {
    let accepted = result.is_ok();
    if let Some(reason) = reason {
        let error = result.expect_err(label).to_string();
        assert!(error.contains(reason), "{label}: {error}");
    } else {
        result.expect(label);
    }
    assert!(
        observed
            .insert(label, if accepted { "accept" } else { "reject" })
            .is_none()
    );
}

#[test]
fn frozen_kernel_descriptor_auxv_and_mapping_controls() {
    let mut observed = BTreeMap::new();
    descriptor_controls(&mut observed);
    auxv_controls(&mut observed);
    vsyscall_controls(&mut observed);
    mapping_refusal_controls(&mut observed);
    elf_controls(&mut observed);
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../toolchain/linux/kernel_image_expectations.json"
    ))
    .unwrap();
    let expected: BTreeMap<_, _> = oracle["expected"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(label, _)| !LIVE_LABELS.contains(&label.as_str()))
        .map(|(label, value)| (label.as_str(), value.as_str().unwrap()))
        .collect();
    assert_eq!(observed, expected);
}

fn descriptor_controls(observed: &mut BTreeMap<&'static str, &'static str>) {
    let bytes = image();
    let pins = pins(&bytes);
    record(observed, "native_kernel_descriptor", pins.validate(), None);
    for (label, change) in [
        ("invalid_kernel_digest", 0),
        ("oversized_kernel_image", 1),
        ("unaligned_kernel_size", 2),
        ("arm_fixed_vsyscall_policy", 3),
    ] {
        let mut bad = pins.clone();
        match change {
            0 => bad.vdso_sha256 = "q".repeat(64),
            1 => bad.vdso_size = 2 * 1024 * 1024,
            2 => bad.vdso_size = 4097,
            _ => {
                bad.architecture = LinuxArchitecture::Aarch64;
                bad.vsyscall = LinuxVsyscallPolicy::FixedEmulated;
            }
        }
        record(
            observed,
            label,
            bad.validate(),
            Some("invalid Linux kernel image pins"),
        );
    }
}

fn auxv_controls(observed: &mut BTreeMap<&'static str, &'static str>) {
    let auxv = auxv(0x1000);
    record(
        observed,
        "complete_auxv",
        kernel_address(&auxv).map(|address| assert_eq!(address, 0x1000)),
        None,
    );
    for (label, bad, reason) in [
        ("truncated_auxv", auxv[..31].to_vec(), "incomplete"),
        (
            "trailing_auxv_pairs",
            [auxv.clone(), vec![0; 16]].concat(),
            "terminator",
        ),
        ("missing_auxv_terminator", auxv[..16].to_vec(), "terminator"),
        (
            "duplicate_auxv_tag",
            [auxv[..16].to_vec(), auxv.clone()].concat(),
            "duplicate",
        ),
        (
            "missing_vdso_auxv",
            vec![0; 16],
            "missing Linux vDSO auxiliary address",
        ),
        (
            "zero_vdso_auxv",
            super::tests::auxv(0),
            "missing Linux vDSO auxiliary address",
        ),
    ] {
        record(
            observed,
            label,
            kernel_address(&bad).map(|_| ()),
            Some(reason),
        );
    }
}

fn vsyscall_controls(observed: &mut BTreeMap<&'static str, &'static str>) {
    let bytes = image();
    let pins = pins(&bytes);
    let auxv = auxv(0x1000);
    let report = "1000-2000 r-xp 00000000 00:00 0 [vdso]\n";
    record(
        observed,
        "native_vdso_mapping",
        kernel_mapping(&parse_maps(report).unwrap(), &auxv, &pins).map(|_| ()),
        None,
    );
    let vsyscall = "ffffffffff600000-ffffffffff601000 --xp 00000000 00:00 0 [vsyscall]\n";
    let mut x86 = pins.clone();
    x86.vsyscall = LinuxVsyscallPolicy::FixedEmulated;
    record(
        observed,
        "native_x86_fixed_vsyscall",
        kernel_mapping(
            &parse_maps(&(report.to_owned() + vsyscall)).unwrap(),
            &auxv,
            &x86,
        )
        .map(|_| ()),
        None,
    );
    let mut arm = pins;
    arm.architecture = LinuxArchitecture::Aarch64;
    record(
        observed,
        "native_arm_absent_vsyscall",
        kernel_mapping(&parse_maps(report).unwrap(), &auxv, &arm).map(|_| ()),
        None,
    );
    for (label, bad, selected) in [
        (
            "vsyscall_wrong_range",
            report.to_owned() + &vsyscall.replace("ffffffffff600000", "ffffffffff5ff000"),
            x86.clone(),
        ),
        (
            "vsyscall_wrong_permissions",
            report.to_owned() + &vsyscall.replace("--xp", "r-xp"),
            x86.clone(),
        ),
        ("vsyscall_on_arm", report.to_owned() + vsyscall, arm),
        ("missing_selected_vsyscall", report.to_owned(), x86),
    ] {
        record(
            observed,
            label,
            kernel_mapping(&parse_maps(&bad).unwrap(), &auxv, &selected).map(|_| ()),
            Some("vsyscall mapping"),
        );
    }
}

fn mapping_refusal_controls(observed: &mut BTreeMap<&'static str, &'static str>) {
    let bytes = image();
    let pins = pins(&bytes);
    let auxv = auxv(0x1000);
    let report = "1000-2000 r-xp 00000000 00:00 0 [vdso]\n";
    for (label, bad, reason) in [
        (
            "missing_vdso_map",
            report.replace("[vdso]", "[vvar]"),
            "unapproved",
        ),
        (
            "duplicate_vdso_map",
            report.to_owned() + "2000-3000 r-xp 00000000 00:00 0 [vdso]\n",
            "invalid Linux vDSO mapping",
        ),
        (
            "vdso_address_mismatch",
            report.replace("1000-2000", "2000-3000"),
            "invalid Linux vDSO mapping",
        ),
        (
            "fake_vdso_label",
            report.replace("[vdso]", "[anon:vdso]"),
            "unapproved",
        ),
        (
            "vdso_writable",
            report.replace("r-xp", "rwxp"),
            "invalid Linux vDSO mapping",
        ),
        (
            "vdso_non_executable",
            report.replace("r-xp", "r--p"),
            "invalid Linux vDSO mapping",
        ),
        (
            "vdso_shared",
            report.replace("r-xp", "r-xs"),
            "invalid Linux vDSO mapping",
        ),
        (
            "vdso_nonzero_offset",
            report.replace("00000000", "00001000"),
            "invalid Linux vDSO mapping",
        ),
        (
            "vdso_nonzero_device",
            report.replace("00:00", "00:01"),
            "invalid Linux vDSO mapping",
        ),
        (
            "vdso_file_inode",
            report.replace("00:00 0", "00:00 1"),
            "unverifiable Linux anonymous mapping",
        ),
        (
            "vdso_size_mismatch",
            report.replace("1000-2000", "1000-3000"),
            "invalid Linux vDSO mapping",
        ),
        (
            "anonymous_rx",
            report.to_owned() + "2000-3000 r-xp 00000000 00:00 0\n",
            "unapproved",
        ),
        (
            "anonymous_wx",
            report.to_owned() + "2000-3000 rwxp 00000000 00:00 0\n",
            "unapproved",
        ),
        (
            "unexpected_kernel_executable",
            report.to_owned() + "2000-3000 r-xp 00000000 00:00 0 [vvar]\n",
            "unapproved",
        ),
    ] {
        let result =
            parse_maps(&bad).and_then(|maps| kernel_mapping(&maps, &auxv, &pins).map(|_| ()));
        record(observed, label, result, Some(reason));
    }
}

fn elf_controls(observed: &mut BTreeMap<&'static str, &'static str>) {
    let bytes = image();
    let pins = pins(&bytes);
    for (label, change) in [
        ("vdso_digest_mismatch", 0),
        ("vdso_wrong_machine", 1),
        ("vdso_wrong_elf_kind", 2),
        ("vdso_invalid_program_table", 3),
    ] {
        let mut bad = bytes.clone();
        let mut selected = pins.clone();
        match change {
            0 => bad[200] ^= 1,
            1 => bad[18..20].copy_from_slice(&183u16.to_le_bytes()),
            2 => bad[16..18].copy_from_slice(&2u16.to_le_bytes()),
            _ => bad[32..40].copy_from_slice(&4096u64.to_le_bytes()),
        }
        if change != 0 {
            selected.vdso_sha256 = sha256_hex(&bad);
        }
        let reason = match change {
            0 => "operator byte pins",
            1 | 2 => "ELF header",
            _ => "program table",
        };
        record(
            observed,
            label,
            verify_vdso_bytes(&bad, &selected),
            Some(reason),
        );
    }
}
