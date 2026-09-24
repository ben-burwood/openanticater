//! Build step: render `hardware/vk01.scad` into per-part STL meshes.
//!
//! The `.scad` file is the single source of truth for the knob's shape. When
//! OpenSCAD is available we invoke it to export each part (base / seam / knob)
//! freshly into `OUT_DIR`; otherwise we fall back to the committed snapshots in
//! `assets/` so the crate still builds without OpenSCAD installed.
//!
//! We use OpenSCAD's fast Manifold backend, which renders the whole assembly —
//! including the knob's twisted-extrude-minus-knurl CSG — at the design's native
//! `$fn = 128` in a fraction of a second, so no tessellation reduction is
//! needed. (The committed `assets/` fallback is a lighter snapshot, used only
//! when OpenSCAD isn't installed.) On older OpenSCAD builds without Manifold we
//! retry with the default backend, and finally fall back to `assets/`.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Parts to export, each rendered at the design's own resolution.
const PARTS: [&str; 3] = ["base", "seam", "knob"];

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let scad = manifest.join("../../hardware/vk01.scad");
    let assets = manifest.join("assets");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());

    println!("cargo:rerun-if-changed={}", scad.display());
    println!("cargo:rerun-if-changed=assets");
    println!("cargo:rerun-if-env-changed=OPENSCAD");

    let openscad = find_openscad();
    if openscad.is_none() {
        println!("cargo:warning=OpenSCAD not found; using committed assets/ meshes. Set OPENSCAD to re-render vk01.scad.");
    }

    for part in PARTS {
        let dst = out.join(format!("{part}.stl"));
        let rendered = openscad
            .as_ref()
            .map(|bin| render_part(bin, &scad, part, &dst))
            .unwrap_or(false);

        if !rendered {
            let asset = assets.join(format!("{part}.stl"));
            std::fs::copy(&asset, &dst).unwrap_or_else(|e| {
                panic!(
                    "no OpenSCAD render and no committed asset {}: {e}",
                    asset.display()
                )
            });
        }
    }
}

/// Run OpenSCAD to export one part; return true on success. Tries the fast
/// Manifold backend first, then the default backend for older OpenSCAD.
fn render_part(bin: &Path, scad: &Path, part: &str, dst: &Path) -> bool {
    if run_export(bin, scad, part, dst, true) {
        return true;
    }
    // Manifold may be unavailable on older OpenSCAD — retry without it.
    if run_export(bin, scad, part, dst, false) {
        return true;
    }
    println!("cargo:warning=OpenSCAD could not export part '{part}'; using committed asset.");
    false
}

fn run_export(bin: &Path, scad: &Path, part: &str, dst: &Path, manifold: bool) -> bool {
    let mut cmd = Command::new(bin);
    if manifold {
        cmd.arg("--backend").arg("Manifold");
    }
    cmd.arg("--export-format")
        .arg("binstl")
        .arg("-o")
        .arg(dst)
        .arg("-D")
        .arg(format!("part=\"{part}\""))
        .arg(scad);

    matches!(cmd.status(), Ok(s) if s.success())
}

/// Locate an OpenSCAD executable: `$OPENSCAD`, then common install paths, then
/// the `PATH` (probed with `--version`).
fn find_openscad() -> Option<PathBuf> {
    if let Ok(p) = env::var("OPENSCAD") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let candidates = [
        r"C:\Program Files\OpenSCAD\openscad.exe",
        r"C:\Program Files\OpenSCAD (Nightly)\openscad.exe",
        "/usr/bin/openscad",
        "/usr/local/bin/openscad",
        "/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD",
    ];
    for c in candidates {
        let p = PathBuf::from(c);
        if p.exists() {
            return Some(p);
        }
    }
    // Last resort: rely on PATH.
    if Command::new("openscad").arg("--version").status().is_ok() {
        return Some(PathBuf::from("openscad"));
    }
    None
}
