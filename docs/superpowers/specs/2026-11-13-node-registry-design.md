# Node registry and node-aware gateway health

Draft for review, 2026-11-13. Addresses review findings 3 (a gateway update counts as healthy
without hearing from any node) and 6 (spoofed frames fill the symbol table and the page).

## Today

- Any `Data` frame from any MAC is interned in `SYMBOLS` (128, append-only until reboot), takes
  one of the page's 8 `Nodes` slots (evicting the one heard from longest ago), and goes to
  QuestDB.
- Settings in flash hold names keyed by device ID; a node without a name isn't remembered across
  a reboot.
- A gateway image on trial is kept once it has WiFi, the time and one QuestDB ack.

## Registry

- **Registered nodes** live in the settings partition: up to 8 entries of device ID and name (the
  name may be empty). Layout TFS3; a TFS2 save's named nodes become registered.
- **Only registered nodes** are interned, shown as rooms, and forwarded to QuestDB. How a node gets
  registered is under [Registration, with keys](#registration-with-keys): its first frame with a
  valid tag.
- **A node the gateway won't take** (forgotten, or a valid tag but the registry is full) gets a
  `Reply` with `stored: 0`. It learns the time and channel, so it doesn't back off into sweeps,
  and keeps its samples. That's what `stored: 0` already means to nodes.
- **Renaming** stays as it is, for registered nodes only.

## Gateway health

- **Before an update restart,** the gateway writes the IDs of registered nodes it heard from in
  the last 3 wake intervals into RTC memory (beside `PANICS`, with its own magic). That's the
  expected set. A software reset keeps RTC memory; nothing goes to flash.
- **On trial,** the new image is healthy once it has WiFi, the time, a QuestDB ack, and a `Data`
  frame from every node in the expected set. An empty set (no node was active) falls back to
  today's rule.
- **The trial window** becomes the longer of 10 minutes and 3 wake intervals plus a minute, using
  the wake interval saved in settings.
- **If a node dies during the update window,** the update rolls back. The old image is confirmed,
  so it doesn't loop; the page says which node wasn't heard. Upload again once it's back, or
  Forget it.
- **Ordinary reboots** (power cut, crash) aren't on trial and don't use the expected set.

## Page

- Registered nodes that haven't reported since the gateway started show as "not heard since
  restart", so a node that never comes back is visible instead of missing.

## Trust: per-node keys, set at USB flash

A MAC is just a claim. Trust comes from a key only our gateway and that node hold.

- **Master secret:** 32 random bytes, generated once and kept in sops beside the WiFi password.
  The gateway gets it at build time, the same way it gets the WiFi credentials.
- **Node key:** `HMAC-SHA256(master, "thermo-node" ‖ MAC)`, cut to 16 bytes. The gateway works
  out any node's key from its MAC, so it stores no keys. A node only knows its own, so a stolen
  node gives away that node and no other.
- **Provisioning:** `scripts/flash-node.sh` already flashes every node once over USB. It also
  reads the node's MAC, works out its key, and writes it to the node's `creds` partition (0x9000,
  unused today). OTA images never touch that partition, so one image still serves every node.
- **Every frame is signed:** a 16-byte tag, `HMAC-SHA256(node key, frame)` cut short, at the end
  of each frame, both ways, including the Discover and Hello broadcasts. The gateway drops a
  frame whose tag doesn't verify before it's interned, shown, or replied to. A node drops gateway
  frames the same way: Replies, Chunks and Hellos. Frames fit: the biggest, Chunk, grows from
  172 to 188 of ESP-NOW's 250 bytes.
- **Replays:** a node can't keep a counter through a power cut, and doesn't need one here.
  - A replayed Data frame only re-sends rows QuestDB already has, with the same timestamps, so
    dedup drops them.
  - A Reply has to echo the nonce the node picked for this wake.
  - A Chunk names its image's SHA-256, which the node checks at the end.

### Why not ESP-NOW's own encryption alone

ESP-NOW encryption (a PMK, plus an LMK per peer, CCMP in hardware) hides frames. It's a poor way
to prove who sent them:

- **Plaintext got through.** For years an encrypted peer's plaintext frames were delivered as if
  encrypted ([esp-idf#8577](https://github.com/espressif/esp-idf/issues/8577)). The wifi library
  fix (2022) still lets plaintext through once a broadcast peer exists, and the gateway needs one
  for discovery.
- **No way to check.** esp-radio's `ReceiveInfo` has no "was encrypted" flag.
- **No broadcasts.** Broadcasts can't be encrypted, so Discover and Hello stay open.
- **A peer limit.** 7 encrypted peers by default, 17 at most (`espnow_max_encrypt_num`). The
  gateway would have to add a peer before it could read that node's first frame.

So the tag does the trusting. ESP-NOW encryption can sit on top for privacy, with
`LMK = HMAC(node key, "lmk")` and so no further provisioning. It's optional: room temperatures
and a public repo's firmware aren't secret.

### Registration, with keys

- **A node with a valid tag is ours:** it was flashed with our master secret's key. Its first
  verified frame adds it to the registry. No Add button is needed, and nothing can be spoofed
  onto the page.
- **The page** keeps **Forget**. A forgotten node is refused until it's re-added; re-adding means
  re-flashing it, or an **Allow again** button.
- **A node flashed without a key,** or with another master's, shows under "Unprovisioned"
  (RAM only, 4 entries, never interned) with the instruction to run `flash-node.sh`.
- **Moving to the new scheme:** today's node needs one USB flash, `flash-node.sh`, to get its key.
  Until then the gateway can accept it untagged, if it's already registered and an "allow
  unsigned" setting is on. That setting turns off once every node is signed.

### Still open

- **Firmware uploads.** Anyone on the LAN can upload images to the gateway's page, by design. A
  signed image (an Ed25519 key in sops, checked by both firmwares before booting an update)
  would close that. It's separate from this change.
- **Changing the master secret** means re-flashing every node over USB.

## Size

- **Library:** the tag (sign and verify, HMAC-SHA256 via sha2), proto frames with tags, key
  derivation, and the settings layout (TFS3 registry). Host tests for each.
- **Node:** read its key from `creds`, sign and verify. Every node needs one USB flash.
- **Gateway:** the registry, verifying, the Unprovisioned list, Forget, the expected set and the
  trial rule, page changes.
- **flash-node.sh:** read the MAC, derive the key with the master from sops, write `creds`.

ESP-NOW encryption on top is a further, small change, once the tags work.
