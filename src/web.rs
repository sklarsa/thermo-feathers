//! The gateway's web page: the settings it keeps in flash, the latest it heard from each node, and
//! the HTML. Plain forms, no JavaScript.

use core::fmt::{self, Write};

use crate::proto::CHUNK;
use crate::{CHARGING, Sample, WAKE_MAX_S, WAKE_MIN_S, crc8};

/// Nodes the page tracks and names it keeps. The design has room for 4.
pub const MAX_NODES: usize = 8;
/// Bytes in a name: names go to QuestDB as `room` symbols, and qwp-embedded's are at most 24.
pub const NAME_MAX: usize = 24;
/// Under this, a node's battery is flagged.
pub const LOW_BATTERY_PCT: f32 = 10.0;
/// A node is stale after this many missed wakes.
pub const STALE_WAKES: u64 = 3;

const MAGIC: [u8; 4] = *b"TFS1";
const NAME_LEN: usize = 6 + 1 + NAME_MAX;
pub const SETTINGS_LEN: usize = 4 + 1 + 2 + MAX_NODES * NAME_LEN + 1;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Name {
    id: [u8; 6],
    len: u8,
    buf: [u8; NAME_MAX],
}

impl Name {
    const EMPTY: Self = Self {
        id: [0; 6],
        len: 0,
        buf: [0; NAME_MAX],
    };

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len as usize]).unwrap_or("")
    }
}

/// What the gateway keeps in flash. Node names are keyed by device ID.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub fahrenheit: bool,
    pub wake_s: u16,
    names: [Name; MAX_NODES],
}

impl Default for Settings {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Settings {
    pub const DEFAULT: Self = Self {
        fahrenheit: true,
        wake_s: 60,
        names: [Name::EMPTY; MAX_NODES],
    };

    pub fn to_bytes(&self) -> [u8; SETTINGS_LEN] {
        let mut b = [0; SETTINGS_LEN];
        b[..4].copy_from_slice(&MAGIC);
        b[4] = self.fahrenheit as u8;
        b[5..7].copy_from_slice(&self.wake_s.to_le_bytes());
        for (n, c) in self.names.iter().zip(b[7..].chunks_exact_mut(NAME_LEN)) {
            c[..6].copy_from_slice(&n.id);
            c[6] = n.len;
            c[7..].copy_from_slice(&n.buf);
        }
        b[SETTINGS_LEN - 1] = crc8(&b[..SETTINGS_LEN - 1]);
        b
    }

    /// `None` for erased flash, a torn write, or anything else that isn't ours.
    pub fn from_bytes(b: &[u8; SETTINGS_LEN]) -> Option<Self> {
        if b[..4] != MAGIC || crc8(&b[..SETTINGS_LEN - 1]) != b[SETTINGS_LEN - 1] {
            return None;
        }
        let mut s = Self {
            fahrenheit: b[4] != 0,
            wake_s: u16::from_le_bytes([b[5], b[6]]).clamp(WAKE_MIN_S, WAKE_MAX_S),
            ..Default::default()
        };
        for (n, c) in s.names.iter_mut().zip(b[7..].chunks_exact(NAME_LEN)) {
            let len = c[6] as usize;
            if len <= NAME_MAX && core::str::from_utf8(&c[7..7 + len]).is_ok() {
                n.id.copy_from_slice(&c[..6]);
                n.len = c[6];
                n.buf.copy_from_slice(&c[7..]);
            }
        }
        Some(s)
    }

    pub fn name(&self, id: &[u8; 6]) -> Option<&str> {
        self.names
            .iter()
            .find(|n| n.len > 0 && n.id == *id)
            .map(Name::as_str)
    }

    /// Sets `id`'s name, trimmed and cut to NAME_MAX bytes on a character boundary; an empty name
    /// forgets it. Returns the name as stored, or `None` when every slot holds another node's name.
    pub fn rename(&mut self, id: [u8; 6], name: &str) -> Option<&str> {
        let name = name.trim();
        let mut end = name.len().min(NAME_MAX);
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        let name = name[..end].trim_end();
        let mine = self.names.iter().position(|n| n.len > 0 && n.id == id);
        if mine.is_none() && name.is_empty() {
            return Some("");
        }
        let i = mine.or_else(|| self.names.iter().position(|n| n.len == 0))?;
        let n = &mut self.names[i];
        *n = Name::EMPTY;
        if !name.is_empty() {
            n.id = id;
            n.len = name.len() as u8;
            n.buf[..name.len()].copy_from_slice(name.as_bytes());
        }
        Some(self.names[i].as_str())
    }
}

/// The latest the gateway heard from a node. Not kept through a reboot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seen {
    pub id: [u8; 6],
    /// Gateway uptime, in seconds, when the node was last heard from.
    pub at_s: u64,
    /// NaN until a good sample.
    pub temp_c: f32,
    pub rh_pct: f32,
    pub charging: bool,
    /// NaN without a battery gauge.
    pub battery_pct: f32,
    pub fw: [u8; 12],
    pub rssi: i32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Nodes(pub [Option<Seen>; MAX_NODES]);

impl Nodes {
    pub const EMPTY: Self = Self([None; MAX_NODES]);

    /// Records a frame from `s.id`. The newest good sample in `samples` gives the reading; a frame
    /// without one keeps the last. A new node takes a free slot, or the one heard from longest ago.
    pub fn heard(&mut self, mut s: Seen, samples: &[Sample]) {
        let old = self.0.iter().position(|x| x.is_some_and(|x| x.id == s.id));
        if let Some(g) = samples.iter().rev().find(|x| !x.temp_c().is_nan()) {
            (s.temp_c, s.rh_pct) = (g.temp_c(), g.rh_pct());
            s.charging = g.flags & CHARGING != 0;
        } else if let Some(o) = old.and_then(|i| self.0[i]) {
            (s.temp_c, s.rh_pct, s.charging) = (o.temp_c, o.rh_pct, o.charging);
        }
        let i = old
            .or_else(|| self.0.iter().position(Option::is_none))
            .unwrap_or_else(|| {
                (0..MAX_NODES)
                    .min_by_key(|&i| self.0[i].map_or(0, |x| x.at_s))
                    .unwrap_or(0)
            });
        self.0[i] = Some(s);
    }
}

/// Where one node's update stands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Progress {
    /// Offered in the node's next reply.
    Waiting,
    /// Fetching; the last offset it asked for.
    Sending(u32),
    /// Fetched the last chunk: it should come back on the new build.
    Sent,
    Done,
    /// Came back on its old build: the update didn't take, or rolled back. Not offered again.
    Failed,
}

/// The firmware image the gateway holds, and which nodes are to get it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rollout {
    /// The image's build ID, as the node will report it.
    pub fw: [u8; 12],
    pub len: u32,
    pub targets: [Option<([u8; 6], Progress)>; MAX_NODES],
}

impl Rollout {
    pub fn new(fw: [u8; 12], len: u32, ids: &[[u8; 6]]) -> Self {
        let mut targets = [None; MAX_NODES];
        for (t, id) in targets.iter_mut().zip(ids) {
            *t = Some((*id, Progress::Waiting));
        }
        Self { fw, len, targets }
    }

    pub fn progress(&self, id: &[u8; 6]) -> Option<Progress> {
        self.targets
            .iter()
            .flatten()
            .find(|t| t.0 == *id)
            .map(|t| t.1)
    }

    fn get(&mut self, id: &[u8; 6]) -> Option<&mut Progress> {
        self.targets
            .iter_mut()
            .flatten()
            .find(|t| t.0 == *id)
            .map(|t| &mut t.1)
    }

    /// Whether `id`'s next reply asks it to fetch the image.
    pub fn offer(&self, id: &[u8; 6]) -> bool {
        matches!(
            self.progress(id),
            Some(Progress::Waiting | Progress::Sending(_))
        )
    }

    /// A Data frame from `id`, which runs build `fw`.
    pub fn heard(&mut self, id: &[u8; 6], fw: &[u8; 12]) {
        let new = self.fw;
        if let Some(p) = self.get(id) {
            if *fw == new {
                *p = Progress::Done;
            } else if *p == Progress::Sent {
                *p = Progress::Failed;
            }
        }
    }

    /// `id` asks for the image at `offset`: whether to answer.
    pub fn fetch(&mut self, id: &[u8; 6], offset: u32) -> bool {
        let len = self.len;
        match self.get(id) {
            Some(p @ (Progress::Waiting | Progress::Sending(_) | Progress::Sent)) => {
                *p = if offset as usize + CHUNK >= len as usize {
                    Progress::Sent
                } else {
                    Progress::Sending(offset)
                };
                true
            }
            _ => false,
        }
    }
}

impl fmt::Display for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Progress::Waiting => f.write_str("update waiting for its next wake"),
            Progress::Sending(_) => f.write_str("updating"),
            Progress::Sent => f.write_str("restarting into the update"),
            Progress::Done => f.write_str("updated"),
            Progress::Failed => f.write_str("<span class=bad>update rolled back</span>"),
        }
    }
}

/// A node's line about the rollout, if it's in one.
fn rollout_line(w: &mut impl Write, r: Option<&Rollout>, id: &[u8; 6]) -> fmt::Result {
    let Some((r, p)) = r.and_then(|r| r.progress(id).map(|p| (r, p))) else {
        return Ok(());
    };
    write!(w, "<br>{p}")?;
    if let Progress::Sending(o) = p {
        write!(w, " {}%", o as u64 * 100 / r.len.max(1) as u64)?;
    }
    write!(w, " · {}", Esc(fw_str(&r.fw)))
}

/// The gateway's own state, for the page's footer.
#[derive(Clone, Copy, Debug, Default)]
pub struct Gateway {
    pub up_s: u64,
    pub channel: u32,
    pub time_synced: bool,
    /// Records in the ring that QuestDB hasn't acked.
    pub waiting: usize,
    /// qwp-embedded's `Stats::last_error`.
    pub last_error: u32,
    /// QuestDB's ip:port, for a link to its web console.
    pub questdb: &'static str,
}

/// HTML-escaped text.
struct Esc<'a>(&'a str);

impl fmt::Display for Esc<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in self.0.chars() {
            match c {
                '&' => f.write_str("&amp;")?,
                '<' => f.write_str("&lt;")?,
                '>' => f.write_str("&gt;")?,
                '"' => f.write_str("&quot;")?,
                '\'' => f.write_str("&#39;")?,
                c => f.write_char(c)?,
            }
        }
        Ok(())
    }
}

/// "45s", "12m", "3h", "2d".
struct Age(u64);

impl fmt::Display for Age {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            s @ 0..60 => write!(f, "{s}s"),
            s @ 60..3600 => write!(f, "{}m", s / 60),
            s @ 3600..86400 => write!(f, "{}h", s / 3600),
            s => write!(f, "{}d", s / 86400),
        }
    }
}

fn id_str(id: &[u8; 6]) -> &str {
    core::str::from_utf8(id).unwrap_or("??????")
}

fn fw_str(fw: &[u8; 12]) -> &str {
    let n = fw.iter().position(|&b| b == 0).unwrap_or(fw.len());
    core::str::from_utf8(&fw[..n]).unwrap_or("?")
}

const HEAD: &str = r#"<!doctype html><html lang=en><head><meta charset=utf-8>
<meta name=viewport content="width=device-width,initial-scale=1">"#;

const STYLE: &str = r#"<style>
:root{--bg:#f6f5f2;--card:#fff;--fg:#1d1d1b;--dim:#6b6a66;--bad:#b3261e;--ok:#1e6b3a;--okbg:#e3f1e7;--line:#e3e1dc}
@media (prefers-color-scheme:dark){:root{--bg:#161615;--card:#22221f;--fg:#ecebe7;--dim:#9a9893;--bad:#f2867d;--ok:#8fd4a5;--okbg:#1f3326;--line:#34332f}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.4 system-ui,sans-serif}
main{max-width:34rem;margin:0 auto;padding:16px}h1{font-size:1.1rem;margin:0 0 12px;color:var(--dim);font-weight:600;display:flex;justify-content:space-between}
.room{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:12px 16px;margin-bottom:10px;display:grid;grid-template-columns:1fr auto;gap:2px 12px;align-items:baseline}
.name{font-weight:600;font-size:1.1rem;overflow-wrap:anywhere;min-width:0}
summary{cursor:pointer;list-style:none}summary::-webkit-details-marker{display:none}
summary::after{content:" ✎";color:var(--dim);font-weight:400;font-size:.9rem}.name[open] summary::after{content:" ✕"}
.t{font-size:2rem;font-weight:600;font-variant-numeric:tabular-nums;grid-row:span 2}
.sub{color:var(--dim);font-size:.9rem}.bad{color:var(--bad);font-weight:600}.stale .t{color:var(--dim)}
.note{padding:8px 12px;border-radius:8px;margin-bottom:12px;font-weight:600;background:var(--okbg);color:var(--ok)}
.note.bad{background:transparent;border:1px solid var(--bad);color:var(--bad)}
footer{color:var(--dim);font-size:.9rem;margin-top:16px}a{color:inherit}
input,select,button{font:inherit;padding:8px 10px;border-radius:8px;border:1px solid var(--line);background:var(--card);color:var(--fg)}
button{cursor:pointer;background:var(--fg);color:var(--bg);border-color:var(--fg);font-weight:600}
.rename{display:flex;gap:8px;margin:8px 0 4px}.rename input{flex:1;min-width:0}
.stack label{display:flex;flex-direction:column;gap:4px;margin-bottom:16px;font-weight:600}
.stack small{color:var(--dim);font-weight:400}.stack button{width:100%}
.stack .check{flex-direction:row;align-items:baseline;font-weight:400;gap:8px}
</style></head><body><main>"#;

/// Drops `/saved` from the address bar, so a reload doesn't say "Saved." again.
const UNSAVE: &str = "<script>if(location.pathname.endsWith('/saved'))\
history.replaceState(null,'',location.pathname.slice(0,-6)||'/')</script>";

/// Swaps in fresh cards every 30 s, unless a rename is open or the tab is hidden.
const REFRESH: &str = "<script>setInterval(async()=>{if(document.hidden||\
document.querySelector('details[open]'))return;try{const t=await(await fetch('/')).text();\
const n=new DOMParser().parseFromString(t,'text/html').getElementById('rooms');\
if(n)document.getElementById('rooms').replaceWith(n)}catch(e){}},30000)</script>";

/// Sends the chosen file as the raw body of `POST /update?to=<ids>`.
const UPLOAD: &str = "<script>up.onsubmit=async e=>{e.preventDefault();\
const to=[...up.querySelectorAll('[name=to]:checked')].map(c=>c.value).join(',');\
if(!to){upmsg.textContent='Pick at least one node.';return}upmsg.textContent='Uploading…';\
try{const r=await fetch('/update?to='+to,{method:'POST',body:img.files[0]});\
upmsg.textContent=await r.text();if(r.ok)setTimeout(()=>location.reload(),1500)}\
catch(e){upmsg.textContent='Upload failed: '+e}}</script>";

/// A line above the page: what was just saved, or why it wasn't.
pub type Note<'a> = Option<Result<&'a str, &'a str>>;

fn note(w: &mut impl Write, n: Note) -> fmt::Result {
    match n {
        Some(Ok(m)) => write!(w, "<p class=note>{}</p>", Esc(m)),
        Some(Err(m)) => write!(w, "<p class='note bad'>{}</p>", Esc(m)),
        None => Ok(()),
    }
}

/// Every node the page knows: heard from since boot, or only named. Sorted by name, then ID.
type Row = ([u8; 6], Option<Seen>);

fn rows(s: &Settings, nodes: &Nodes) -> ([Row; 2 * MAX_NODES], usize) {
    let mut out = [([0u8; 6], None); 2 * MAX_NODES];
    let mut n = 0;
    for x in nodes.0.iter().flatten() {
        out[n] = (x.id, Some(*x));
        n += 1;
    }
    for name in s.names.iter().filter(|x| x.len > 0) {
        if !out[..n].iter().any(|(id, _)| *id == name.id) {
            out[n] = (name.id, None);
            n += 1;
        }
    }
    // named first, by name; then the rest by ID
    out[..n].sort_unstable_by(|a, b| {
        let (na, nb) = (s.name(&a.0), s.name(&b.0));
        (na.is_none(), na.unwrap_or(id_str(&a.0))).cmp(&(nb.is_none(), nb.unwrap_or(id_str(&b.0))))
    });
    (out, n)
}

/// IDs of every node the page knows: heard from since boot, or named.
pub fn known_ids(s: &Settings, nodes: &Nodes) -> ([[u8; 6]; 2 * MAX_NODES], usize) {
    let (rows, n) = rows(s, nodes);
    let mut ids = [[0u8; 6]; 2 * MAX_NODES];
    for (i, r) in rows[..n].iter().enumerate() {
        ids[i] = r.0;
    }
    (ids, n)
}

/// The room view: one card per node, temperature first. Tapping a name opens its rename box.
pub fn render_home(
    w: &mut impl Write,
    s: &Settings,
    nodes: &Nodes,
    g: &Gateway,
    rollout: Option<&Rollout>,
    n: Note,
) -> fmt::Result {
    w.write_str(HEAD)?;
    w.write_str("<title>Rooms</title>")?;
    w.write_str(STYLE)?;
    w.write_str("<h1>Rooms<a href=/settings>Settings</a></h1>")?;
    note(w, n)?;
    w.write_str("<div id=rooms>")?;
    let (rows, n) = rows(s, nodes);
    if n == 0 {
        w.write_str("<p class=sub>No nodes heard from since the gateway started.</p>")?;
    }
    let unit = if s.fahrenheit { "°F" } else { "°C" };
    let stale_s = STALE_WAKES * s.wake_s as u64;
    for (id, seen) in &rows[..n] {
        let named = s.name(id);
        let age = seen.map(|x| g.up_s.saturating_sub(x.at_s));
        let stale = age.is_none_or(|a| a > stale_s);
        write!(
            w,
            "<section class='room{}'><details class=name><summary>{}</summary>\
             <form class=rename method=post action=/name>\
             <input type=hidden name=id value={}>\
             <input name=name value=\"{}\" placeholder='Room name' maxlength={NAME_MAX} \
             aria-label='Room name'><button>Save</button></form></details><div class=t>",
            if stale { " stale" } else { "" },
            Esc(named.unwrap_or(id_str(id))),
            id_str(id),
            Esc(named.unwrap_or(""))
        )?;
        let (Some(x), Some(age)) = (seen, age) else {
            w.write_str("–</div><div class=sub>not heard from since the gateway started")?;
            if named.is_some() {
                write!(w, "<br>{}", id_str(id))?;
            }
            w.write_str("</div></section>")?;
            continue;
        };
        if x.temp_c.is_nan() {
            w.write_str("–")?;
        } else {
            let t = if s.fahrenheit {
                x.temp_c * 9.0 / 5.0 + 32.0
            } else {
                x.temp_c
            };
            write!(w, "{t:.1}{unit}")?;
        }
        w.write_str("</div><div class=sub>")?;
        if !x.rh_pct.is_nan() {
            write!(w, "{:.0}% RH · ", x.rh_pct)?;
        }
        if x.charging {
            w.write_str("charging · ")?;
        } else if x.battery_pct < LOW_BATTERY_PCT {
            write!(w, "<span class=bad>battery {:.0}%</span> · ", x.battery_pct)?;
        } else if !x.battery_pct.is_nan() {
            // the MAX17048 reads a little over 100 % when full
            write!(w, "battery {:.0}% · ", x.battery_pct.min(100.0))?;
        }
        if stale {
            write!(w, "<span class=bad>last seen {} ago</span>", Age(age))?;
        } else {
            write!(w, "{} ago", Age(age))?;
        }
        w.write_str("<br>")?;
        if named.is_some() {
            write!(w, "{} · ", id_str(id))?;
        }
        write!(w, "fw {} · {} dBm", Esc(fw_str(&x.fw)), x.rssi)?;
        rollout_line(w, rollout, id)?;
        w.write_str("</div></section>")?;
    }
    w.write_str("<footer>Gateway: ")?;
    match g.channel {
        0 => w.write_str("<span class=bad>WiFi down</span>")?,
        c => write!(w, "WiFi channel {c}")?,
    }
    write!(w, " · <a href=\"http://{}/\">QuestDB</a>", Esc(g.questdb))?;
    match (g.waiting, g.last_error) {
        (0, _) => w.write_str(" up to date")?,
        (n, 0) => write!(w, ": {n} waiting")?,
        (n, e) => write!(
            w,
            "<span class=bad>: {n} waiting, last error: {}</span>",
            match e {
                1 => "connect",
                2 => "read",
                3 => "write",
                4 => "server",
                5 => "protocol",
                _ => "other",
            }
        )?,
    }
    if !g.time_synced {
        w.write_str(" · <span class=bad>no time yet</span>")?;
    }
    write!(w, " · up {}</footer></div>", Age(g.up_s))?;
    w.write_str(UNSAVE)?;
    w.write_str(REFRESH)?;
    w.write_str("</main></body></html>")
}

/// Unit and wake interval. Names are set on the room cards, next to their readings.
pub fn render_settings(
    w: &mut impl Write,
    s: &Settings,
    nodes: &Nodes,
    rollout: Option<&Rollout>,
    n: Note,
) -> fmt::Result {
    w.write_str(HEAD)?;
    w.write_str("<title>Settings</title>")?;
    w.write_str(STYLE)?;
    w.write_str("<h1><a href=/>← Rooms</a></h1>")?;
    note(w, n)?;
    write!(
        w,
        "<form class=stack method=post action=/settings>\
         <label>Temperature unit<select name=unit>\
         <option value=f{}>°F</option><option value=c{}>°C</option></select></label>\
         <label>Wake interval, seconds<input type=number name=wake min={WAKE_MIN_S} \
         max={WAKE_MAX_S} value={}><small>How often each node reads and sends, from {WAKE_MIN_S} \
         to {WAKE_MAX_S}. Nodes switch at their next send; shorter drains batteries faster.\
         </small></label><button>Save</button></form>",
        if s.fahrenheit { " selected" } else { "" },
        if s.fahrenheit { "" } else { " selected" },
        s.wake_s
    )?;
    w.write_str(
        "<p class=sub>To rename a room, tap its name on the Rooms page. Each rename is also a \
         row in QuestDB's device_locations.</p>",
    )?;
    w.write_str(
        "<h1>Node firmware</h1><form class=stack id=up><label>Image (.bin from espflash \
         save-image)<input type=file id=img accept=.bin required></label>",
    )?;
    let (rows, n) = rows(s, nodes);
    for (id, _) in &rows[..n] {
        write!(
            w,
            "<label class=check><input type=checkbox name=to value={} checked><span>{}",
            id_str(id),
            Esc(s.name(id).unwrap_or(id_str(id)))
        )?;
        rollout_line(w, rollout, id)?;
        w.write_str("</span></label>")?;
    }
    w.write_str(
        "<button>Upload</button><small id=upmsg></small></form><p class=sub>Each node fetches it \
         at its next wake if charging or above 40 % battery, restarts into it, and goes back to its \
         old firmware if the new one doesn't reach the gateway on its first try.</p>",
    )?;
    w.write_str(UPLOAD)?;
    w.write_str(UNSAVE)?;
    w.write_str("</main></body></html>")
}

/// A request's head, once `buf` holds all of it.
pub struct Head<'a> {
    pub post: bool,
    pub path: &'a [u8],
    /// After the `?`; empty without one.
    pub query: &'a [u8],
    /// Where the body starts in `buf`.
    pub len: usize,
    pub content_length: usize,
}

/// `None` until the blank line after the headers arrives, or if the request line is malformed.
pub fn parse_head(buf: &[u8]) -> Option<Head<'_>> {
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let mut lines = buf[..end].split(|&b| b == b'\n');
    let mut first = lines.next()?.split(|&b| b == b' ');
    let post = match first.next()? {
        b"GET" | b"HEAD" => false,
        b"POST" => true,
        _ => return None,
    };
    let target = first.next()?;
    let (path, query) = match target.iter().position(|&b| b == b'?') {
        Some(i) => (&target[..i], &target[i + 1..]),
        None => (target, &[][..]),
    };
    let mut len = 0;
    for l in lines {
        if let Some(i) = l.iter().position(|&b| b == b':')
            && l[..i].eq_ignore_ascii_case(b"content-length")
        {
            len = core::str::from_utf8(&l[i + 1..])
                .ok()?
                .trim()
                .parse()
                .ok()?;
        }
    }
    Some(Head {
        post,
        path,
        query,
        len: end,
        content_length: len,
    })
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// `key`'s value in a form body (`application/x-www-form-urlencoded`), decoded into `out`. `None`
/// if it's missing, longer than `out` or not UTF-8.
pub fn form_value<'a>(body: &[u8], key: &str, out: &'a mut [u8]) -> Option<&'a str> {
    for pair in body.split(|&b| b == b'&') {
        let (k, v) = match pair.iter().position(|&b| b == b'=') {
            Some(i) => (&pair[..i], &pair[i + 1..]),
            None => (pair, &[][..]),
        };
        if k != key.as_bytes() {
            continue;
        }
        let (mut i, mut n) = (0, 0);
        while i < v.len() {
            let b = match v[i] {
                b'+' => b' ',
                b'%' => match (
                    v.get(i + 1).and_then(|&b| hex(b)),
                    v.get(i + 2).and_then(|&b| hex(b)),
                ) {
                    (Some(h), Some(l)) => {
                        i += 2;
                        h << 4 | l
                    }
                    _ => b'%',
                },
                b => b,
            };
            *out.get_mut(n)? = b;
            n += 1;
            i += 1;
        }
        return core::str::from_utf8(&out[..n]).ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_reject_garbage() {
        let mut s = Settings {
            fahrenheit: false,
            wake_s: 120,
            ..Default::default()
        };
        s.rename(*b"610338", "Bedroom").unwrap();
        let b = s.to_bytes();
        assert_eq!(Settings::from_bytes(&b), Some(s));
        assert_eq!(Settings::from_bytes(&[0xFF; SETTINGS_LEN]), None); // erased flash
        let mut torn = b;
        torn[20] ^= 1;
        assert_eq!(Settings::from_bytes(&torn), None);
    }

    #[test]
    fn rename_trims_cuts_and_forgets() {
        let mut s = Settings::default();
        assert_eq!(s.rename(*b"aaaaaa", "  Kitchen "), Some("Kitchen"));
        assert_eq!(s.rename(*b"aaaaaa", "Den"), Some("Den")); // same slot
        assert_eq!(s.name(b"aaaaaa"), Some("Den"));
        // 23 ASCII bytes and a 2-byte é: cut before the é, not through it
        let long = "abcdefghijklmnopqrstuvwé";
        assert_eq!(s.rename(*b"bbbbbb", long), Some("abcdefghijklmnopqrstuvw"));
        assert_eq!(s.rename(*b"aaaaaa", ""), Some(""));
        assert_eq!(s.name(b"aaaaaa"), None);
        for i in 0..MAX_NODES as u8 {
            let _ = s.rename([b'c', b'c', b'c', b'c', b'c', b'0' + i], "x");
        }
        assert_eq!(s.rename(*b"zzzzzz", "full"), None);
        assert_eq!(s.rename(*b"zzzzzz", ""), Some("")); // forgetting a stranger is fine
    }

    fn seen(id: &[u8; 6], at_s: u64) -> Seen {
        Seen {
            id: *id,
            at_s,
            temp_c: f32::NAN,
            rh_pct: f32::NAN,
            charging: false,
            battery_pct: 50.0,
            fw: *b"0.3.0\0\0\0\0\0\0\0",
            rssi: -45,
        }
    }

    #[test]
    fn nodes_keep_last_good_reading_and_evict_oldest() {
        let mut n = Nodes::default();
        n.heard(seen(b"aaaaaa", 1), &[Sample::new(1, 21.5, 40.0, 0)]);
        n.heard(seen(b"aaaaaa", 2), &[Sample::new(2, f32::NAN, f32::NAN, 1)]);
        let a = n.0[0].unwrap();
        assert_eq!((a.at_s, a.temp_c), (2, 21.5));
        for i in 0..MAX_NODES as u8 {
            n.heard(
                seen(&[b'b', b'b', b'b', b'b', b'b', b'0' + i], 10 + i as u64),
                &[],
            );
        }
        assert!(n.0.iter().flatten().all(|x| x.id != *b"aaaaaa")); // oldest went
    }

    #[test]
    fn home_page_flags_and_escapes() {
        let mut s = Settings::default();
        s.rename(*b"aaaaaa", "<b>Kid's room</b>").unwrap();
        s.rename(*b"cccccc", "Attic").unwrap(); // named, never heard from
        let mut n = Nodes::default();
        let mut low = seen(b"aaaaaa", 100);
        low.battery_pct = 7.0;
        n.heard(low, &[Sample::new(1, 20.0, 41.0, 0)]);
        n.heard(seen(b"bbbbbb", 0), &[Sample::new(1, 22.0, 40.0, 0)]);
        let g = Gateway {
            up_s: 300,
            channel: 6,
            time_synced: true,
            questdb: "10.0.0.2:9000",
            ..Default::default()
        };
        let mut h = String::new();
        let mut ro = Rollout::new(*b"0.4.0-abcdef", 1000, &[*b"aaaaaa"]);
        ro.fetch(b"aaaaaa", 500);
        render_home(&mut h, &s, &n, &g, Some(&ro), Some(Ok("Saved."))).unwrap();
        assert!(h.contains("updating 50% · 0.4.0-abcdef"));
        assert!(h.contains("class=note>Saved."));
        assert!(h.contains("&lt;b&gt;Kid&#39;s room&lt;/b&gt;") && !h.contains("<b>Kid"));
        assert!(h.contains("68.0°F") && h.contains("class=bad>battery 7%"));
        // bbbbbb: 300 s since heard, past 3 × 60 s
        assert!(h.contains("last seen 5m ago") && h.contains("Attic"));
        assert!(h.contains("WiFi channel 6") && h.contains("QuestDB</a> up to date"));
        assert!(h.contains("href=\"http://10.0.0.2:9000/\""));
        assert!(h.contains("value=bbbbbb") && h.contains("value=\"Attic\""));
        assert!(h.contains("<summary>bbbbbb</summary>")); // unnamed: the ID is the name
        let mut h = String::new();
        render_settings(&mut h, &s, &n, None, Some(Err("nope"))).unwrap();
        assert!(h.contains("name=to value=bbbbbb checked"));
        assert!(h.contains("class='note bad'>nope") && h.contains("value=60"));
    }

    #[test]
    fn rollout_follows_each_node() {
        let (a, b, c) = (*b"aaaaaa", *b"bbbbbb", *b"cccccc");
        let new = *b"0.4.0-abcdef";
        let old = *b"0.3.0-123456";
        let mut r = Rollout::new(new, 300, &[a, b]);
        assert!(r.offer(&a) && r.offer(&b) && !r.offer(&c));
        assert!(!r.fetch(&c, 0)); // not a target
        assert!(r.fetch(&a, 0));
        assert_eq!(r.progress(&a), Some(Progress::Sending(0)));
        assert!(r.fetch(&a, 256)); // the last chunk
        assert_eq!(r.progress(&a), Some(Progress::Sent));
        assert!(!r.offer(&a));
        assert!(r.fetch(&a, 256)); // its answer got lost: asked again
        r.heard(&a, &new);
        assert_eq!(r.progress(&a), Some(Progress::Done));
        // b gets it all but comes back on its old build: rolled back, and left alone
        r.fetch(&b, 256);
        r.heard(&b, &old);
        assert_eq!(r.progress(&b), Some(Progress::Failed));
        assert!(!r.offer(&b) && !r.fetch(&b, 0));
        // a node already on the build is done without fetching
        let mut r = Rollout::new(new, 300, &[a]);
        r.heard(&a, &new);
        assert_eq!(r.progress(&a), Some(Progress::Done));
        // one still waiting that reports its old build keeps waiting
        let mut r = Rollout::new(new, 300, &[a]);
        r.heard(&a, &old);
        assert!(r.offer(&a));
    }

    #[test]
    fn parses_requests_and_forms() {
        let r = b"POST /name?x=1 HTTP/1.1\r\nHost: g\r\ncontent-length: 7\r\n\r\nid=abc";
        let h = parse_head(r).unwrap();
        assert!(h.post && h.path == b"/name" && h.query == b"x=1");
        assert_eq!((h.len, h.content_length), (r.len() - 6, 7));
        assert!(parse_head(b"GET / HTTP/1.1\r\nHost: g\r\n").is_none()); // not all there yet
        assert!(parse_head(b"BREW / HTTP/1.1\r\n\r\n").is_none());
        assert_eq!(parse_head(b"GET /a HTTP/1.1\r\n\r\n").unwrap().query, b"");
        let mut out = [0; 32];
        let body = b"id=610338&name=Kid%27s+room%20%E2%9C%93&bad=%zz";
        assert_eq!(form_value(body, "name", &mut out), Some("Kid's room ✓"));
        assert_eq!(form_value(body, "id", &mut out), Some("610338"));
        assert_eq!(form_value(body, "bad", &mut out), Some("%zz"));
        assert_eq!(form_value(body, "nope", &mut out), None);
        assert_eq!(form_value(b"name=%FF", "name", &mut out), None); // not UTF-8
        assert_eq!(form_value(b"a&b=1", "a", &mut [0; 4]), Some("")); // key only
        let long = [b"n=".as_slice(), &[b'x'; 40]].concat();
        assert_eq!(form_value(&long, "n", &mut [0; 4]), None);
    }
}
