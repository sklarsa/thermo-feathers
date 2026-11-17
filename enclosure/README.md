# Enclosure

A node is a **core**, which holds the boards, and a **shell** over it, which snaps on from the
front and closes it. Every shell fits every core, so a room's node can change its look without
touching the electronics. OpenSCAD, PLA, no supports.

57 × 21 × 82 mm core, about 61 × 25 × 84 mm with the plain shell. It stands on a shelf or hangs
on a screw.

```
  front (as you face it)    side
 ┌──────────────┐          ┌────┐
 │ ┌──────┐┌──┐ │          │F  B│   F  Feather, components facing the wall,
 │ │ ant. ││  │ │  board   │F  B│      USB out the right-hand side
 │ └──────┘│B │ │          │   B│   B  battery, upright, in front of its USB half
 │         │  │ │          │   B│
 │  cable  │  │ │          │c  B│   c  STEMMA QT cable, down behind the battery
 │   run   └──┘ │          │    │
 ├──────────────┤ floor    ├────┤
 │   break      │          │    │   thermal break: open on three sides
 ├──────────────┤          │    │
 │  [ SHT41 ]   │ pocket   │ S  │   S  SHT41, flat, vented all round
 └──────────────┘          └────┘
```

## What it's built around

- **The sensor sits below everything that makes heat**, in a pocket open at the front, sides and
  floor, with a gap open to the room between it and the electronics. Self-heating is about 1 mW
  on average with ESP-NOW (~0.01 °C). Readings while charging run warm in any case; the firmware
  flags them CHARGING.
- **Nothing covers the antenna.** The ESP32-S3 module's antenna is at the Feather's far end. The
  battery stands in front of the USB half only and clears it by 9.5 mm: a LiPo's foil laid across
  the whole board, as in the first concept, sits between the antenna and the room.
- **Fixings as on the s3-sensors box** (homelab `esp32/enclosure.scad`): two M2.5 self-tapping
  screws through the Feather's plated holes into printed standoffs, pins in its smaller unplated
  holes at the antenna end. The standoffs leave 7 mm under the board for the JST-PH socket and the
  STEMMA QT plug's turn.
- **No tools for the rest.** The battery and the SHT41 slide in from the front; the shell's front
  plate keeps them there.
- **Prints without supports.** The core prints back down, so every wall, fin and post rises
  straight off the bed. Shells print front down, as the shell.scad notes say.
- **Paperclip holes** in the back reach RESET and BOOT: hold BOOT, tap RESET, to flash a
  factory-fresh or wedged board over USB. The CHG LED is next to the USB socket and shows through
  its opening.

## Print

```sh
openscad -D 'part="core"' -o enclosure/stl/core.stl enclosure/main.scad
openscad -D 'part="shell"' -D 'shell="plain"' -D 'colour=1' -o enclosure/stl/shell_plain_c1.stl enclosure/main.scad
```

Give each core its node's device ID (on the gateway's page, or in device_status), cut into the
back so a node can be told apart after moving rooms:

```sh
openscad -D 'part="core"' -D 'id="610338"' -o enclosure/stl/core_610338.stl enclosure/main.scad
```

`stl/` has them built, a core for each node so far and `core.stl` without an ID. Print the test pieces first; each is cut from the same geometry:

| Part | Checks | Knobs (`knobs.scad`) |
|---|---|---|
| `test_feather` | Feather on its standoffs and pins, screws biting, USB plug in, buttons by paperclip | `m25_pilot`, `pin_d`, `qt_gap` |
| `test_sensor` | SHT41 slides in under its lips and sits still | `sht_play`, `sht_lip` |

`part="check_core"` and `part="check"` intersect the boards with the core or the shell: both
should come out empty, apart from faces that touch on purpose (standoffs, battery fins).
`python3 enclosure/render.py` renders every view, the STLs and `viewer.html`; it needs openscad
and an ImageMagick with PNG support.

## Parts

- 2 × M2.5 × 6 mm pan-head self-tapping screws (the Feather); the pins at the antenna end only locate it
- a STEMMA QT cable of 100 mm (Adafruit 4210); the run is about 85 mm, and a longer one coils
  behind the battery
- to hang it: a pan-head screw with a head under 7.5 mm and a shank under 3.8 mm

## Assembly

1. Plug the STEMMA QT cable and the battery into the Feather (both sockets face the wall once
   it's in), then screw it to its standoffs, components to the wall, USB to the side opening. Run
   the cable towards the USB side, down behind where the battery goes and through the slot at the
   back of the bay floor.
2. Plug the cable into the SHT41's nearer socket, then slide the SHT41 in from the front on its
   rails, sensor up, until it stops at the back wall under its lips.
3. Stand the battery on the floor where it says BATTERY, in front of the board's USB half, wires at the top. Ribs inside the shell press it onto its fins.
4. Push the shell on from the front until both tabs click.

## Shells

`shell.scad` is the interface: a shell supplies flat artwork (outline, holes, up to three inlay
colours) and gets the sleeve, snap tabs, USB opening and side vents for free. `shells/plain.scad`
is the only one so far. Its holes must open over the sensor pocket.

## Not done yet

- Nothing printed: every number is from Adafruit's board files, not calipers.
- More shells.
