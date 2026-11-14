use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::archive::Backend;
use crate::archive::writer::{self, CopyEntry, EntrySource, MemberReader};
use crate::error::{AppError, Result};
use crate::graph::ClosureMode;
use crate::split::{self, SplitOptions};
use crate::util::create_temp_dir;
use crate::verify;

const PART_A: &str = "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\n\
<partsmanagement count=\"1\" type=\"EPLAN.PartsManagement\" version=\"2022.0\">\n\
 <part P_ARTICLE_PARTNR=\"SYN.001\" P_ARTICLE_MANUFACTURER=\"SYN\" P_ARTICLE_MACRO=\"$(MD_MACROS)\\Syn\\A.ema\" P_ARTICLE_PICTUREFILE=\"$(MD_IMG)\\Syn\\a.png\" P_ARTICLE_EXTERNAL_DOCUMENT_1=\"https://example.com/datasheet\">\n\
  <variant P_ARTICLE_VARIANT=\"1\" />\n\
 </part>\n\
</partsmanagement>";

const PART_B: &str = "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\n\
<partsmanagement count=\"1\" type=\"EPLAN.PartsManagement\" version=\"2022.0\">\n\
 <part P_ARTICLE_PARTNR=\"SYN.002\" P_ARTICLE_MANUFACTURER=\"SYN\">\n\
  <variant P_ARTICLE_VARIANT=\"1\" />\n\
 </part>\n\
</partsmanagement>";

const MANUFACTURER: &str = "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\n\
<partsmanagement count=\"1\" type=\"EPLAN.PartsManagement\" version=\"2022.0\">\n\
 <manufacturer P_ARTICLE_MANUFACTURER=\"SYN\" />\n\
</partsmanagement>";

const MANIFEST: &str = "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\n\
<manifest version=\"2.0\">\n\
 <packages>\n\
  <package type=\"part\" key=\"SYN.001\" name=\"SYN.001\">\n\
   <items>\n\
    <item type=\"partxml\" name=\"part\" locator=\"SYN.001.part.xml\"/>\n\
    <item type=\"partxml\" name=\"manufacturer\" locator=\"SYN.manufacturer.xml\"/>\n\
    <item type=\"macro\" name=\"graphicmacro\" locator=\"Syn\\A.ema\"/>\n\
    <item type=\"picture\" name=\"picturefile\" locator=\"Syn\\a.png\"/>\n\
   </items>\n\
  </package>\n\
  <package type=\"part\" key=\"SYN.002\" name=\"SYN.002\">\n\
   <items>\n\
    <item type=\"partxml\" name=\"part\" locator=\"SYN.002.part.xml\"/>\n\
    <item type=\"partxml\" name=\"manufacturer\" locator=\"SYN.manufacturer.xml\"/>\n\
   </items>\n\
  </package>\n\
 </packages>\n\
</manifest>";

fn never_read(_: usize) -> Result<Vec<u8>> {
    Err(AppError::message("no archive members expected"))
}

fn write_synthetic(path: &Path, extra: &[(String, Vec<u8>)]) -> Result<()> {
    let mut files: Vec<(String, Vec<u8>)> = vec![
        (
            "items\\partxml\\SYN.001.part.xml".to_string(),
            PART_A.as_bytes().to_vec(),
        ),
        (
            "items\\partxml\\SYN.002.part.xml".to_string(),
            PART_B.as_bytes().to_vec(),
        ),
        (
            "items\\partxml\\SYN.manufacturer.xml".to_string(),
            MANUFACTURER.as_bytes().to_vec(),
        ),
        (
            "items\\macro\\Syn\\A.ema".to_string(),
            b"FAKE-MACRO-A".to_vec(),
        ),
        (
            "items\\picture\\Syn\\a.png".to_string(),
            vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A],
        ),
        ("manifest.xml".to_string(), MANIFEST.as_bytes().to_vec()),
    ];
    files.extend_from_slice(extra);

    let entries: Vec<CopyEntry> = files
        .iter()
        .map(|(key, bytes)| CopyEntry {
            key: key.clone(),
            size: bytes.len() as u64,
            source: EntrySource::Bytes(Arc::new(bytes.clone())),
        })
        .collect();

    let reader = never_read;
    writer::write_copy(path, &entries, &reader as &dyn MemberReader)
}

fn options(archive: PathBuf, out_dir: PathBuf) -> SplitOptions {
    let mut options = SplitOptions::new(archive, out_dir);
    options.closure = ClosureMode::Full;
    options.fix_refs = "auto".to_string();
    options.verify_structural = true;
    options
}

pub fn run() -> Result<i32> {
    let root = create_temp_dir("edzsplit-selftest")?;
    let out_dir = root.join("out");
    std::fs::create_dir_all(&out_dir)?;
    let source = root.join("synthetic-two-parts.edz");

    let outcome = run_steps(&root, &out_dir, &source);
    match outcome {
        Ok(0) => {
            let _ = std::fs::remove_dir_all(&root);
            Ok(0)
        }
        Ok(code) => Ok(code),
        Err(error) => {
            println!("SELFTEST FAIL: {error}");
            println!("selftest workspace kept at: {}", root.display());
            Ok(4)
        }
    }
}

fn run_steps(root: &Path, out_dir: &Path, source: &Path) -> Result<i32> {
    // 1) Fabricate a synthetic two-part EDZ.
    write_synthetic(source, &[])?;
    println!("selftest: synthetic EDZ written -> {}", source.display());

    // 2) Verify the synthetic source round-trips through 7z.
    let source_check = verify::run(source, false, true, Backend::Auto);
    println!(
        "selftest: source verify Ok={} errors={}",
        source_check.ok,
        source_check.errors.len()
    );
    if !source_check.ok {
        return fail(
            &format!(
                "Source EDZ failed structural verification: {}",
                source_check.errors.join("; ")
            ),
            root,
        );
    }

    // 3) Split it.
    let report = split::run(&options(source.to_path_buf(), out_dir.to_path_buf()))?;
    println!(
        "selftest: split planned={} written={} failed={}",
        report.parts_planned, report.parts_written, report.parts_failed
    );
    if report.parts_failed > 0 {
        return fail(
            &format!("Split reported failures: {}", report.failed.join("; ")),
            root,
        );
    }
    if report.parts_written != 2 {
        return fail(
            &format!("Expected 2 outputs, got {}", report.parts_written),
            root,
        );
    }

    // 4) Verify both outputs.
    for name in ["SYN.001.edz", "SYN.002.edz"] {
        let path = out_dir.join(name);
        let result = verify::run(&path, false, true, Backend::Auto);
        println!(
            "selftest: {name} verify Ok={} errors={} warnings={}",
            result.ok,
            result.errors.len(),
            result.warnings.len()
        );
        if !result.ok {
            return fail(
                &format!(
                    "Output failed verification: {} :: {}",
                    path.display(),
                    result.errors.join("; ")
                ),
                root,
            );
        }
    }

    // 5) Split only one part.
    let out_one = root.join("out-one");
    let mut only = options(source.to_path_buf(), out_one.clone());
    only.only = vec!["SYN.001".to_string()];
    let one = split::run(&only)?;
    println!("selftest: --only SYN.001 -> written={}", one.parts_written);
    if one.parts_written != 1 {
        return fail("--only filter did not produce exactly one output", root);
    }
    if !out_one.join("SYN.001.edz").is_file() || out_one.join("SYN.002.edz").exists() {
        return fail("--only filter produced the wrong file set", root);
    }

    // 6) Dry run writes nothing.
    let out_dry = root.join("out-dry");
    let mut dry = options(source.to_path_buf(), out_dry.clone());
    dry.dry_run = true;
    let plan = split::run(&dry)?;
    println!(
        "selftest: --dry-run planned={} written={}",
        plan.parts_planned, plan.parts_written
    );
    if plan.parts_planned != 2 || plan.parts_written != 0 {
        return fail(
            "--dry-run did not plan exactly two parts and write nothing",
            root,
        );
    }
    if out_dry.join("SYN.001.edz").exists() || out_dry.join("SYN.002.edz").exists() {
        return fail("--dry-run wrote an EDZ", root);
    }

    // 7) Resume skips everything already done.
    let out_resume = root.join("out-resume");
    let first = split::run(&options(source.to_path_buf(), out_resume.clone()))?;
    if first.parts_written != 2 {
        return fail("resume pre-run did not write both parts", root);
    }
    let mut resumed = options(source.to_path_buf(), out_resume);
    resumed.resume = true;
    let second = split::run(&resumed)?;
    println!(
        "selftest: --resume written={} skipped={}",
        second.parts_written, second.parts_skipped_resume
    );
    if second.parts_written != 0 || second.parts_skipped_resume != 2 {
        return fail("--resume did not skip both completed parts", root);
    }

    // 8) Path traversal is rejected by verification.
    let traversal = root.join("traversal.edz");
    write_synthetic(
        &traversal,
        &[("../escape.txt".to_string(), b"escape".to_vec())],
    )?;
    let traversal_check = verify::run(&traversal, false, false, Backend::Auto);
    println!(
        "selftest: traversal verify Ok={} errors={}",
        traversal_check.ok,
        traversal_check.errors.len()
    );
    if traversal_check.ok
        || !traversal_check
            .errors
            .iter()
            .any(|error| error.contains("Unsafe entry path"))
    {
        return fail("path traversal entry was not rejected", root);
    }

    // 9) --strict-part still writes the seed part.
    let out_strict = root.join("out-strict");
    let mut strict = options(source.to_path_buf(), out_strict.clone());
    strict.strict_part = true;
    let strict_report = split::run(&strict)?;
    println!(
        "selftest: --strict-part planned={} written={}",
        strict_report.parts_planned, strict_report.parts_written
    );
    if strict_report.parts_written != 2 {
        return fail("--strict-part changed the single-part output count", root);
    }
    let strict_check = verify::run(&out_strict.join("SYN.001.edz"), false, true, Backend::Auto);
    if !strict_check.ok {
        return fail("--strict-part output failed verification", root);
    }

    println!("SELFTEST PASS");
    Ok(0)
}

fn fail(message: &str, root: &Path) -> Result<i32> {
    println!("SELFTEST FAIL: {message}");
    println!("selftest workspace kept at: {}", root.display());
    Ok(4)
}
