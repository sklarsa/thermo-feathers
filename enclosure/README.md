# Enclosure

A node is a **core**, which holds the boards, and a **shell** over it, which slides on from the
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
- **The shell is screwed on**, by one M2.5 screw from the back into a post on the shell, flush in
  a counterbore. Snap tabs were tried and shook off. The battery and the SHT41
  slide in from the front, and the shell's front plate keeps them there.
- **Prints without supports.** The core prints back down, so every wall, fin and post rises
  straight off the bed. Shells print front down, as the shell.scad notes say.
- **Button holes** in the back reach RESET and BOOT with a toothpick or a SIM tool: hold BOOT,
  tap RESET, to flash a factory-fresh or wedged board over USB. A tube runs each hole to its
  button, so the STEMMA QT cable can't cover it. The CHG LED is next to the USB socket and shows through
  its opening.

## Print

```sh
openscad -D 'part="core"' -o enclosure/stl/core.stl enclosure/main.scad
openscad -D 'part="shell"' -D 'shell="plain"' -D 'colour=1' -o enclosure/stl/shell_plain_c1.stl enclosure/main.scad
```

Give each core its node's device ID (on the gateway's page, or in device_status), cut into the
back so a node can be told apart after moving rooms:

```sh
just core 610338
```

`stl/` has them built: a core for each node so far, and `core.stl` without an ID.

After changing a fit, print its test piece rather than a whole core. Each is cut from the same
geometry; build one with `openscad -D 'part="test_feather"' -o test_feather.stl main.scad`:

| Part | Checks | Knobs (`knobs.scad`) |
|---|---|---|
| `test_feather` | Feather on its standoffs and pins, screws biting, USB plug in, buttons by toothpick | `m25_pilot`, `pin_d`, `qt_gap` |
| `test_sensor` | SHT41 slides in under its lips and sits still | `sht_play`, `sht_lip` |

`part="check_core"` and `part="check"` intersect the boards with the core or the shell: both
should come out empty, apart from faces that touch on purpose (standoffs, battery fins).
`python3 enclosure/render.py` renders every view, the STLs and `viewer.html`; it needs openscad
and an ImageMagick with PNG support.

## Parts

- 3 × M2.5 × 6 mm pan-head self-tapping screws: two for the Feather (the pins at the antenna end
  only locate it), one for the shell
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
4. Slide the shell on from the front, then drive the third screw into
   the counterbored hole on the back, beside the device ID.

## Shells

`shell.scad` is the interface: a shell supplies flat artwork (outline, holes, up to three inlay
colours) and gets the sleeve, screw post, USB opening and side vents for free. `shells/plain.scad`
is the only one so far. Its holes must open over the sensor pocket.

## The gateway

`gateway.scad`: the gateway Feather, on USB power on a shelf, in a 57 × 35 × 16 mm case. It
reuses the node's board data and fits: the Feather screws to a base plate, component side up, and
a cover drops over it and is held by two screws from underneath, one near each end, flush in
counterbores.

- Print `stl/gw_base.stl` flat and `stl/gw_cover.stl` top down, no supports.
- 4 × M2.5 × 6 screws: two through the Feather's USB-end holes into the base (their pilots go
  right through, so a 6 mm screw can't bottom out), two up through the base into the cover's posts.
- RESET and BOOT through funnelled holes in the top, as on a node. Air comes in low along the
  sides and leaves through slots over the S3 module.
- `part="gw_check"` intersects the Feather with both parts: it should come out empty.

## Not done yet

- First core, shell and test pieces printed 2026-11-17. Fixed since, not yet printed: flared post
  roots (one broke off mid-print), a snugger SHT41, a cable slot the battery fin no longer
  blocks, the shell's battery ribs and the BATTERY label,
  3.2 mm button holes with tubes (the cable covered them), and a screw for the shell in place of
  snap tabs (they shook off).
- Other numbers are still from Adafruit's board files, not calipers.
- More shells.
