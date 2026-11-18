// The common core: holds the Feather, the battery and the SHT41, and carries the standard snap
// interface every shell clicks onto. Open at the front: everything goes in from there, and the
// shell's front plate closes it.
//
// Frame: x across (USB on the -x side), y depth from the back face (y = 0, towards the wall) to the
// front, z up from the shelf. Prints BACK DOWN, so every wall and fin rises straight off the bed:
//
//   feet .. pocket_top       sensor pocket: the SHT41 lies flat on rails, front, sides and floor open
//   pocket_top .. elec_z0    thermal break: open to the room on three sides, joined at the back
//   elec_z0 .. core_H        the bay: the Feather high on the back wall, components facing it, on
//                            standoffs; the battery stands upright in front of its USB half
//
// The battery's foil is kept off the ESP32-S3 module's antenna, at the board's far (+x) end: in the
// first concept the battery lay across the whole board, between the antenna and the room.
//
// Heat rises, so the sensor sits below everything that makes heat. Self-heating is about 1 mW on
// average with ESP-NOW (~0.01 C). Readings taken while charging run warm in any layout; the
// firmware flags them CHARGING.
//
// Fixings, as the s3-sensors box (homelab esp32/enclosure.scad): two M2.5 self-tapping screws
// through the Feather's plated holes into printed standoffs; pins locate its other end. Nothing
// else needs a tool: the battery and the SHT41 are held by the core and the shell's front plate.

include <knobs.scad>

// ---- Board data, from Adafruit's Eagle files (Adafruit-Feather-ESP32-S3-PCB, shared by 5323,
// 5477 and 5885; Adafruit-SHT40-PCB for 5776). Board coordinates, origin at the lower-left
// corner seen from the component side. ----
fw = 50.8; fh = 22.86; pcb = 1.6;
f_screws = [[2.54, 2.54], [2.54, 20.32]];          // 2.5 mm plated: the screws
f_pins = [[48.26, 1.8415], [48.26, 20.955]];        // 2.2 mm unplated: locating pins
f_reset = [10.795, 5.1435];   // SW2, KMR2 4.2 x 2.8 mm
f_boot = [20.955, 5.207];     // SW1
f_chg = [1.4605, 5.461];      // orange CHG LED, 0603
f_usb = [2.667, 11.43];       // USB-C receptacle centre; its face is 1.14 mm past the board's end
f_jst = [10.922, 19.3675];    // battery JST-PH, plug enters from the top edge (y = fh)
f_qt = [22.352, 11.43];       // STEMMA QT, plug enters from the USB side
f_module = [30.39, 3.73, 20.5, 15.4]; // ESP32-S3-MINI-1 (U1, R270): x, y, w, h. Antenna at the far end

sw = 25.4; sh = 17.78;        // SHT41 breakout
s_sensor = [12.7, 8.89];
s_holes = [[2.54, 2.54], [22.86, 2.54], [2.54, 15.24], [22.86, 15.24]];
s_qt_y = [5.9, 11.9];         // its two STEMMA QT sockets sit on the short edges, over this y span
batt = [62.5, 35.5, 5.2];     // 258, nominal 62 x 34 x 5; one distributor lists 62.5 x 35.5 x 5.2

// ---- Core layout ----
corner_r = 6;                 // front-view corner radius of the core
side_wall = 1.6;              // the cover's sleeve is the outer skin, so the core's sides can be thin
board_y = wall + qt_gap;      // the Feather's component face
core_W = ceil(2 * side_wall + 0.3 + 1.14 + fw + 1.5);
in_x0 = -core_W / 2 + side_wall;
in_x1 = core_W / 2 - side_wall;
board_x = in_x0 + 1.14 + 0.3; // USB face just inside the -x wall
batt_x = in_x0 + 1;           // clear of the USB plug's tip
feet = 1.5;                   // the pocket floor sits this far off the shelf
floor_t = 1.4;
pocket_top = 11;              // SHT41 flat on rails, its plug, and air above
gap_h = 3;
elec_z0 = pocket_top + gap_h; // bottom of the bay's floor
bay_floor = 1.6;
bay_z = elec_z0 + bay_floor;
batt_z = bay_z + tol;
roof_z = batt_z + batt[0] + tol + 1.3;   // inside of the roof; 1.3 clears the battery's corner of the rounded one
core_H = ceil(roof_z + wall);
board_z = roof_z - jst_room - fh;  // the board's bottom edge; its JST plug goes in from above
core_D = ceil(max(wall + sht_back + sh + 2 * tol, board_y + pcb + 1.2 + batt[2] + 2 * tol));
batt_y = core_D - shell_clear - tol - batt[2];  // the shell's front plate holds it in
sht_z = feet + floor_t + 1.2; // on 1.2 mm rails
sht_y = wall + sht_back;      // the SHT41's back edge

function fpos(p) = [board_x + p[0], board_y, board_z + p[1]]; // a point on the Feather's component face
usb_c = fpos(f_usb);
antenna_x = board_x + f_module[0] + f_module[2] - 6;   // where the module's PCB antenna starts

echo(str("core W x D x H = ", core_W, " x ", core_D, " x ", core_H));
echo(str("battery clears the antenna by ", antenna_x - (batt_x + batt[1] + tol), " mm"));

module xz(y0, h) translate([0, y0 + h, 0]) rotate([90, 0, 0]) linear_extrude(h) children();

// A slot along y with 45-degree pointed ends.
module yslot(len, w) hull() { translate([0, -len / 2 + w / 2]) rotate(45) square(w / sqrt(2), center = true);
                              translate([0, len / 2 - w / 2]) rotate(45) square(w / sqrt(2), center = true); }

// Front-view outline of the core, x across and z up.
module core_outline() translate([-core_W / 2, 0]) offset(r = corner_r, $fn = 48) offset(delta = -corner_r) square([core_W, core_H]);
module inner_outline() translate([-core_W / 2 + side_wall, 0]) offset(r = corner_r - side_wall, $fn = 48) offset(delta = -(corner_r - side_wall)) square([core_W - 2 * side_wall, core_H - wall]);

// The USB-C plug's overmould needs about 12 x 7 mm. Shells cut the same opening. It's the back
// wall's edge too: the CHG LED, next to the socket, shows through it.
usb_open = [8, 13];           // (depth, height)
usb_face_x = board_x - 1.14;  // the receptacle's face
usb_y1 = usb_c.y - pcb / 2 + usb_open[0] / 2;   // front edge of the opening
module usb_cut(shell = false) {
  translate([-60, usb_y1 - usb_open[0], usb_c.z - usb_open[1] / 2]) cube([60 + usb_face_x + 0.5, usb_open[0], usb_open[1]]);
  // in a shell the opening runs out to the back edge, so it never bridges
  if (shell) translate([-60, -5, usb_c.z - usb_open[1] / 2]) cube([60 + usb_face_x, usb_y1 + 5, usb_open[1]]);
}

// Holes in the back, straight onto RESET and BOOT, for a toothpick or a SIM tool. BOOT is held while
// tapping RESET to flash a board over USB; both are needed only for that and for a node that won't
// wake. Each is funnelled on the outside, and a tube runs it to just short of the button, so the
// STEMMA QT cable can't lie across it.
btn_top = qt_gap - 1.9;       // the KMR2 stands 1.9 off the board
module button_holes() for (b = [f_reset, f_boot]) let(p = fpos(b)) translate([p.x, -1, p.z]) rotate([-90, 0, 0]) {
  cylinder(d = btn_d, h = wall + btn_top + 2, $fn = 24);
  translate([0, 0, 1 - 0.01]) cylinder(d1 = btn_d + 3, d2 = btn_d, h = 1.5, $fn = 24);
}
module button_tubes() for (b = [f_reset, f_boot]) let(p = fpos(b))
  translate([p.x, wall - 0.5, p.z]) rotate([-90, 0, 0]) cylinder(d = btn_d + 2, h = btn_top, $fn = 24);   // ends 0.5 off it

// To hang it on a wall: a keyhole for a pan-head screw, its head into the space behind the battery,
// between the fins and clear of the cable's run.
key_c = [batt_x + 15, (bay_z + board_z) / 2 - 3];
module keyhole() translate([key_c.x, -1, key_c.y]) rotate([-90, 0, 0]) linear_extrude(wall + 2) {
  circle(d = key_head, $fn = 32);
  translate([-key_shank / 2, -6]) square([key_shank, 6]);   // the slot runs up: it hangs from its top
  translate([0, -6]) circle(d = key_shank, $fn = 24);
}

// The device ID, cut into the back: it prints on the bed, so it comes out crisp. Reads from behind,
// below the keyhole.
module id_label() if (id != "") translate([0, -0.01, bay_z + 2.5]) rotate([90, 0, 0]) mirror([0, 0, 1])
  linear_extrude(id_depth + 0.01) text(id, size = id_size, font = "Liberation Mono:style=Bold", halign = "center", valign = "center");

// Feather: standoffs from the back wall to its component face. Screws at the USB end, pins at the
// antenna end, where the holes are unplated and smaller.
module root(d) cylinder(d1 = d + root_flare, d2 = d, h = 3.5, $fn = 32);   // from 0.5 inside the wall
module feather_mounts() {
  for (h = f_screws) let(p = fpos(h)) translate([p.x, wall - 0.5, p.z]) rotate([-90, 0, 0])
    difference() { union() { cylinder(d = standoff_d, h = qt_gap + 0.5, $fn = 32); root(standoff_d); } translate([0, 0, 1]) cylinder(d = m25_pilot, h = qt_gap, $fn = 24); }
  for (h = f_pins) let(p = fpos(h)) translate([p.x, wall - 0.5, p.z]) rotate([-90, 0, 0]) {
    cylinder(d = pin_post_d, h = qt_gap + 0.5, $fn = 32);
    root(pin_post_d);
    cylinder(d = pin_d, h = qt_gap + 0.5 + pcb + 0.8, $fn = 24);
    translate([0, 0, qt_gap + 0.5 + pcb + 0.8]) cylinder(d1 = pin_d, d2 = pin_d - 0.8, h = 0.4, $fn = 24);
  }
}

// Battery: fins from the back wall below the board hold it off the board and the cable run; its
// left edge is the -x wall, a fin stops its right edge, and the roof and bay floor its ends.
module battery_fins() {
  // the left one clear of the cable slot and the keyhole
  for (x = [batt_x + 10, batt_x + batt[1] - 6]) translate([x - 0.8, wall - 0.5, bay_z - 0.01]) cube([1.6, batt_y - wall + 0.5, board_z - 2 - bay_z]);
  translate([batt_x + batt[1] + 2 * tol, wall - 0.5, bay_z - 0.01]) cube([1.6, core_D - wall + 0.5, board_z - 2 - bay_z]);
}

// "BATTERY" cut into the bay floor where it stands, read from the front.
module batt_label() translate([batt_x + batt[1] / 2, batt_y + batt[2] / 2, bay_z - 0.4]) linear_extrude(0.41)
  text("BATTERY", size = 3.2, font = "Liberation Sans:style=Bold", halign = "center", valign = "center");

// SHT41: slides in from the front on two rails under its short edges, under lips at its two back
// corners, to the back wall. The front lips are left out: its STEMMA QT sockets sit right at those
// edges and would hit them on the way in. The shell's front plate stops it sliding out.
module sht_holder() for (s = [-1, 1]) {
  e = sw / 2 + sht_side;                                     // inside face of the rails
  translate([s > 0 ? e - 1.2 : -e - 1.6, sht_y, feet]) cube([2.8, core_D - sht_y, sht_z - feet]);   // rail, 1.2 under the board
  translate([s > 0 ? e : -e - 1.6, sht_y, feet]) cube([1.6, s_qt_y[0] - 0.8, sht_z + pcb + sht_play + 1.2 - feet]);   // back post
  translate([s > 0 ? e - sht_lip : -e - 1.6, sht_y, sht_z + pcb + sht_play]) cube([1.6 + sht_lip, s_qt_y[0] - 0.8, 1.2]);  // lip
}

module core() {
  difference() {
    union() {
      difference() {
        xz(0, core_D) core_outline();
        // bay and pocket, open at the front
        xz(wall, core_D) intersection() { inner_outline(); translate([-60, bay_z]) square([120, 100]); }
        translate([in_x0, wall, feet + floor_t]) cube([in_x1 - in_x0, core_D, pocket_top - feet - floor_t + 0.01]);
        // thermal break: open to the room at the sides and front, solid only along the back
        translate([-60, wall + 3, pocket_top]) cube([120, core_D, gap_h]);
        // the pocket's sides and floor, vented
        for (s = [-1, 1]) for (z = [feet + floor_t + 2.2, feet + floor_t + 5.8]) translate([s > 0 ? 15 : -60, core_D / 2 + 1, z]) rotate([0, 90, 0]) linear_extrude(45) yslot(core_D - 7, 2.4);
        for (x = [-17 : 6 : 17]) translate([x - 1.5, wall + 2, -1]) cube([3, core_D - wall - 4, feet + floor_t + 2]);
        translate([-core_W / 2 + 6, -1, -1]) cube([core_W - 12, core_D + 2, feet + 1]);   // feet: a rail at each end
        // the STEMMA QT cable: down behind the battery, through the bay floor at the back
        translate([in_x0 + 2, wall - 0.01, pocket_top - 1]) cube([7, 5, bay_z - pocket_top + 2]);   // a JST-SH plug is 6 x 4.3
        batt_label();
        usb_cut();
        button_holes();
        keyhole();
        id_label();
      }
      feather_mounts();
      button_tubes();
      battery_fins();
      sht_holder();
    }
    usb_cut();
    button_holes();
  }
}

// ---- Hardware, as bounding solids from the board files ----
module feather() {
  color("#1b1b1b") translate([board_x, board_y, board_z]) rotate([90, 0, 0]) mirror([0, 0, 1]) linear_extrude(pcb)
    difference() {
      offset(r = 2.54, $fn = 24) offset(delta = -2.54) square([fw, fh]);
      for (h = f_screws) translate(h) circle(d = 2.5, $fn = 16);
      for (h = f_pins) translate(h) circle(d = 2.2, $fn = 16);
    }
  translate([board_x, board_y, board_z]) {
    color("silver") translate([f_module[0], -2.4, f_module[1]]) cube([f_module[2], 2.4, f_module[3]]);        // S3 module
    color("silver") translate([f_usb[0] - 3.81, -3.26, f_usb[1] - 4.47]) cube([7.35, 3.26, 8.94]);         // USB-C
    color("ivory") translate([f_jst[0] - 4, -6, f_jst[1] - 4.5]) cube([8, 6, 7.5]);                         // JST-PH
    color("ivory") translate([f_qt[0] - 1.49, -2.95, f_qt[1] - 3]) cube([3.18, 2.95, 6.1]);                  // STEMMA QT
    for (b = [f_reset, f_boot]) color("gainsboro") translate([b[0] - 2.1, -1.9, b[1] - 1.4]) cube([4.2, 1.9, 2.8]);
    color("orange") translate([f_chg[0] - 0.4, -0.6, f_chg[1] - 0.8]) cube([0.8, 0.6, 1.6]);
  }
}
module battery() color("#c9c9d4") translate([batt_x, batt_y, batt_z]) cube([batt[1], batt[2], batt[0]]);
module sht41() translate([-sw / 2, sht_y, sht_z]) {
  color("#1b1b1b") difference() { cube([sw, sh, pcb]); for (h = s_holes) translate([h[0], h[1], -1]) cylinder(d = 2.5, h = 4, $fn = 16); }
  color("ivory") for (x = [0, sw - 4.5]) translate([x, s_qt_y[0], pcb]) cube([4.5, s_qt_y[1] - s_qt_y[0], 3]);
  color("white") translate([s_sensor[0] - 0.75, s_sensor[1] - 0.75, pcb]) cube([1.5, 1.5, 0.5]);
}
// The STEMMA QT cable, roughly: Feather socket, down behind the battery, through the bay floor,
// into the SHT41's -x socket.
module qt_cable() color("#3060c0") let(p = fpos(f_qt), q = [-sw / 2 - 3, sht_y + 8.9, sht_z + pcb + 1.5],
    pts = [[p.x - 4, p.y - 1.5, p.z], [in_x0 + 5, wall + 1.5, p.z], [in_x0 + 5, wall + 1.5, pocket_top - 2],
           [q.x - 2, wall + 2, q.z], [q.x - 2, q.y, q.z], q])
  for (i = [0 : len(pts) - 2]) hull() for (r = [pts[i], pts[i + 1]]) translate(r) sphere(d = 1.6, $fn = 8);

module hardware() { feather(); battery(); sht41(); qt_cable(); }
module boards() { feather(); battery(); sht41(); }   // what must never touch the core

// Print orientation: back face on the bed.
module core_print() rotate([90, 0, 0]) children();
