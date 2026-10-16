// Legacy plugins mod: runs the native assistant plugins (Razor / ClassicAssist DLLs from
// settings.json `plugins`) that cuo-host loads through the net472 launcher, and wires them
// to the client. It is the port of the client's old in-guest LegacyPluginPlugin: the
// plugin DLLs, their C ABI and the cliloc table live in cuo-host
// (`cuo:modding/legacy-plugins`); this mod is the policy side.
//
// Systems:
//   - input   (PreUpdate)  key / mouse edges (cuo:input/keyboard + cuo:input/mouse) → the plugins' OnHotkey / OnMouse; a key a
//                          plugin answers "consumed" is eaten (and the client's own hotkey
//                          on it retracted) via cuo:input/consume.
//   - tick    (Update)     loads the plugins once, drains their requests (inject packets,
//                          walk, cast, title, ...), reports the player's position and the
//                          autowalk state, and ticks them.
// Packet observers (incoming + outgoing, every id): the plugins' OnRecv / OnSend, with the
// pass / block / replace verdict applied by the client.
//
// Connect / disconnect / focus / closing events are raised by cuo-host itself: it owns the
// socket and the window, and a mod has no resource for either.

use cuo_mod_sdk::actions::{self, Ability, Direction, Point};
use cuo_mod_sdk::prelude::*;
use std::cell::RefCell;

// Plugin -> client request kinds of `legacy-plugins.poll`.
const K_INJECT_IN: u8 = 1;
const K_INJECT_OUT: u8 = 2;
const K_CAST: u8 = 3;
const K_MOVE: u8 = 4;
const K_TITLE: u8 = 5;
const K_PRIMARY: u8 = 6;
const K_SECONDARY: u8 = 7;
const K_AUTOWALK: u8 = 8;
const K_WALK_TO: u8 = 9;

const EV_TICK: u32 = 0;

// MouseButtonType values the plugins get (the client passed them untouched).
const MOUSE_MIDDLE: i32 = 2;
const MOUSE_X1: i32 = 4;
const MOUSE_X2: i32 = 5;

static ALL_IDS: [u8; 256] = {
    let mut ids = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        ids[i] = i as u8;
        i += 1;
    }
    ids
};

#[derive(Default)]
struct State {
    /// `legacy_plugins::load` was tried.
    started: bool,
    /// At least one plugin is loaded: every other call is live.
    loaded: bool,
    packet_lengths: Vec<i16>,
    has_pos: bool,
    pos: (i32, i32, i32),
    /// The plugins' autowalk query: a walk-to is under way.
    autowalking: bool,
    /// (x, y, distance) of the walk-to being waited for.
    walk_target: Option<(i32, i32, i32)>,
    down: Vec<bool>,
    any_down: bool,
}

thread_local! {
    static S: RefCell<State> = RefCell::new(State::default());
}

fn setup(app: &mut App) {
    app.add_systems(Schedule::PreUpdate, input);
    app.add_systems(Schedule::Update, tick);
    app.add_packet_observer(PacketDirection::Incoming, &ALL_IDS, on_incoming);
    app.add_packet_observer(PacketDirection::Outgoing, &ALL_IDS, on_outgoing);
}

// ── packets ─────────────────────────────────────────────────────────────────────────

#[system]
fn on_incoming(packet: Packet) -> Verdict {
    through(true, packet)
}

#[system]
fn on_outgoing(packet: Packet) -> Verdict {
    through(false, packet)
}

fn through(incoming: bool, packet: Packet) -> Verdict {
    if !S.with(|s| s.borrow().loaded) {
        return Verdict::Pass;
    }
    verdict(&legacy_plugins::packet(incoming, packet.bytes()))
}

/// The host's verdict blob: [0] pass, [1] block, [2, bytes..] replace.
fn verdict(blob: &[u8]) -> Verdict {
    match blob.split_first() {
        Some((1, _)) => Verdict::Block,
        Some((2, rest)) if !rest.is_empty() => Verdict::Replace(rest.to_vec()),
        _ => Verdict::Pass,
    }
}

// ── input ───────────────────────────────────────────────────────────────────────────

#[system]
fn input(mut cmds: Commands, kb: Option<Res<KeyboardInputDto>>, mouse: Option<Res<MouseInputDto>>) {
    S.with(|s| {
        let mut s = s.borrow_mut();
        if !s.loaded {
            return;
        }
        let Some(kb) = kb else { return };
        let frame = input_frame(&kb, mouse.as_deref());
        if s.down.is_empty() {
            s.down = vec![false; 256];
        }

        let mut down = vec![false; 256];
        for &vk in &frame.keys {
            down[vk as usize] = true;
        }
        for vk in 1..256usize {
            if down[vk] == s.down[vk] {
                continue;
            }
            s.down[vk] = down[vk];
            let sdl = sdl_keycode(vk as u8);
            if sdl == 0 {
                continue;
            }
            // Raw edges: a plugin's binding is visible even on a key the client's own
            // matcher claimed. Precedence is the consume below: a plugin that answers
            // false owns the key.
            if legacy_plugins::hotkey(sdl as i32, frame.mods as i32, down[vk]) || !down[vk] {
                continue;
            }
            cmds.send(InputConsume::key(vk as u32));
        }

        // Legacy also pinged (0,0,false) while nothing was held; some assistants use it
        // as a "keyboard idle" edge to release held actions.
        let any = frame.keys.iter().any(|&k| k != 0);
        if !any && s.any_down {
            legacy_plugins::hotkey(0, 0, false);
        }
        s.any_down = any;

        if frame.wheel != 0 {
            legacy_plugins::mouse(0, frame.wheel as i32);
        }
        for (bit, button) in [(1, MOUSE_MIDDLE), (2, MOUSE_X1), (4, MOUSE_X2)] {
            if frame.buttons & bit != 0 {
                legacy_plugins::mouse(button, 0);
            }
        }
    });
}

struct InputFrame {
    mods: u32,
    wheel: i8,
    buttons: u8,
    keys: Vec<u8>,
}

// SDL_Keymod bits (what the legacy client passed through untouched), from the modifier
// key codes (Win32 VK: what `cuo:input/keyboard` reports).
const KMOD: [(i32, u32); 8] = [
    (160, 0x0001), // left shift
    (161, 0x0002), // right shift
    (162, 0x0040), // left ctrl
    (163, 0x0080), // right ctrl
    (164, 0x0100), // left alt
    (165, 0x0200), // right alt
    (91, 0x0400),  // left windows
    (92, 0x0800),  // right windows
];

/// One frame of plugin input from the client's own input resources: the keys held or
/// pressed this frame (`cuo:input/keyboard`, which the host leaves empty while a text
/// field has focus), the SDL_Keymod derived from them, and the wheel notch / middle / X
/// presses (`cuo:input/mouse`).
fn input_frame(kb: &KeyboardInputDto, mouse: Option<&MouseInputDto>) -> InputFrame {
    let mut down = [false; 256];
    for &k in kb.pressed.iter().chain(&kb.pressed_once) {
        if (1..256).contains(&k) {
            down[k as usize] = true;
        }
    }
    let mods = KMOD.iter().filter(|&&(vk, _)| down[vk as usize]).fold(0, |m, &(_, bit)| m | bit);
    let (wheel, buttons) = mouse.map_or((0, 0), |m| {
        (
            if m.wheel > 0.0 { 1 } else if m.wheel < 0.0 { -1 } else { 0 },
            m.middle_pressed as u8 | (m.x1_pressed as u8) << 1 | (m.x2_pressed as u8) << 2,
        )
    });
    InputFrame { mods, wheel, buttons, keys: (1..256u16).filter(|&k| down[k as usize]).map(|k| k as u8).collect() }
}

/// The SDL keycode of a Win32 virtual-key code (what TinyEcs KeyCode is); 0 = none worth
/// reporting. The legacy client fed plugins SDL keycodes, so every assistant's hotkey
/// table is written in them. SDL2 and SDL3 agree on everything below.
fn sdl_keycode(vk: u8) -> u32 {
    const SC: u32 = 0x4000_0000;
    let v = vk as u32;
    match vk {
        0x41..=0x5A => v - 0x41 + 'a' as u32,
        0x30..=0x39 => v,
        8 => 8,
        9 => 9,
        13 => 13,
        27 => 27,
        32 => 32,
        46 => 127,
        187 => '=' as u32,
        188 => ',' as u32,
        189 => '-' as u32,
        190 => '.' as u32,
        191 => '/' as u32,
        192 => '`' as u32,
        186 => ';' as u32,
        222 => '\'' as u32,
        219 => '[' as u32,
        221 => ']' as u32,
        220 | 226 => '\\' as u32,
        20 => SC | 0x39,
        112..=123 => SC | (0x3A + v - 112),
        124..=135 => SC | (0x68 + v - 124),
        44 => SC | 0x46,
        145 => SC | 0x47,
        19 => SC | 0x48,
        45 => SC | 0x49,
        36 => SC | 0x4A,
        33 => SC | 0x4B,
        35 => SC | 0x4D,
        34 => SC | 0x4E,
        39 => SC | 0x4F,
        37 => SC | 0x50,
        40 => SC | 0x51,
        38 => SC | 0x52,
        144 => SC | 0x53,
        111 => SC | 0x54,
        106 => SC | 0x55,
        109 => SC | 0x56,
        107 => SC | 0x57,
        // SDL orders the keypad 1..9 then 0.
        96 => SC | 0x62,
        97..=105 => SC | (0x59 + v - 97),
        110 => SC | 0x63,
        93 => SC | 0x65,
        162 => SC | 0xE0,
        160 => SC | 0xE1,
        164 => SC | 0xE2,
        91 => SC | 0xE3,
        163 => SC | 0xE4,
        161 => SC | 0xE5,
        165 => SC | 0xE6,
        92 => SC | 0xE7,
        _ => 0,
    }
}

// ── tick ────────────────────────────────────────────────────────────────────────────

#[system]
fn tick(me: Query<&WorldPosition, With<Player>>, ctx: Option<Res<GameContextDto>>) {
    S.with(|s| {
        let mut s = s.borrow_mut();
        if !s.started {
            // The first frame: the client version is known by now, and the login screen
            // is up (no connection yet), like the old Startup load.
            s.started = true;
            if !legacy_plugins::active() {
                return;
            }
            let lengths = legacy_plugins::packet_lengths();
            s.packet_lengths = lengths.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
            let version = ctx.as_ref().map_or(0, |c| c.client_version);
            let loaded = legacy_plugins::load(&[], version, &lengths);
            host::log(&format!("{loaded} plugin(s) loaded by the host"));
            s.loaded = loaded > 0;
        }
        if !s.loaded {
            return;
        }

        let pos = me.single().map(|(_, p)| (p.x as i32, p.y as i32, p.z as i32));
        update_position(&mut s, pos);

        // What the plugins asked for since the last frame, then their tick.
        for request in parse_requests(&legacy_plugins::poll()) {
            apply(&mut s, request);
        }
        update_autowalk(&mut s);
        legacy_plugins::notify(EV_TICK);
    });
}

/// Reports the player's position to the plugins on a real change (a polled edge, like
/// legacy's PlayerMobile hook).
fn update_position(s: &mut State, pos: Option<(i32, i32, i32)>) {
    match pos {
        Some(p) if !s.has_pos || p != s.pos => {
            s.has_pos = true;
            s.pos = p;
            legacy_plugins::player_position(true, p.0, p.1, p.2);
        }
        None if s.has_pos => {
            s.has_pos = false;
            legacy_plugins::player_position(false, 0, 0, 0);
        }
        _ => {}
    }
}

/// The plugins' autowalk query has no client-side source to read, so it is derived: a
/// walk-to turns it on, reaching the target (or an autowalk-off request) turns it off.
fn update_autowalk(s: &mut State) {
    if let Some((x, y, distance)) = s.walk_target {
        let reached = s.has_pos && (s.pos.0 - x).abs().max((s.pos.1 - y).abs()) <= distance;
        if reached {
            s.walk_target = None;
        }
    }
    let on = s.walk_target.is_some();
    if on != s.autowalking {
        s.autowalking = on;
        legacy_plugins::autowalking(on);
    }
}

#[derive(Debug, PartialEq)]
enum Request {
    InjectIncoming(Vec<u8>),
    InjectOutgoing(Vec<u8>),
    CastSpell(i32),
    Move { dir: u8, run: bool },
    SetTitle(String),
    Ability(bool),
    Autowalk(bool),
    WalkTo { x: i32, y: i32, z: i32, distance: i32 },
}

/// `legacy-plugins.poll`: [u8 kind][u32 len][payload] records.
fn parse_requests(mut stream: &[u8]) -> Vec<Request> {
    let mut out = Vec::new();
    while stream.len() >= 5 {
        let kind = stream[0];
        let len = u32::from_le_bytes([stream[1], stream[2], stream[3], stream[4]]) as usize;
        if len > stream.len() - 5 {
            break;
        }
        let p = &stream[5..5 + len];
        stream = &stream[5 + len..];
        let int = |at: usize| p.get(at..at + 4).map(|b| i32::from_le_bytes(b.try_into().unwrap()));
        let request = match kind {
            K_INJECT_IN if len > 0 => Some(Request::InjectIncoming(p.to_vec())),
            K_INJECT_OUT if len > 0 => Some(Request::InjectOutgoing(p.to_vec())),
            K_CAST => int(0).map(Request::CastSpell),
            K_MOVE if len >= 2 => Some(Request::Move { dir: p[0], run: p[1] != 0 }),
            K_TITLE => Some(Request::SetTitle(String::from_utf8_lossy(p).into_owned())),
            K_PRIMARY => Some(Request::Ability(true)),
            K_SECONDARY => Some(Request::Ability(false)),
            K_AUTOWALK if len >= 1 => Some(Request::Autowalk(p[0] != 0)),
            K_WALK_TO if len >= 16 => Some(Request::WalkTo {
                x: int(0).unwrap(),
                y: int(4).unwrap(),
                z: int(8).unwrap(),
                distance: int(12).unwrap(),
            }),
            _ => None,
        };
        out.extend(request);
    }
    out
}

fn apply(s: &mut State, request: Request) {
    match request {
        // A plugin's SendToClient: whole frames, one send each. The client dispatches
        // each as a received packet, skipping this mod's own packet observers; a frame
        // spliced into the wire buffer instead would desync the stream.
        Request::InjectIncoming(data) => {
            for frame in split_frames(&data, &s.packet_lengths) {
                packets::send_to_client(frame);
            }
        }
        // Its SendToServer: sent as the plugin built it, past this mod's own packet hook.
        Request::InjectOutgoing(data) => packets::send_to_server(&data),
        Request::CastSpell(id) => {
            if id > 0 {
                actions::cast_spell(id as u16);
            }
        }
        Request::Move { dir, run } => actions::walk(direction(dir), run),
        Request::SetTitle(title) => actions::set_window_title(&title),
        Request::Ability(primary) => {
            actions::use_ability(if primary { Ability::Primary } else { Ability::Secondary })
        }
        // Legacy's "on" just flipped the flag with no destination; only "off" (cancel the
        // current walk-to) has an effect.
        Request::Autowalk(on) => {
            if !on {
                actions::cancel_walk_to();
                s.walk_target = None;
            }
        }
        Request::WalkTo { x, y, z, distance } => {
            let at = Point { x: x as u16, y: y as u16, z: z as i8 };
            actions::walk_to_within(at, distance.max(0) as u32);
            s.walk_target = Some((x, y, distance.max(0)));
        }
    }
}

fn direction(dir: u8) -> Direction {
    match dir & 7 {
        0 => Direction::North,
        1 => Direction::NorthEast,
        2 => Direction::East,
        3 => Direction::SouthEast,
        4 => Direction::South,
        5 => Direction::SouthWest,
        6 => Direction::West,
        _ => Direction::NorthWest,
    }
}

/// Splits a plugin's injected bytes into whole frames by the client's length table
/// (-1 = variable, length in bytes 1..3 big-endian); what does not fit is one last frame.
fn split_frames<'a>(data: &'a [u8], lengths: &[i16]) -> Vec<&'a [u8]> {
    let mut frames = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let declared = lengths.get(data[at] as usize).copied().unwrap_or(0);
        let mut len = declared as i32;
        if declared == -1 {
            len = if at + 3 <= data.len() { ((data[at + 1] as i32) << 8) | data[at + 2] as i32 } else { 0 };
        }
        if len <= 0 || at + len as usize > data.len() {
            len = (data.len() - at) as i32;
        }
        frames.push(&data[at..at + len as usize]);
        at += len as usize;
    }
    frames
}

export_mod!(setup);
