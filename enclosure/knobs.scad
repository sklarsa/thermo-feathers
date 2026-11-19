// Shared knobs for the core and every shell. Hardware never matches the datasheet: change a knob,
// reprint the matching test_* part, repeat.
//
// Material is PLA, not PETG as in esp-light-show: stiffer, more brittle, creeps less. It softens
// around 55 °C: keep nodes out of sunny windows.
//
// Print the test_* parts (main.scad) before a whole core: each is cut from the same geometry.
// A screw holds the shell on. Snap tabs were tried and shook off: their catch faces were the core's
// first layer and an overhang on the shell, and both printed rounded.

// ---- Fits ----
tol = 0.3;          // gap around a board or the battery, from light-show
shell_clear = 0.3;  // air between the core's outside and a shell's inside

// ---- Feather: as the s3-sensors box, M2.5 self-tapping screws into printed standoffs ----
qt_gap = 7;         // back wall to the board's component face: its tallest part, the JST-PH, is 6 mm
standoff_d = 4.4;   // under the screws: inside the hole pads, clear of the CHG LED
root_flare = 3;     // extra diameter at a post's root, tapering to nothing 3 mm up: one broke off mid-print
pin_post_d = 3.2;   // under the pins: clear of the ESP32-S3 module beside them
m25_pilot = 2.2;    // M2.5 self-tapping pilot; 2.0 if the screw spins, 2.4 if it won't go in
pin_d = 1.9;        // locating pins in its 2.2 mm holes
btn_d = 3.2;        // RESET and BOOT holes: a toothpick or a SIM tool
jst_room = 10;      // above the board, for the battery plug and the bend in its wires

// ---- Wall hanging: a keyhole for a pan-head screw ----
key_head = 7.5;     // the head goes through here
key_shank = 3.8;    // and slides up on its shank

// ---- Device ID on the back ----
id_size = 6;        // cap height, mm
id_depth = 0.6;     // three layers at 0.2 mm

// ---- Battery ----
batt_press = 0.5;   // the shell's ribs press into the battery's front this far; more if it still moves

// ---- SHT41 ----
sht_back = 0.5;     // air between its back edge and the back wall
sht_play = 0.1;     // between it and its lips; 0.3 rattled. More if it won't slide in
sht_side = 0.15;    // each side, between it and the rails' inside faces
sht_lip = 0.8;      // how far the lips reach over its back corners

// ---- Shell screw: one M2.5 x 6 from the back, through the core into a post on the shell ----
screw_clear = 2.9;  // through the core
screw_head = [5.5, 2.0]; // counterbore for the pan head, so the back stays flat on a wall

// ---- Colour (AMS): shells print face down, so face artwork is inlaid in the first layers ----
inlay_t = 0.6;      // inlay depth: 3 layers at 0.2 mm, each colour swap happens only there

// ---- Walls ----
wall = 2.0;         // core walls
shell_t = 1.6;      // shell sleeve and rim walls
front_t = 1.8;      // shell front plate; it prints on the bed
edge_ch = 0.8;      // chamfer round the cover's front edge

