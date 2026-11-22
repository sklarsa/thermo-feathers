// The gateway's case: a Feather on USB power, on a shelf. The Feather screws to a base plate,
// component side up, and a cover drops over it, held by two screws from underneath into posts, one
// near each end. Four M2.5 x 6 screws: two for the Feather, two for the cover.
//
// Frame: x along the board (USB-C at the -x end), y across, z up from the shelf. The origin is the
// inside corner of the cover's walls. Parts are built from main.scad: gw_base (flat on the bed),
// gw_cover (top down), gw_assembly, gw_check (the Feather against both: only faces that touch).

gw_t = 1.6;            // cover walls and top
gw_r = 3;              // outside corner radius
base_t = 2;
gw_standoff = 4;       // under the board: a 6 mm screw through it stops 1 mm short of the pilot's end
gw_top_room = 1.5;     // over its tallest part, the 6 mm JST-PH
gw_post_d = 6;

gx = 0.3 + 1.14;                        // board origin: the USB face 0.3 inside the -x wall
gy = 1;
gz = base_t + gw_standoff + pcb;        // the component face
gw_posts = [for (x = [gx + 5, gx + fw - 5]) [x, gy + fh + 1 + gw_post_d / 2]];   // beside the board, one near each end
gw_post = gw_posts[0];
gw_L = gx + fw + 1.5;
gw_W = gw_post.y + gw_post_d / 2 + 1;
gw_H = gz + 6 + gw_top_room + gw_t;
echo(str("gateway case ", gw_L + 2 * gw_t, " x ", gw_W + 2 * gw_t, " x ", gw_H));

function gpos(p) = [gx + p[0], gy + p[1]];
module at(p, z) translate([p[0], p[1], z]) children();
module gw_outline(d = 0) translate([-gw_t - d, -gw_t - d]) let(s = [gw_L + 2 * (gw_t + d), gw_W + 2 * (gw_t + d)], r = gw_r + d)
  translate([r, r]) offset(r = r, $fn = 32) square([s.x - 2 * r, s.y - 2 * r]);

// core.scad's Feather, turned from the node's frame into this one
module gw_feather() translate([gx, gy, gz]) rotate([-90, 0, 0]) translate(-[board_x, board_y, board_z]) feather();

module gw_base() difference() {
  union() {
    linear_extrude(base_t) gw_outline(-gw_t - shell_clear);
    for (h = f_screws) at(gpos(h), base_t - 0.01) cylinder(d = standoff_d, h = gw_standoff + 0.01, $fn = 32);
    for (h = f_pins) at(gpos(h), base_t - 0.01) {
      cylinder(d = pin_post_d, h = gw_standoff + 0.01, $fn = 32);
      cylinder(d = pin_d, h = gw_standoff + pcb + 0.8, $fn = 24);
      translate([0, 0, gw_standoff + pcb + 0.8]) cylinder(d1 = pin_d, d2 = pin_d - 0.8, h = 0.4, $fn = 24);
    }
    for (p = gw_posts) at(p, 0) cylinder(d = 7.5, h = 4, $fn = 32);   // the cover screws' bosses
  }
  for (h = f_screws) at(gpos(h), 0.6) cylinder(d = m25_pilot, h = 10, $fn = 24);
  for (p = gw_posts) at(p, -1) {
    cylinder(d = screw_clear, h = 10, $fn = 24);
    cylinder(d = screw_head[0], h = 1 + screw_head[1], $fn = 32);   // flush, so it sits flat
  }
}

btn_z = gz + 1.9 + 0.5;   // the tubes end 0.5 over the KMR2 buttons
module gw_cover() difference() {
  union() {
    difference() {
      linear_extrude(gw_H) gw_outline();
      translate([0, 0, -1]) linear_extrude(gw_H - gw_t + 1) gw_outline(-gw_t);
    }
    for (p = gw_posts) at(p, 4.2) cylinder(d = gw_post_d, h = gw_H - gw_t - 4.2 + 0.01, $fn = 32);
    for (b = [f_reset, f_boot]) at(gpos(b), btn_z) cylinder(d = btn_d + 2, h = gw_H - btn_z - 0.01, $fn = 24);
  }
  for (p = gw_posts) at(p, 3) cylinder(d = m25_pilot, h = 8, $fn = 24);
  for (b = [f_reset, f_boot]) at(gpos(b), btn_z - 1) {
    cylinder(d = btn_d, h = 30, $fn = 24);
    translate([0, 0, gw_H - btn_z - 0.5]) cylinder(d1 = btn_d, d2 = btn_d + 3, h = 1.5 + 0.01, $fn = 24);   // funnel
  }
  // USB-C plug, as a node's opening: 13 wide, 8 from under the board to over the socket
  translate([-gw_t - 1, gy + f_usb[1] - usb_open[1] / 2, gz - 3.2]) cube([gw_t + 2, usb_open[1], usb_open[0]]);
  // air in low along both long sides, out through the top over the S3 module
  for (y = [-gw_t - 1, gw_W - 1], x = [8 : 5 : gw_L - 8]) translate([x, y, base_t + 0.6]) cube([2, gw_t + 2, 3]);
  for (x = [gx + f_module[0] : 4 : gx + fw - 2]) translate([x, gy + f_module[1], gw_H - gw_t - 1]) cube([1.8, f_module[3], gw_t + 2]);
}

module gw_print_cover() translate([0, 0, gw_H]) rotate([180, 0, 0]) gw_cover();
