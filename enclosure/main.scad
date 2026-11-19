// thermo-feathers node enclosure. Print in PLA, no supports.
//
//   openscad -D 'part="core"' -D 'id="610338"' -o enclosure/stl/core_610338.stl enclosure/main.scad
//                                                                    (back down on the bed)
//   openscad -D 'part="shell"' -D 'shell="plain"' -D 'colour=1' -o enclosure/stl/shell_plain_c1.stl enclosure/main.scad
//
// A shell is up to four STLs, one per colour, in the same coordinates: load them together as one
// object's parts and give each a filament.
//
// Print the test_* parts first, each cut from the same geometry, then tune knobs.scad and reprint:
//   test_feather   back wall: standoffs, pins, button holes, and the -x wall's USB opening
//   test_sensor    the bottom of the core: SHT41 rails and lips, thermal break, bay floor
//
// check, check_core: boards intersected with the shell or the core; empty when nothing collides.
//
// part: assembly | exploded | core | core_view | shell | shell_view | hardware | check | check_core
//       | test_* | palette
part = "assembly";
id = "";           // the node's device ID (gateway page, device_status), cut into the core's back
shell = "plain";
colour = 0;        // 0: every colour (views); 1-4: just that colour's part
explode = 40;
turn = false;      // renders: OpenSCAD lights from -y, our front is +y

include <shell.scad>
include <shells/plain.scad>

// name, title, palette
shells = [["plain", "Plain", plain_palette]];
core_color = "#3a3d45";

module the_shell(n = shell, c = colour) {
  p = c == 0 ? "all" : str("c", c);
  if (n == "plain") shell_plain(p);
}
module shown() if (turn) rotate([0, 0, 180]) children(); else children();

if (part == "assembly") shown() { color(core_color) core(); hardware(); the_shell(); }
if (part == "exploded") shown() { color(core_color) core(); hardware(); translate([0, explode, 0]) the_shell(); }
if (part == "core") core_print() core();
if (part == "core_view_only") core();   // in place, for the viewer
if (part == "core_view") shown() { color(core_color) core(); hardware(); }
if (part == "shell") shell_print() the_shell();
if (part == "shell_view") shown() the_shell();
if (part == "hardware") hardware();
if (part == "check") intersection() { the_shell(c = 0); boards(); }
if (part == "check_core") intersection() { core(); boards(); }

module cut(from, to) intersection() { children(); translate(from) cube(to - from); }
if (part == "test_feather") core_print() cut([-core_W, -1, board_z - 1.5], [board_x + fw + 2, board_y + pcb + 3, board_z + fh + 4]) core();
if (part == "test_sensor") core_print() cut([-core_W, -1, -1], [core_W, core_D + 1, bay_z + 0.01]) core();
if (part == "palette") for (s = shells) echo(str("SHELL|", s[0], "|", s[1], "|", s[2][0], "|", s[2][1], "|", s[2][2], "|", s[2][3]));

// Section through the shell screw, seen from above. OpenCSG paints every cut face one colour, so
// render.py exports each cut part with CGAL (sec_*) and draws them together.
module sec() translate([0, 0, 10]) rotate([90, 0, 0]) translate([0, 0, -screw_c.y]) shown()
  difference() { children(); translate([-150, -150, screw_c.y]) cube(300); }
if (part == "sec_core") sec() core();
if (part == "sec_hw") sec() hardware();
if (part == "sec_shell") sec() the_shell(c = 1);
