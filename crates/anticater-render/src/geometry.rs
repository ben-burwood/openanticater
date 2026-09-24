//! Scene geometry for the VK-01 viewer.
//!
//! The base, light seam and knob meshes are **rendered from `hardware/vk01.scad`
//! by OpenSCAD** at build time (see `build.rs`) and loaded here as STL — so the
//! `.scad` file is the single source of truth for the shape. STL stores loose
//! per-face triangles, so we weld coincident vertices and recompute smooth
//! per-vertex normals for nicer shading.
//!
//! The only procedural mesh is the glow halo, which is a rendering effect (a
//! soft additive band around the seam), not part of the physical design.

use std::f32::consts::TAU;

use glam::Vec3;

/// A position + normal vertex, laid out to match the wgpu vertex buffer.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
}

/// One renderable mesh: interleaved vertices plus a triangle index list.
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

/// The four parts of the assembly, each a separate mesh so they can carry
/// distinct materials and (for the knob) their own animated transform.
pub struct Scene {
    pub base: Mesh,
    pub seam: Mesh,
    pub knob: Mesh,
    pub glow: Mesh,
    /// Z of the assembly's mid-point — the camera's orbit target.
    pub center_z: f32,
}

// STL meshes rendered from vk01.scad at build time (see build.rs).
const BASE_STL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/base.stl"));
const SEAM_STL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/seam.stl"));
const KNOB_STL: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/knob.stl"));

/// Build every part of the VK-01 assembly.
pub fn build_scene() -> Scene {
    let base = load_stl(BASE_STL);
    let seam = load_stl(SEAM_STL);
    let knob = load_stl(KNOB_STL);
    let glow = build_glow(&seam);
    let center_z = combined_center_z(&[&base, &seam, &knob]);
    Scene {
        base,
        seam,
        knob,
        glow,
        center_z,
    }
}

// ---- STL loading -----------------------------------------------------------

/// Parse a binary or ASCII STL into a welded, smooth-normal mesh.
fn load_stl(bytes: &[u8]) -> Mesh {
    let tris = if is_binary_stl(bytes) {
        parse_binary_stl(bytes)
    } else {
        parse_ascii_stl(bytes)
    };
    weld(&tris)
}

/// Binary STL is `84 + 50 * triangle_count` bytes exactly.
fn is_binary_stl(b: &[u8]) -> bool {
    if b.len() < 84 {
        return false;
    }
    let n = u32::from_le_bytes([b[80], b[81], b[82], b[83]]) as usize;
    b.len() == 84 + n * 50
}

fn parse_binary_stl(b: &[u8]) -> Vec<[f32; 3]> {
    let n = u32::from_le_bytes([b[80], b[81], b[82], b[83]]) as usize;
    let mut out = Vec::with_capacity(n * 3);
    let read = |off: usize| f32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]]);
    for t in 0..n {
        let base = 84 + t * 50 + 12; // skip the 3-float face normal
        for v in 0..3 {
            let o = base + v * 12;
            out.push([read(o), read(o + 4), read(o + 8)]);
        }
    }
    out
}

fn parse_ascii_stl(b: &[u8]) -> Vec<[f32; 3]> {
    let text = String::from_utf8_lossy(b);
    let mut out = Vec::new();
    let mut it = text.split_whitespace();
    while let Some(tok) = it.next() {
        if tok == "vertex" {
            let x = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
            let y = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
            let z = it.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
            out.push([x, y, z]);
        }
    }
    out
}

/// Weld coincident vertices (STL duplicates them per face) and accumulate
/// area-weighted face normals into smooth per-vertex normals.
fn weld(tri_positions: &[[f32; 3]]) -> Mesh {
    use std::collections::HashMap;

    // Quantise to 1 µm to merge vertices shared between adjacent triangles.
    let key = |p: [f32; 3]| -> [i32; 3] {
        [
            (p[0] * 1000.0).round() as i32,
            (p[1] * 1000.0).round() as i32,
            (p[2] * 1000.0).round() as i32,
        ]
    };

    let mut map: HashMap<[i32; 3], u32> = HashMap::new();
    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut indices: Vec<u32> = Vec::with_capacity(tri_positions.len());

    for &p in tri_positions {
        let k = key(p);
        let idx = *map.entry(k).or_insert_with(|| {
            positions.push(p);
            (positions.len() - 1) as u32
        });
        indices.push(idx);
    }

    let mut normals = vec![Vec3::ZERO; positions.len()];
    for tri in indices.chunks_exact(3) {
        let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        let (pa, pb, pc) = (
            Vec3::from(positions[a]),
            Vec3::from(positions[b]),
            Vec3::from(positions[c]),
        );
        // Cross product is area-weighted, biasing normals toward larger faces.
        let fnrm = (pb - pa).cross(pc - pa);
        normals[a] += fnrm;
        normals[b] += fnrm;
        normals[c] += fnrm;
    }

    let vertices = positions
        .iter()
        .zip(normals.iter())
        .map(|(&pos, &n)| Vertex {
            pos,
            normal: n.normalize_or(Vec3::Z).to_array(),
        })
        .collect();

    Mesh { vertices, indices }
}

// ---- Glow halo (procedural render effect) ----------------------------------

/// A soft additive band around the light seam, sized from the seam's bounds.
fn build_glow(seam: &Mesh) -> Mesh {
    // Single pass over the seam: outer radius and vertical extent.
    let mut radius = 0.0f32;
    let mut zlo = f32::INFINITY;
    let mut zhi = f32::NEG_INFINITY;
    for v in &seam.vertices {
        radius = radius.max(v.pos[0].hypot(v.pos[1]));
        zlo = zlo.min(v.pos[2]);
        zhi = zhi.max(v.pos[2]);
    }
    let radius = radius + 1.5;
    let z0 = zlo - 1.0;
    let z1 = zhi + 1.0;

    let n = 96usize;
    let mut vertices = Vec::with_capacity(n * 2);
    for &z in &[z0, z1] {
        for i in 0..n {
            let a = TAU * (i as f32 / n as f32);
            vertices.push(Vertex {
                pos: [radius * a.cos(), radius * a.sin(), z],
                // Normals are unused for the emissive glow pass.
                normal: [a.cos(), a.sin(), 0.0],
            });
        }
    }
    let mut indices = Vec::with_capacity(n * 6);
    for i in 0..n as u32 {
        let j = (i + 1) % n as u32;
        let (b0, t0) = (i, i + n as u32);
        let (b1, t1) = (j, j + n as u32);
        indices.extend_from_slice(&[b0, t0, t1, b0, t1, b1]);
    }
    Mesh { vertices, indices }
}

// ---- small helpers ---------------------------------------------------------

fn combined_center_z(meshes: &[&Mesh]) -> f32 {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for m in meshes {
        for v in &m.vertices {
            lo = lo.min(v.pos[2]);
            hi = hi.max(v.pos[2]);
        }
    }
    0.5 * (lo + hi)
}
