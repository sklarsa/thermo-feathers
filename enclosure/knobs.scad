// Shared knobs for the core and every shell. Hardware never matches the datasheet: change a knob,
// reprint the matching test_* part, repeat.
//
// Material is PLA, not PETG as in esp-light-show. PLA is stiffer and more brittle and creeps less,
// so snap arms are longer and lips smaller, keeping bending strain under about 2 %. PLA softens
// around 55 °C: keep nodes out of sunny windows.
//
// STAGE 1 (concept sheet): only the knobs the rough shells use are live. The board-holding knobs
// arrive with the real core in stage 2.

// ---- Fits ----
tol = 0.3;          // gap around a board or the battery, from light-show
shell_clear = 0.3;  // air between the core's outside and a shell's inside

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
