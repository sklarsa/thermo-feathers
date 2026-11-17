// Shared knobs for the core and every shell. Hardware never matches the datasheet: change a knob,
// reprint the matching test_* part, repeat.
//
// Material is PLA, not PETG as in esp-light-show. PLA is stiffer and more brittle and creeps less,
// so snap arms are longer and lips smaller, keeping bending strain under about 2 %. PLA softens
// around 55 °C: keep nodes out of sunny windows.
//
// Print the test_* parts (main.scad) before a whole core: each is cut from the same geometry.

// ---- Fits ----
tol = 0.3;          // gap around a board or the battery, from light-show
shell_clear = 0.3;  // air between the core's outside and a shell's inside

// ---- Feather: as the s3-sensors box, M2.5 self-tapping screws into printed standoffs ----
qt_gap = 7;         // back wall to the board's component face: its tallest part, the JST-PH, is 6 mm
standoff_d = 4.4;   // under the screws: inside the hole pads, clear of the CHG LED
pin_post_d = 3.2;   // under the pins: clear of the ESP32-S3 module beside them
m25_pilot = 2.2;    // M2.5 self-tapping pilot; 2.0 if the screw spins, 2.4 if it won't go in
pin_d = 1.9;        // locating pins in its 2.2 mm holes
jst_room = 10;      // above the board, for the battery plug and the bend in its wires

// ---- Wall hanging: a keyhole for a pan-head screw ----
key_head = 7.5;     // the head goes through here
key_shank = 3.8;    // and slides up on its shank

// ---- SHT41 ----
sht_back = 0.5;     // air between its back edge and the back wall
sht_play = 0.3;     // between it and its lips; less if it rattles, more if it won't slide in
sht_lip = 0.8;      // how far the lips reach over its back corners

// ---- Shell snap interface: tabs in the sleeve's side walls click over the core's back edge ----
snap_lip = 0.5;     // how far a tab's lip reaches over the core's back edge
snap_arm = 14;      // free length of each tab, back edge to root. Strain ~ 1.5·t·lip / arm²
snap_tab_w = 10;    // width of each tab
slit = 0.8;         // the slits that free a tab, as light-show's LED clips

// ---- Colour (AMS): shells print face down, so face artwork is inlaid in the first layers ----
inlay_t = 0.6;      // inlay depth: 3 layers at 0.2 mm, each colour swap happens only there

// ---- Walls ----
wall = 2.0;         // core walls
shell_t = 1.6;      // shell sleeve and rim walls
front_t = 1.8;      // shell front plate; it prints on the bed
edge_ch = 0.8;      // chamfer round the cover's front edge

function strain(t, lip, arm) = 1.5 * t * lip / (arm * arm);
echo(str("shell tab strain ", round(strain(shell_t, snap_lip, snap_arm) * 1000) / 10, " %"));
