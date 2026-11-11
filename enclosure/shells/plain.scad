// Plain: the cover at its smallest. Same outline as the core, a chamfered front edge, and a fine
// louvre band across the bottom over the sensor pocket and the thermal break. One colour.
plain_palette = ["#e9e6df", "#e9e6df", "#e9e6df", "#e9e6df"];
module shell_plain(part = "all") {
  hw = core_W / 2 - 7;   // louvre half-length
  if (part == "all" || part == "c1") color(part == "all" ? plain_palette[0] : undef) shell_body(rim = 0) {
    sleeve_outline();
    for (z = [feet + floor_t + 0.9 : 2.6 : pocket_top + gap_h - 1.5]) translate([0, z]) hull() { for (s = [-1, 1]) translate([s * hw, 0.7]) circle(d = 1.4, $fn = 16); }
  }
}
