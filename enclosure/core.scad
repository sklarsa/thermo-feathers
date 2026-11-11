// The common core: holds the Feather, the battery and the SHT41, and carries the standard snap
// interface every shell clicks onto.
//
// Open at the front. The Feather sits on two standoffs with pegs and a pad, its bottom edge tucked
// under two fixed ledges and its top edge clicked under two hooks. The battery rests against the
// hooks and a backstop; the SHT41 slides back under two corner ledges. The cover's ribs keep both
// from sliding out. Prints back face down, no supports.
//
// Frame: x across (USB on the -x side), y depth from the back face (y = 0, towards the wall) to the
// front, z up from the shelf. The node is a compact upright block with rounded corners:
//
//   feet .. pocket_top       sensor pocket: the SHT41 lies flat, vented front, sides and floor
//   pocket_top .. elec_z0    thermal break: open to the room on three sides, joined to the pocket
//   elec_z0 .. core_H        Feather against the back, battery in front of it
//
// Self-heating is about 1 mW on average with ESP-NOW (~0.01 C), so the break is a margin, not a
// necessity. Readings taken while charging run warm in any layout: drop them (CHARGING flag).
//
// Heat rises, so the sensor sits below everything that makes heat. The Feather's components face
// the back wall: reset (pin hole) and the CHG LED are on the back face; USB-C is out the -x side.

include <knobs.scad>

// ---- Board data, from Adafruit's Eagle files (Adafruit-Feather-ESP32-S3-PCB, shared by 5323,
// 5477 and 5885; Adafruit-SHT40-PCB for 5776). Board coordinates, origin at the lower-left
// corner seen from the component side. ----
fw = 50.8; fh = 22.86; pcb = 1.6;
f_holes = [[2.54, 2.54], [2.54, 20.32], [48.26, 1.8415], [48.26, 20.955]]; // 2.5 and 2.2 mm drills
f_reset = [10.795, 5.1435];   // SW2, KMR2 4.2 x 2.8 mm
f_boot = [20.955, 5.207];     // SW1
f_chg = [1.4605, 5.461];      // orange CHG LED, 0603
f_usb = [2.667, 11.43];       // USB-C receptacle centre; its face is 1.14 mm past the board's end
f_jst = [10.922, 19.3675];    // battery JST-PH, plug enters from the top edge (y = fh)
f_qt = [22.352, 11.43];       // STEMMA QT, plug enters from the USB side
f_module = [32.82, 1.3, 15.6, 20.3]; // ESP32-S3-MINI-1: x, y, w, h. Antenna at the far end

sw = 25.4; sh = 17.78;        // SHT41 breakout
s_sensor = [12.7, 8.89];
batt = [62.5, 35.5, 5.2];     // 258, nominal 62 x 34 x 5; one distributor lists 62.5 x 35.5 x 5.2

// ---- Core layout: as small as the parts allow ----
corner_r = 6;                 // front-view corner radius of the core
side_wall = 1.6;              // the cover's sleeve is the outer skin, so the core's sides can be thin
qt_gap = 7;                   // back wall to board top: room for the STEMMA QT plug to turn
board_y = wall + qt_gap;      // the board's component face
batt_y = board_y + pcb + 1.6; // room for the board's hook lips and ledges between board and battery
core_D = ceil(max(batt_y + batt[2] + tol * 2 + wall, sh + 2 * tol + 2 * wall));  // the SHT41 lies across the depth
// extra room each side so the battery's square corners clear the rounded inside corners
corner_room = (corner_r - side_wall) * (1 - 1 / sqrt(2));
core_W = ceil(batt[0] + 2 * (tol + corner_room) + 2 * side_wall + lead_room);
batt_x = -core_W / 2 + side_wall + corner_room + tol + lead_room;  // the battery's -x end
feet = 1.5;                   // the pocket floor sits this far off the shelf
floor_t = 1.4;
pocket_top = 11;              // SHT41 flat on rails, its plug, and air above
gap_h = 3;
elec_z0 = pocket_top + gap_h; // bottom of the floor under the battery
bay_floor = 1.6;
batt_z = elec_z0 + bay_floor + tol;
board_z = elec_z0 + bay_floor + 1.5;
core_H = ceil(batt_z + batt[1] + tol + corner_room + wall);
board_x = -core_W / 2 + side_wall + 1.14 + 0.3;   // USB face just inside the -x wall
sht_z = feet + floor_t + 1.2; // on 1.2 mm rails

function fpos(p) = [board_x + p[0], board_y, board_z + p[1]]; // a point on the Feather's top face
usb_c = fpos(f_usb);
reset_c = fpos(f_reset);
chg_c = fpos(f_chg);

echo(str("core W x D x H = ", core_W, " x ", core_D, " x ", core_H));

module xz(y0, h) translate([0, y0 + h, 0]) rotate([90, 0, 0]) linear_extrude(h) children();
module yz(x0, h) translate([x0, 0, 0]) rotate([90, 0, 90]) linear_extrude(h) children();

// A slot along y with 45-degree pointed ends, so it prints in a wall without bridging.
module yslot(len, w) hull() { translate([0, -len / 2 + w / 2]) rotate(45) square(w / sqrt(2), center = true);
                              translate([0, len / 2 - w / 2]) rotate(45) square(w / sqrt(2), center = true); }

// Front-view outline of the core, x across and z up.
module core_outline() translate([-core_W / 2, 0]) offset(r = corner_r, $fn = 48) offset(delta = -corner_r) square([core_W, core_H]);
module inner_outline() translate([-core_W / 2 + side_wall, 0]) offset(r = corner_r - side_wall, $fn = 48) offset(delta = -(corner_r - side_wall)) square([core_W - 2 * side_wall, core_H - wall]);

// The USB-C plug's overmould needs about 12 x 7 mm. Shells cut the same opening.
usb_open = [8, 13];           // (depth, height)
usb_face_x = board_x - 1.14;  // the receptacle's face
usb_y1 = usb_c.y - pcb / 2 + usb_open[0] / 2;   // front edge of the opening
module usb_cut(shell = false) {
  translate([-60, usb_y1 - usb_open[0], usb_c.z - usb_open[1] / 2]) cube([60 + usb_face_x + 0.5, usb_open[0], usb_open[1]]);
  // in a shell the opening runs out to the back edge, so it never bridges
  if (shell) translate([-60, -5, usb_c.z - usb_open[1] / 2]) cube([60 + usb_face_x, usb_y1 + 5, usb_open[1]]);
}

// ---- Holders. Everything goes in from the open front; the cover's ribs keep the battery and the
// SHT41 from sliding back out. ----
qt_c = fpos(f_qt);
board_front = board_y + pcb;
hook_t = 1.6;                 // post thickness, the way it bends
hook_len = board_front + 1.1 - wall;   // free length, back wall to the lip's top
board_top = board_z + fh;
top_hooks = [26, 40];         // board x of each top-edge hook, clear of the JST plug at 7-15
ledges = [22, 38];            // board x of each fixed bottom-edge ledge, clear of the cable's slot
echo(str("board hook strain ", round(strain(hook_t, hook_lip, hook_len) * 1000) / 10, " %"));

// A hook over the board's top edge, flexing in z: a post from the back wall, a lip at its free end
// that reaches hook_lip down over the board's front face, chamfered so the board pushes past.
module top_hook(bx, w = 5) {
  z0 = board_top + tol;
  translate([board_x + bx - w / 2, 0, 0]) rotate([90, 0, 90]) linear_extrude(w)
    polygon([[wall - 0.5, z0], [board_front + board_play, z0], [board_front + board_play, z0 - hook_lip - tol],
             [board_front + 1.1, z0], [board_front + 1.1, z0 + hook_t], [wall - 0.5, z0 + hook_t + 1.2]]);
}

module holders() {
  // standoffs and pegs at the two mounting holes by the USB end; the far holes sit under the module
  for (h = [f_holes[0], f_holes[1]]) let(p = fpos(h)) {
    translate([p.x, wall - 0.5, p.z]) rotate([-90, 0, 0]) cylinder(d = 3.8, h = board_y - wall + 0.5 - 0.01, $fn = 24);
    translate([p.x, board_y - 0.01, p.z]) rotate([-90, 0, 0]) cylinder(d1 = peg_d, d2 = peg_d - 0.6, h = pcb, $fn = 16);
  }
  // pad behind the antenna end, past the module
  translate([board_x + 49, wall - 0.5, board_z + 0.5]) cube([1.8, board_y - wall + 0.5 - 0.01, fh - 1]);
  // fixed ledges over the bottom edge: tuck it under them, then press the top past the hooks
  // Each sits on a shelf under the board's edge that rises from the back wall, so it prints
  // (back face down) without hanging off the floor.
  for (bx = ledges) translate([board_x + bx - 3, 0, elec_z0 + bay_floor - 0.01]) {
    translate([0, wall - 0.5, 0]) cube([6, board_front + board_play - wall + 0.5, board_z - tol - elec_z0 - bay_floor + 0.01]);
    translate([0, board_front + board_play, 0]) cube([6, 1.2, board_z - elec_z0 - bay_floor + ledge_lip + 0.01]);
  }
  for (bx = top_hooks) top_hook(bx);
  // battery backstop where the board isn't
  translate([board_x + fw + 1.5, wall - 0.5, elec_z0 + bay_floor - 0.01]) cube([1.6, batt_y - wall + 0.5, batt[1] - 6]);
  // SHT41: two rails under it, and ledges over its back corners that it slides under
  for (s = [-1, 1]) {
    translate([s * 8 - 1, wall - 0.5, feet + floor_t - 0.01]) cube([2, (core_D + sh) / 2 - wall + 0.5, sht_z - feet - floor_t + 0.01]);
    translate([s > 0 ? sw / 2 - 3 : -sw / 2, wall - 0.5, sht_z + pcb + 0.2]) cube([3, (core_D - sh) / 2 + 1 - wall + 0.5 + tol, 1.2]);
  }
}

module core() {
  holders();
  difference() {
    xz(0, core_D) core_outline();
    // electronics bay and sensor pocket, both open at the front: the cover closes them
    xz(wall, core_D) intersection() { inner_outline(); translate([-60, elec_z0 + bay_floor]) square([120, 100]); }
    translate([-core_W / 2 + side_wall, wall, feet + floor_t]) cube([core_W - 2 * side_wall, core_D, pocket_top - feet - floor_t + 0.01]);
    // thermal break: open to the room at the sides and front, solid only along the back
    translate([-60, wall + 3, pocket_top]) cube([120, core_D, gap_h]);
    // the STEMMA QT cable's way down to the pocket, wide enough for its plug
    translate([qt_c.x - 12, wall + 0.4, pocket_top - 1]) cube([7, 4, gap_h + bay_floor + 2]);
    for (s = [-1, 1]) for (z = [feet + floor_t + 2.2, feet + floor_t + 5.8]) translate([s > 0 ? 15 : -60, core_D / 2 + 1, z]) rotate([0, 90, 0]) linear_extrude(45) yslot(core_D - 7, 2.4);
    for (x = [-20 : 8 : 20]) translate([x - 1.5, wall + 2, -1]) cube([3, core_D - 2 * wall - 4, feet + floor_t + 2]);
    translate([-core_W / 2 + 6, -1, -1]) cube([core_W - 12, core_D + 2, feet + 1]);   // feet: a rail at each end
    usb_cut();
    // reset: a paperclip goes straight in to the button; CHG LED: a 2.5 mm window
    translate([reset_c.x, -1, reset_c.z]) rotate([-90, 0, 0]) cylinder(d = 1.8, h = wall + 2, $fn = 16);
    translate([chg_c.x, -1, chg_c.z]) rotate([-90, 0, 0]) cylinder(d = 2.5, h = wall + 2, $fn = 16);
  }
}

// ---- Hardware, as bounding solids from the board files ----
module feather() {
  color("#1b1b1b") translate([board_x, board_y, board_z]) rotate([90, 0, 0]) mirror([0, 0, 1]) linear_extrude(pcb)
    difference() { offset(r = 2.54, $fn = 24) offset(delta = -2.54) square([fw, fh]); for (h = f_holes) translate(h) circle(d = 2.2, $fn = 16); }
  translate([board_x, board_y, board_z]) {
    color("silver") translate([f_module[0], -2.4, f_module[1]]) cube([f_module[2], 2.4, f_module[3]]);        // S3 module
    color("silver") translate([f_usb[0] - 3.81, -3.26, f_usb[1] - 4.47]) cube([7.35, 3.26, 8.94]);         // USB-C
    color("ivory") translate([f_jst[0] - 4, -6, f_jst[1] - 4.5]) cube([8, 6, 7.5]);                         // JST-PH
    color("ivory") translate([f_qt[0] - 1.49, -2.95, f_qt[1] - 3]) cube([3.18, 2.95, 6.1]);                  // STEMMA QT
    for (b = [f_reset, f_boot]) color("gainsboro") translate([b[0] - 2.1, -1.9, b[1] - 1.4]) cube([4.2, 1.9, 2.8]);
    color("orange") translate([f_chg[0] - 0.4, -0.6, f_chg[1] - 0.8]) cube([0.8, 0.6, 1.6]);
  }
}
module battery() color("#c9c9d4") translate([batt_x, batt_y + tol, batt_z]) cube([batt[0], batt[2], batt[1]]);
module sht41() translate([-sw / 2, (core_D - sh) / 2, sht_z]) {
  color("#1b1b1b") cube([sw, sh, pcb]);
  color("ivory") for (x = [0, sw - 5]) translate([x, sh / 2 - 3, pcb]) cube([5, 6, 3]);
  color("white") translate([s_sensor[0] - 0.75, s_sensor[1] - 0.75, pcb]) cube([1.5, 1.5, 0.5]);
}
// The STEMMA QT cable, roughly: plug, up off the board, down the back to the pocket.
module qt_cable() color("#3060c0") let(p = fpos(f_qt), pts = [[p.x - 4, p.y - 1.5, p.z], [p.x - 10, wall + 1.5, p.z],
    [p.x - 10, wall + 1.5, elec_z0 + 1], [-sw / 2 - 2, wall + 1.5, sht_z + 3], [-sw / 2 + 2, (core_D) / 2, sht_z + 3.5]])
  for (i = [0 : len(pts) - 2]) hull() for (q = [pts[i], pts[i + 1]]) translate(q) sphere(d = 1.6, $fn = 8);

module hardware() { feather(); battery(); sht41(); qt_cable(); }
