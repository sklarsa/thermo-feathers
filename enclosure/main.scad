// thermo-feathers node enclosure. Print in PLA, no supports.
//
//   openscad -D 'part="core"' -o stl/core.stl enclosure/main.scad
//   openscad -D 'part="shell"' -D 'shell="plain"' -D 'colour=1' -o stl/shell_plain_c1.stl enclosure/main.scad
//
// A shell is up to four STLs, one per colour, in the same coordinates: load them together as one
// object's parts and give each a filament.
//
// part: assembly | exploded | core | core_view | shell | shell_view | hardware | check | palette
part = "assembly";
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
if (part == "core") core();
if (part == "core_view") shown() { color(core_color) core(); hardware(); }
if (part == "shell") shell_print() the_shell();
if (part == "shell_view") shown() the_shell();
if (part == "hardware") hardware();
if (part == "check") intersection() { the_shell(c = 0); hardware(); }
if (part == "palette") for (s = shells) echo(str("SHELL|", s[0], "|", s[1], "|", s[2][0], "|", s[2][1], "|", s[2][2], "|", s[2][3]));

// Section through the snap tabs, seen from above. OpenCSG paints every cut face one colour, so
// render.py exports each cut part with CGAL (sec_*) and draws them together.
module sec() translate([0, 0, 10]) rotate([90, 0, 0]) translate([0, 0, -tab_z]) shown()
  difference() { children(); translate([-150, -150, tab_z]) cube(300); }
if (part == "sec_core") sec() core();
if (part == "sec_hw") sec() hardware();
if (part == "sec_shell") sec() the_shell(c = 1);
