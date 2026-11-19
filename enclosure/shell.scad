// What every shell (cover) shares: a sleeve that hugs the core on the top, sides and front,
// with the standard screw post, USB opening and side vents. A design supplies only flat 2D artwork:
//
//   shell_build(part, palette) {
//     silhouette;        // front outline; the sleeve is added to it, so it can't be too small
//     holes;             // through the front: vents and cut-outs (must open over the sensor pocket)
//     colour 2 region;   // inlays, each painted over the ones before it
//     colour 3 region;
//     colour 4 region;   // pass union() {} for an unused colour
//   }
//
// Shells print FRONT DOWN on the bed. The front plate is the first layers and every other wall
// rises straight from it, so any outline prints without supports. The inlays are the first
// inlay_t of the face: an AMS swaps filament only in those few layers.
//
// Artwork is drawn as seen from the front: x to the right, z up. (In the model, +x is the node's
// left as you face it, so shell_build mirrors the artwork.)
//
// A shell never touches the boards: its inside clears the core by shell_clear everywhere.

include <core.scad>

shell_front_y = core_D + shell_clear;              // inside face of the front plate
shell_D = shell_front_y + front_t;                 // the outside face, which prints on the bed

module core_clear() xz(-10, shell_front_y + 10) offset(delta = shell_clear) core_outline();
module sleeve_outline() offset(delta = shell_clear + shell_t) core_outline();

// Side vents over the sensor pocket and the thermal break, matching the core's.
module side_vents() for (s = [-1, 1]) {
  for (z = [feet + floor_t + 2.2, feet + floor_t + 5.8]) translate([s > 0 ? 15 : -75, core_D / 2 + 1, z]) rotate([0, 90, 0]) linear_extrude(60) yslot(core_D - 7, 2.4);
  translate([s > 0 ? 15 : -75, wall + 3, pocket_top + 0.5]) cube([60, core_D - wall - 5, gap_h - 1]);
}

module art() mirror([1, 0]) children();

module shell_body(rim) {
  difference() {
    union() {
      xz(shell_front_y, front_t) union() { art() children(0); sleeve_outline(); }             // front plate
      // the silhouette's own edge wall, rim deep (the sleeve is full depth anyway)
      xz(shell_front_y - rim, rim) difference() { union() { art() children(0); sleeve_outline(); } offset(delta = -shell_t) union() { art() children(0); sleeve_outline(); } }
      xz(0, shell_front_y) sleeve_outline();                                           // sleeve, flush with the core's back
    }
    core_clear();
    xz(shell_front_y - 1, front_t + 2) art() children(1);                                     // holes
    side_vents();
    usb_cut(shell = true);
    translate([-200, -50, -50]) cube([400, 100, 50]);                                    // flat on the shelf
    // front edge chamfer, one 0.2 mm layer per step: it is the first layers on the bed
    for (i = [0 : round(edge_ch / 0.2) - 1]) xz(shell_D - (i + 1) * 0.2, 0.2 + 0.01) difference() {
      offset(delta = 20) art() union() { children(0); sleeve_outline(); }
      offset(delta = -(edge_ch - i * 0.2)) art() union() { children(0); sleeve_outline(); } }
  }
  batt_ribs();
  screw_post();
}

// The post the screw through the core's back bites into: all that holds the shell on.
module screw_post() let(y0 = wall + boss_t + 0.2) translate([screw_c.x, y0, screw_c.y]) rotate([-90, 0, 0])
  difference() { cylinder(d = 6, h = shell_front_y - y0 + 0.01, $fn = 32); translate([0, 0, -1]) cylinder(d = m25_pilot, h = 8, $fn = 24); }

// Ribs on the front plate's inside, over the battery's fins: they press it batt_press onto them.
module batt_ribs() for (x = [batt_x + 10, batt_x + batt[1] - 6])
  translate([x - 1, batt_y + batt[2] - batt_press, batt_z + 8]) cube([2, shell_front_y - (batt_y + batt[2]) + batt_press + 0.01, batt[0] - 16]);

// rim: depth of the outline's own edge wall, for parts that stick out past the sleeve
module shell_build(part = "all", palette = ["#999999", "#dddddd", "#333333", "#cc3333"], rim = 4) {
  inlay_y = shell_D - inlay_t;
  if (part == "all") {
    for (k = [1 : 4]) color(palette[k - 1]) shell_build(str("c", k), palette, rim) { children(0); children(1); children(2); children(3); children(4); }
  } else if (part == "c1") {
    difference() {
      shell_body(rim) { children(0); children(1); }
      xz(inlay_y, inlay_t + 1) union() { art() children(2); art() children(3); art() children(4); }
    }
  } else {
    k = part == "c2" ? 2 : part == "c3" ? 3 : 4;
    xz(inlay_y, inlay_t) difference() {
      intersection() { art() children(k); union() { art() children(0); sleeve_outline(); } }
      art() children(1);
      if (k < 3) art() children(3);
      if (k < 4) art() children(4);
    }
  }
}

// Print orientation: front plate on the bed.
module shell_print() translate([0, 0, shell_D]) rotate([-90, 0, 0]) children();
