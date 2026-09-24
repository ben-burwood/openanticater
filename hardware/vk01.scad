// =============================================================================
//  Anticater VK-01 Desktop Volume Control Knob
//  Parametric visual model for OpenSCAD
//
//  *visual* representation of the Anticater VK-01:
//  CNC-aluminium square base with rounded corners, round knurled knob on
//  top, a glowing RGB seam between them and a rear USB-C port.
// =============================================================================

// -------- Quality ------------------------------------------------------------
$fn = 128;              // curve smoothness (drop to 64 for faster previews)

// -------- Unit --------------------------------------------------------------
total_h = 33;

// -------- Base (CNC aluminium body) -----------------------------------------
base_w        = 60;     // square footprint X / Y
base_h        = 16;     // base body height
corner_r      = 2;      // rounded corner radius
top_chamfer   = 0.6;    // subtle chamfer on the top edge of the base
bottom_chamfer= top_chamfer;    // subtle chamfer on the bottom edge

// -------- Knob ---------------------------------------------------------------
knob_d        = 39;     // knob diameter
knob_h        = total_h - base_h;   // knob height (above the light seam)

// -------- Knurling (angled / helical flutes on the knob side) ----------------
knurl_count   = 80;     // number of flutes around the circumference
knurl_depth   = 0.4;    // how deep each flute is cut
knurl_tool_d  = 2;    // cutter diameter (bigger = wider, softer flutes)
knurl_angle   = 35;     // helix angle of the flutes from vertical (degrees)
knurl_slices  = 80;     // vertical resolution of the twisted extrude

// -------- RGB light seam -----------------------------------------------------
light_gap     = 1.4;    // height of the glowing ring between base and knob
light_over    = -0.4;    // how far the ring stands proud of the knob radius

// -------- Rear USB-C port ----------------------------------------------------
usbc_w        = 9.0;    // port opening width
usbc_h        = 3.4;    // port opening height
usbc_z        = 8;      // height of port centre above the desk
usbc_r        = 1.5;    // rounded corners of the port

// -------- Colours ------------------------------------------------------------
col_base = [0.4, 0.4, 0.4];  // brushed aluminium
col_knob = [0.3, 0.3, 0.3];  // dark anodized knob
col_led  = [0.15, 0.85, 1.00];  // cyan RGB glow

// -------- Derived ------------------------------------------------------------
knob_z   = base_h + light_gap;   // z where the knob body starts
led_z    = base_h;               // z where the light seam starts
eps      = 0.01;

// Twist (degrees over the knob height) that yields the requested helix angle.
// tan(angle) = horizontal_travel / vertical_travel, at the knob outer radius.
knurl_twist = tan(knurl_angle) * knob_h / (knob_d/2) * 180 / PI;

// =============================================================================
//  Helpers
// =============================================================================

// A rounded-corner square as a 2D profile, centred on origin.
module rounded_square(w, r) {
    hull()
        for (sx = [-1, 1], sy = [-1, 1])
            translate([sx * (w/2 - r), sy * (w/2 - r)])
                circle(r = r);
}

// Solid rounded-corner slab of given height.
module rounded_slab(w, h, r) {
    linear_extrude(height = h)
        rounded_square(w, r);
}

// =============================================================================
//  Parts
// =============================================================================

// The CNC base: rounded square block with chamfered top & bottom edges,
// minus the rear USB-C cutout.
module base() {
    difference() {
        union() {
            // bottom chamfer
            translate([0, 0, 0])
                hull() {
                    rounded_slab(base_w - 2*bottom_chamfer, eps,
                                 max(0.1, corner_r - bottom_chamfer));
                    translate([0, 0, bottom_chamfer])
                        rounded_slab(base_w, eps, corner_r);
                }
            // main body
            translate([0, 0, bottom_chamfer])
                rounded_slab(base_w, base_h - bottom_chamfer - top_chamfer,
                             corner_r);
            // top chamfer
            translate([0, 0, base_h - top_chamfer])
                hull() {
                    rounded_slab(base_w, eps, corner_r);
                    translate([0, 0, top_chamfer])
                        rounded_slab(base_w - 2*top_chamfer, eps,
                                     max(0.1, corner_r - top_chamfer));
                }
        }
        // rear USB-C port (cut from the +Y face)
        translate([0, base_w/2 + eps, usbc_z])
            rotate([90, 0, 0])
                linear_extrude(height = 6, center = true)
                    offset(r = usbc_r)
                        square([usbc_w - 2*usbc_r, usbc_h - 2*usbc_r],
                               center = true);
    }
}

// The glowing RGB seam that sits between base and knob.
module light_ring() {
    cylinder(h = light_gap, d = knob_d + 2*light_over);
}

// 2D cross-section of the knob: a circle with the knurl flutes cut into its rim.
module knob_profile() {
    difference() {
        circle(d = knob_d);
        for (i = [0 : knurl_count - 1])
            rotate([0, 0, i * 360 / knurl_count])
                translate([knob_d/2 - knurl_depth + knurl_tool_d/2, 0])
                    circle(d = knurl_tool_d);
    }
}

// The knob body: a twisted extrude of the fluted profile gives the angled
// (helical) knurling; the top is left flat and clean.
module knurled_knob() {
    linear_extrude(height = knob_h, twist = knurl_twist,
                   slices = knurl_slices, convexity = 10)
        knob_profile();
}

// =============================================================================
//  Assembly
// =============================================================================

module vk01() {
    color(col_base) base();
    color(col_led)  translate([0, 0, led_z])  light_ring();
    color(col_knob) translate([0, 0, knob_z]) knurled_knob();
}

// -------- Render selector ----------------------------------------------------
// `part` defaults to "all", so opening this file in OpenSCAD shows the whole
// assembly as before. The viewer's build step overrides it (e.g.
// `openscad -D part="knob"`) to export each part as its own positioned mesh,
// which keeps the base / light seam / knob as separate, individually animatable
// meshes in the app.
part = "all";

if      (part == "all")  vk01();
else if (part == "base") base();
else if (part == "seam") translate([0, 0, led_z])  light_ring();
else if (part == "knob") translate([0, 0, knob_z]) knurled_knob();
