// zhur: terminal whose command line is a real text editor (Warp style).
// Uso: zhur.exe [shell] [args...]   p.ej. `zhur wsl`, `zhur cmd`, `zhur pwsh`. Por defecto: powershell.
// Atajos: Ctrl+Shift+T nueva pestaña, Ctrl+Shift+W cerrarla, Ctrl+Tab cambiar.
#![windows_subsystem = "windows"]

use std::{
    collections::HashMap,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use alacritty_terminal::{
    event::{Event as TermEvent, EventListener, WindowSize},
    event_loop::{EventLoop as PtyLoop, EventLoopSender, Msg},
    grid::{Dimensions, Scroll},
    index::{Column, Line, Point, Side},
    selection::{Selection as TermSel, SelectionType},
    sync::FairMutex,
    term::{self, Term, TermMode, cell::Flags, color::Colors},
    tty,
    vte::ansi::{Color as AColor, CursorShape, NamedColor, Rgb},
};
use glyphon::{
    Action, Attrs, Buffer, Cache, Color, Edit, Editor, Family, FontSystem, Metrics, Resolution,
    Shaping, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport, Weight,
    cosmic_text::{Motion, Selection},
};
use wgpu::util::DeviceExt;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, StartCause, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{Theme, Window, WindowId},
};

// Sizes in logical pixels.
const FONT: f32 = 15.0;
const LINE: f32 = 21.0;
const CHIP_FONT: f32 = 12.5;
const CHIP_LINE: f32 = 17.0;
const MARGIN: f32 = 10.0;
const CARD_PAD: f32 = 12.0;
const RADIUS: f32 = 10.0;
const SIDEBAR: f32 = 52.0;
const BADGE: f32 = 34.0;
const ICON: f32 = 16.0;
const MAX_EDITOR_LINES: usize = 8;
const BLINK: Duration = Duration::from_millis(530);

// Campbell (Windows Terminal) palette for ANSI colors.
const ANSI: [u32; 16] = [
    0x0C0C0C, 0xC50F1F, 0x13A10E, 0xC19C00, 0x0037DA, 0x881798, 0x3A96DD, 0xCCCCCC, 0x767676,
    0xE74856, 0x16C60C, 0xF9F1A5, 0x3B78FF, 0xB4009E, 0x61D6D6, 0xF2F2F2,
];
const BG: u32 = 0x0E1014;
const FG: u32 = 0xD4D4D8;
const SIDEBAR_BG: u32 = 0x0A0C10;
const CARD: u32 = 0x171A21;
const BORDER: u32 = 0x2A2F3A;
const PILL: u32 = 0x232833;
const BLOCK: u32 = 0x161A22;
const MUTED: u32 = 0x8A90A0;
const GHOST: u32 = 0x565C6B;
const ACCENT: u32 = 0x7AA2F7;
const NODE: u32 = 0x8BC34A;
const GIT: u32 = 0xC792EA;
const DIRTY: u32 = 0xE5C07B;
const SELECTION: u32 = 0x2F4A7A;

// Each shell's prompt is replaced by an invisible one that sets the window title to
// `zhur-idle<n>?<shell>?<cwd>`: that is how we know it waits for a command, which shell it is
// and where. It must be the last title the prompt sets: ConPTY only forwards the final title.
// While a command runs, keystrokes go straight to the program.
const IDLE: &str = "zhur-idle";
// The visible prompt is a blue "> ", so each command reads "> cmd" in the history. It ends in "> "
// on purpose: PSReadLine recolors those two chars (PromptText) red on syntax errors.
// It starts with a newline, so a blank row separates blocks; that color is how headers are found.
const PS_HOOK: &str = "function global:prompt { $Host.UI.RawUI.WindowTitle='zhur-idle'+(Get-Random)+'?ps?'+$PWD.ProviderPath; [string][char]10+[char]27+'[38;2;122;162;247m> '+[char]27+'[0m' }";
const CMD_HOOK: &str = "$E]2;zhur-idle$T?cmd?$P$E\\$_$E[38;2;122;162;247m$G $E[0m";
const BASH_HOOK: &str = r#"PS1="\[\e]2;zhur-idle\${RANDOM}?${WSL_DISTRO_NAME:+wsl:}${WSL_DISTRO_NAME:-bash}?\${PWD}\a\]\n\[\e[38;2;122;162;247m\]> \[\e[0m\]""#;
// Only these fonts are loaded: loading every system font costs memory and startup time.
const FONTS: [&str; 9] = [
    "CascadiaMono.ttf", "consola.ttf", "seguisym.ttf", "seguiemj.ttf", "segoeui.ttf", "msyh.ttc",
    "YuGothR.ttc", "malgun.ttf", "segmdl2.ttf",
];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Ps,
    Cmd,
    Bash,
    Wsl,
    Other,
}

impl Kind {
    fn from_name(n: &str) -> Kind {
        match n {
            "powershell" | "pwsh" | "ps" => Kind::Ps,
            "cmd" => Kind::Cmd,
            "bash" => Kind::Bash,
            n if n.starts_with("wsl") => Kind::Wsl,
            _ => Kind::Other,
        }
    }
    /// Tab badge: icon, background, foreground.
    fn badge(self) -> (&'static str, u32, u32) {
        // Segoe MDL2 Assets: E756 = command prompt, E943 = code.
        match self {
            Kind::Ps => ("\u{E756}", 0x1F4E8C, 0xFFFFFF),
            Kind::Wsl => ("\u{E756}", 0xE95420, 0xFFFFFF),
            Kind::Bash => ("\u{E943}", 0x24331F, 0x8BC34A),
            Kind::Cmd => ("\u{E756}", 0x30343C, 0xD4D4D8),
            Kind::Other => ("\u{E943}", 0x30343C, 0xD4D4D8),
        }
    }
    fn color(self) -> u32 {
        match self {
            Kind::Ps => 0x5A9BF0,
            Kind::Wsl => 0xE95420,
            Kind::Bash => NODE,
            Kind::Cmd | Kind::Other => MUTED,
        }
    }
}

/// Returns (program, args, integrated, kind). `integrated` = we know how to detect its prompt.
fn shell_cmd(mut args: Vec<String>) -> (String, Vec<String>, bool, Kind) {
    let program = if args.is_empty() { "powershell.exe".to_string() } else { args.remove(0) };
    let name = Path::new(&program).file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
    let kind = Kind::from_name(&name);
    if kind == Kind::Ps {
        args.extend(["-NoLogo", "-NoExit", "-Command", PS_HOOK].map(String::from));
    }
    // cmd and bash/wsl pick their hook from the environment (PROMPT / PROMPT_COMMAND), which also
    // reaches shells started from inside another one.
    // alacritty does not quote the program, only the args.
    let program = if program.contains(' ') { format!("\"{program}\"") } else { program };
    (program, args, kind != Kind::Other, kind)
}

/// Parses `zhur-idle<n>?<shell>?<cwd>` into (shell, cwd).
fn parse_idle(title: &str) -> Option<(&str, &str)> {
    let mut it = title.strip_prefix(IDLE)?.splitn(3, '?');
    it.next();
    Some((it.next()?, it.next()?))
}

/// Rest of the most recent history entry that starts with `text`.
fn suggest<'a>(history: &'a [String], text: &str) -> Option<&'a str> {
    if text.is_empty() || text.contains('\n') {
        return None;
    }
    history.iter().rev().find(|h| h.len() > text.len() && h.starts_with(text)).map(|h| &h[text.len()..])
}

/// PSReadLine keeps every PowerShell command ever run: seed suggestions and ↑ with it.
fn load_history() -> Vec<String> {
    let path = std::env::var("APPDATA").unwrap_or_default()
        + "\\Microsoft\\Windows\\PowerShell\\PSReadLine\\ConsoleHost_history.txt";
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut h: Vec<String> = Vec::new();
    for l in text.lines().filter(|l| !l.trim().is_empty() && !l.ends_with('`')) {
        if h.last().map(String::as_str) != Some(l) {
            h.push(l.to_string());
        }
    }
    h.split_off(h.len().saturating_sub(5000))
}

/// Prompt info shown as chips above the editor.
#[derive(Default)]
struct Info {
    node: Option<String>,
    path: String,
    branch: Option<String>,
    changes: usize,
}

/// Maps a cwd reported by any shell to a Windows path: `C:\x`, Git Bash `/c/x`, WSL `/mnt/c/x`.
fn win_path(cwd: &str) -> Option<String> {
    if cwd.as_bytes().get(1) == Some(&b':') {
        return Some(cwd.to_string());
    }
    let rest = cwd.strip_prefix("/mnt/").or_else(|| cwd.strip_prefix('/'))?;
    let (drive, tail) = rest.split_once('/').unwrap_or((rest, ""));
    if drive.len() != 1 || !drive.as_bytes()[0].is_ascii_alphabetic() {
        return None;
    }
    Some(format!("{}:\\{}", drive.to_uppercase(), tail.replace('/', "\\")))
}

/// Local wall-clock time as `HH:MM:SS`, straight from Windows (std has no time zones).
fn local_time() -> String {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime([u16; 8]); // year, month, weekday, day, hour, minute, second, ms
    unsafe extern "system" {
        fn GetLocalTime(t: *mut SystemTime);
    }
    let mut t = SystemTime::default();
    unsafe { GetLocalTime(&mut t) };
    format!("{:02}:{:02}:{:02}", t.0[4], t.0[5], t.0[6])
}

/// Completes the last word of `text` as a path under `cwd`; returns the whole new text.
/// shortcut: several matches only complete their common prefix, no candidate list; add one if missed.
fn complete_path(text: &str, cwd: &Path, sep: char) -> Option<String> {
    let quoted = text.matches('"').count() % 2 == 1;
    let start = if quoted { text.rfind('"')? + 1 } else { text.rfind(char::is_whitespace).map_or(0, |i| i + 1) };
    let word = &text[start..];
    let (dir, prefix) = word.split_at(word.rfind(['/', '\\']).map_or(0, |i| i + 1));
    let sep = dir.chars().last().unwrap_or(sep);
    let lower = prefix.to_lowercase();
    let mut names: Vec<(String, bool)> = std::fs::read_dir(cwd.join(dir))
        .ok()?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            n.to_lowercase().starts_with(&lower).then(|| (n, e.path().is_dir()))
        })
        .collect();
    names.sort();
    // Windows paths ignore case, so the common prefix does too.
    let mut common: Vec<char> = names.first()?.0.chars().collect();
    for (n, _) in &names[1..] {
        let same = common.iter().zip(n.chars()).take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase())).count();
        common.truncate(same);
    }
    let mut common: String = common.into_iter().collect();
    if names.len() == 1 && names[0].1 {
        common.push(sep);
    }
    let quote = if !quoted && common.contains(' ') { "\"" } else { "" };
    let new = format!("{}{quote}{dir}{common}", &text[..start]);
    (new != text).then_some(new)
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new(cmd).args(args).creation_flags(CREATE_NO_WINDOW).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs off the UI thread: git and node take tens of milliseconds.
fn probe(cwd: &str) -> Info {
    let Some(dir) = win_path(cwd) else {
        // shortcut: WSL-only paths (/home/...) get no git/node info; would need `wsl git` per prompt.
        let path = match cwd.strip_prefix("/home/") {
            Some(r) => format!("~{}", r.find('/').map_or("", |i| &r[i..])),
            None => cwd.to_string(),
        };
        return Info { path, ..Default::default() };
    };
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    let path = match dir.get(..home.len()) {
        Some(p) if !home.is_empty() && p.eq_ignore_ascii_case(&home) => format!("~{}", &dir[home.len()..]),
        _ => dir.clone(),
    };
    let n = path.chars().count();
    let path = if n > 50 { format!("…{}", path.chars().skip(n - 49).collect::<String>()) } else { path };
    let (branch, changes) = match run("git", &["--no-optional-locks", "-C", &dir, "status", "--porcelain", "-b"]) {
        Some(out) => {
            let mut lines = out.lines();
            let head = lines.next().unwrap_or("").trim_start_matches("## ");
            let head = head.strip_prefix("No commits yet on ").unwrap_or(head);
            (Some(head.split("...").next().unwrap_or(head).to_string()), lines.count())
        }
        None => (None, 0),
    };
    let node = Path::new(&dir)
        .ancestors()
        .any(|p| p.join("package.json").exists())
        .then(|| run("node", &["--version"]))
        .flatten()
        .map(|v| v.trim().to_string());
    Info { node, path, branch, changes }
}

/// Events for the UI thread; the usize is the tab id.
enum Ev {
    Term(usize, TermEvent),
    Info(usize, Info),
}

#[derive(Clone)]
struct Proxy(EventLoopProxy<Ev>, usize);
impl EventListener for Proxy {
    fn send_event(&self, e: TermEvent) {
        let _ = self.0.send_event(Ev::Term(self.1, e));
    }
}

fn rgb(c: u32) -> Rgb {
    Rgb { r: (c >> 16) as u8, g: (c >> 8) as u8, b: c as u8 }
}

fn color(c: u32) -> Color {
    Color::rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

fn index256(i: u8) -> Rgb {
    match i {
        0..16 => rgb(ANSI[i as usize]),
        16..232 => {
            let l = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            let i = i - 16;
            Rgb { r: l(i / 36), g: l(i / 6 % 6), b: l(i % 6) }
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            Rgb { r: v, g: v, b: v }
        }
    }
}

fn resolve(c: AColor, colors: &Colors) -> Rgb {
    match c {
        AColor::Spec(c) => c,
        AColor::Indexed(i) => colors[i as usize].unwrap_or_else(|| index256(i)),
        AColor::Named(n) => colors[n].unwrap_or_else(|| match n as usize {
            i @ 0..16 => rgb(ANSI[i]),
            _ if n == NamedColor::Background => rgb(BG),
            _ => rgb(FG),
        }),
    }
}

fn key_bytes(ev: &KeyEvent, mods: ModifiersState, app_cursor: bool) -> Option<Vec<u8>> {
    let arrow = |c: u8| vec![0x1b, if app_cursor { b'O' } else { b'[' }, c];
    let mut b = match &ev.logical_key {
        Key::Named(n) => match n {
            NamedKey::Enter => b"\r".to_vec(),
            NamedKey::Backspace => vec![0x7f],
            NamedKey::Tab if mods.shift_key() => b"\x1b[Z".to_vec(),
            NamedKey::Tab => b"\t".to_vec(),
            NamedKey::Escape => vec![0x1b],
            NamedKey::ArrowUp => arrow(b'A'),
            NamedKey::ArrowDown => arrow(b'B'),
            NamedKey::ArrowRight => arrow(b'C'),
            NamedKey::ArrowLeft => arrow(b'D'),
            NamedKey::Home => b"\x1b[H".to_vec(),
            NamedKey::End => b"\x1b[F".to_vec(),
            NamedKey::Insert => b"\x1b[2~".to_vec(),
            NamedKey::Delete => b"\x1b[3~".to_vec(),
            NamedKey::PageUp => b"\x1b[5~".to_vec(),
            NamedKey::PageDown => b"\x1b[6~".to_vec(),
            NamedKey::Space if mods.control_key() => vec![0],
            _ => ev.text.as_ref()?.as_bytes().to_vec(),
        },
        Key::Character(s) if mods.control_key() => {
            let c = s.chars().next()?.to_ascii_lowercase();
            match c {
                'a'..='z' => vec![c as u8 & 0x1f],
                '[' => vec![0x1b],
                '\\' => vec![0x1c],
                ']' => vec![0x1d],
                _ => return None,
            }
        }
        _ => ev.text.as_ref()?.as_bytes().to_vec(),
    };
    if mods.alt_key() {
        b.insert(0, 0x1b);
    }
    Some(b)
}

fn mono() -> Attrs<'static> {
    Attrs::new().family(Family::Name("Cascadia Mono"))
}

fn label_buf(fs: &mut FontSystem, metrics: Metrics, text: &str, c: u32, bold: bool) -> Buffer {
    let mut b = Buffer::new(fs, metrics);
    let a = mono().color(color(c));
    b.set_text(text, &if bold { a.weight(Weight::BOLD) } else { a }, Shaping::Advanced, None);
    b.shape_until_scroll(fs, false);
    b
}

/// One glyph from the Windows icon font, `size` px square.
fn icon_buf(fs: &mut FontSystem, size: f32, glyph: &str, c: u32) -> Buffer {
    let mut b = Buffer::new(fs, Metrics::new(size, size));
    b.set_text(glyph, &Attrs::new().family(Family::Name("Segoe MDL2 Assets")).color(color(c)), Shaping::Advanced, None);
    b.shape_until_scroll(fs, false);
    b
}

fn line_w(b: &Buffer) -> f32 {
    b.layout_runs().next().map_or(0.0, |r| r.line_w)
}

/// One shell session.
struct Tab {
    id: usize,
    term: Arc<FairMutex<Term<Proxy>>>,
    pty: EventLoopSender,
    kind: Kind,
    shell_label: String,
    badge: Buffer,
    term_buf: Buffer,
    chips: Vec<Buffer>,
    editor: Editor<'static>,
    // Cached grid render; rebuilt only when the terminal changed.
    term_dirty: bool,
    cell_bgs: Vec<(usize, i32, Rgb)>,
    // Fallback-font glyphs drawn on their own: (x from the text's left, row, buffer).
    glyphs: Vec<(f32, i32, Buffer)>,
    term_cursor: Option<(usize, i32)>,
    shift: usize,
    // Commands sent: (absolute grid line where the shell echoed it, first line of the command, run time).
    blocks: Vec<(usize, String, String)>,
    // Visible header rows, with their run time laid out for the right edge.
    headers: Vec<(i32, Buffer)>,
    idle: bool,
    integrated: bool,
    undo: Vec<String>,
    typing: bool,
    hist_pos: usize,
    // Windows path of the shell's folder, for Tab completion.
    cwd: Option<String>,
}

impl Tab {
    fn send(&self, bytes: Vec<u8>) {
        let _ = self.pty.send(Msg::Input(bytes.into()));
    }

    fn text(&self) -> String {
        self.editor.with_buffer(|b| b.lines.iter().map(|l| l.text()).collect::<Vec<_>>().join("\n"))
    }

    fn set_text(&mut self, fs: &mut FontSystem, t: &str) {
        self.editor.with_buffer_mut(|b| b.set_text(t, &mono(), Shaping::Advanced, None));
        self.editor.set_selection(Selection::None);
        self.editor.action(fs, Action::Motion(Motion::BufferEnd));
    }

    /// Runs an editing action, saving an undo point first (consecutive word chars share one).
    fn edit(&mut self, fs: &mut FontSystem, action: Action) {
        let word_char = matches!(action, Action::Insert(c) if c.is_alphanumeric());
        if !(word_char && self.typing) {
            let t = self.text();
            if self.undo.last() != Some(&t) {
                self.undo.push(t);
            }
        }
        self.typing = word_char;
        self.editor.action(fs, action);
    }

    fn motion(&mut self, fs: &mut FontSystem, m: Motion, select: bool) {
        if select {
            if self.editor.selection() == Selection::None {
                let c = self.editor.cursor();
                self.editor.set_selection(Selection::Normal(c));
            }
        } else {
            self.editor.set_selection(Selection::None);
        }
        self.typing = false;
        self.editor.action(fs, Action::Motion(m));
    }

    fn cursor_at_end(&self) -> bool {
        let c = self.editor.cursor();
        self.editor.with_buffer(|b| c.line + 1 == b.lines.len() && c.index == b.lines[c.line].text().len())
    }

    /// Re-reads the grid into the text buffer, cell backgrounds and block headers.
    fn build_term(&mut self, fs: &mut FontSystem, cw: f32) {
        let mut text = String::new();
        let mut row_starts = Vec::new();
        let mut spans: Vec<(usize, usize, Rgb, bool)> = Vec::new();
        let mut prompt_rows = Vec::new();
        // Chars missing from the mono font come from fallback fonts whose advance cosmic-text rounds
        // to whole cells (✔ takes 2), shifting the rest of the row. They are drawn apart, at their column.
        let mono_font = fs.db().faces().find(|f| f.families.iter().any(|(n, _)| n == "Cascadia Mono")).map(|f| f.id);
        let mono_font = mono_font.and_then(|id| fs.get_font(id, Weight::NORMAL));
        let in_mono = |c: char| c.is_ascii() || mono_font.as_ref().is_none_or(|f| f.as_swash().charmap().map(c) != 0);
        let mut foreign: Vec<(usize, i32, String, Rgb, usize)> = Vec::new();
        self.glyphs.clear();
        self.cell_bgs.clear();
        self.headers.clear();
        {
            let term = self.term.lock();
            let content = term.renderable_content();
            let offset = content.display_offset as i32;
            let colors = content.colors;
            let mut row = None;
            for cell in content.display_iter {
                let r = cell.point.line.0 + offset;
                if row != Some(r) {
                    if row.is_some() {
                        text.push('\n');
                        // The newline must belong to a span, or the two rows render as one.
                        if let Some(s) = spans.last_mut() {
                            s.1 = text.len();
                        }
                    }
                    row_starts.push(text.len());
                    row = Some(r);
                }
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let (mut fg, mut bg) = (resolve(cell.fg, colors), resolve(cell.bg, colors));
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if content.selection.is_some_and(|s| s.contains(cell.point)) {
                    self.cell_bgs.push((cell.point.column.0, r, rgb(SELECTION)));
                } else if bg != rgb(BG) {
                    self.cell_bgs.push((cell.point.column.0, r, bg));
                }
                if cell.point.column.0 == 0 && cell.c == '>' && fg == rgb(ACCENT) {
                    prompt_rows.push(row_starts.len() - 1);
                }
                let bold = cell.flags.contains(Flags::BOLD);
                let start = text.len();
                if cell.flags.contains(Flags::HIDDEN) {
                    text.push(' ');
                } else if in_mono(cell.c) {
                    text.push(cell.c);
                    if let Some(z) = cell.zerowidth() {
                        text.extend(z);
                    }
                } else {
                    let cells = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
                    let s: String = std::iter::once(cell.c).chain(cell.zerowidth().into_iter().flatten().copied()).collect();
                    foreign.push((cell.point.column.0, r, s, fg, cells));
                    text.push_str(&" ".repeat(cells));
                }
                match spans.last_mut() {
                    Some(s) if s.2 == fg && s.3 == bold => s.1 = text.len(),
                    _ => spans.push((start, text.len(), fg, bold)),
                }
            }
            let c = content.cursor;
            let cursor_row = c.point.line.0 + offset;
            self.term_cursor = (c.shape != CursorShape::Hidden).then_some((c.point.column.0, cursor_row));
            let alt = content.mode.contains(TermMode::ALT_SCREEN);
            // Output sits right above the editor: push it down by the empty rows under the cursor.
            // While waiting, the cursor row is just the "> " prompt: push it under the card.
            let waiting = (self.idle && self.integrated) as i32;
            self.shift = if alt { 0 } else { (term.screen_lines() as i32 - 1 - cursor_row + waiting).max(0) as usize };
            // Headers are found by content, not by remembered rows: those drift (ConPTY repaints, `cls`).
            // A header row starts with our prompt, "> " in ACCENT, followed by a command.
            let history = term.history_size() as i64;
            // The prompt where the cursor waits is the next command, not a block yet.
            let waiting_row = (self.idle as i32 == 1).then_some(cursor_row);
            for r in prompt_rows.into_iter().filter(|&r| !alt && waiting_row != Some(r as i32)) {
                let line = text[row_starts[r]..].split('\n').next().unwrap_or("");
                let cmd = line.get(2..).unwrap_or("").trim();
                if cmd.is_empty() {
                    continue;
                }
                // Time of the run: the sent block with this command nearest to the row.
                let abs = r as i64 - offset as i64 + history;
                let time = self.blocks.iter().filter(|b| b.1 == cmd).min_by_key(|b| (b.0 as i64 - abs).abs()).map_or("", |b| b.2.as_str());
                let metrics = self.term_buf.metrics();
                self.headers.push((r as i32, label_buf(fs, metrics, time, MUTED, false)));
            }
        }
        let attrs = mono();
        self.term_buf.set_rich_text(
            spans.iter().map(|&(a, b, c, bold)| {
                let at = attrs.clone().color(Color::rgb(c.r, c.g, c.b));
                (&text[a..b], if bold { at.weight(Weight::BOLD) } else { at })
            }),
            &attrs,
            Shaping::Advanced,
            None,
        );
        self.term_buf.shape_until_scroll(fs, false);
        let metrics = self.term_buf.metrics();
        for (col, row, s, fg, cells) in foreign {
            let room = cells as f32 * cw;
            let mut b = Buffer::new(fs, metrics);
            let attrs = mono().color(Color::rgb(fg.r, fg.g, fg.b));
            b.set_text(&s, &attrs, Shaping::Advanced, None);
            b.shape_until_scroll(fs, false);
            // Too wide for its cells: shrink it. The advance includes wide side bearings, so it may
            // overhang half a cell before the ink touches its neighbours.
            if line_w(&b) > room * 1.5 {
                let size = metrics.font_size * room * 1.5 / line_w(&b);
                b.set_metrics(Metrics::new(size, metrics.line_height));
                b.shape_until_scroll(fs, false);
            }
            self.glyphs.push((col as f32 * cw + (room - line_w(&b)) / 2.0, row, b));
        }
        self.term_dirty = false;
    }
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    rect_pipeline: wgpu::RenderPipeline,
    font_system: FontSystem,
    swash: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text: TextRenderer,
}

struct State {
    gpu: Gpu,
    proxy: EventLoopProxy<Ev>,
    tabs: Vec<Tab>,
    active: usize,
    next_id: usize,
    shell_args: Vec<String>,
    history: Vec<String>,
    metrics: Metrics,
    chip_metrics: Metrics,
    plus: Buffer,
    close: Buffer,
    hover: Option<usize>,
    ghost_buf: Buffer,
    ghost: String,
    rects: Vec<f32>,
    // Geometry of the last frame, for mouse hit testing and text placement.
    card_top: f32,
    editor_origin: (f32, f32),
    chip_x: Vec<f32>,
    ghost_pos: (f32, f32),
    cell: (f32, f32),
    scale: f32,
    mods: ModifiersState,
    mouse: (f32, f32),
    dragging: bool,
    // Dragging a selection over the terminal output.
    selecting: bool,
    term_top: f32,
    last_click: Option<Instant>,
    focused: bool,
    blink_on: bool,
    next_blink: Instant,
    clipboard: Option<arboard::Clipboard>,
    // Dropped last: the surface must go before the window.
    window: Arc<Window>,
}

// Rounded rectangles: each vertex carries its rect (pixels) and radius; the fragment shader
// cuts the corners with a signed distance, antialiased over one pixel.
const RECT_SHADER: &str = r"
struct V {
    @builtin(position) p: vec4<f32>,
    @location(0) c: vec4<f32>,
    @location(1) @interpolate(flat) r: vec4<f32>,
    @location(2) @interpolate(flat) rad: f32,
};
@vertex fn vs(@location(0) p: vec2<f32>, @location(1) c: vec4<f32>, @location(2) r: vec4<f32>, @location(3) rad: f32) -> V {
    var o: V; o.p = vec4<f32>(p, 0.0, 1.0); o.c = c; o.r = r; o.rad = rad; return o;
}
@fragment fn fs(i: V) -> @location(0) vec4<f32> {
    let half = i.r.zw * 0.5;
    let q = abs(i.p.xy - (i.r.xy + half)) - half + vec2<f32>(i.rad);
    let d = length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - i.rad;
    return vec4<f32>(i.c.rgb, i.c.a * clamp(0.5 - d, 0.0, 1.0));
}
";
const RECT_FLOATS: usize = 11;

fn linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

impl State {
    fn new(event_loop: &ActiveEventLoop, proxy: EventLoopProxy<Ev>) -> Self {
        let window = Arc::new(
            event_loop
                .create_window(
                    Window::default_attributes()
                        .with_title("zhur")
                        .with_theme(Some(Theme::Dark))
                        .with_inner_size(winit::dpi::LogicalSize::new(1100.0, 700.0)),
                )
                .expect("window"),
        );
        let mut gpu = pollster::block_on(Gpu::new(window.clone(), event_loop));
        let scale = window.scale_factor() as f32;
        let metrics = Metrics::new(FONT * scale, (LINE * scale).round());
        let chip_metrics = Metrics::new(CHIP_FONT * scale, (CHIP_LINE * scale).round());
        let cell = (measure_cell(&mut gpu.font_system, metrics), metrics.line_height);
        let plus = icon_buf(&mut gpu.font_system, (14.0 * scale).round(), "\u{E710}", MUTED);
        let close = icon_buf(&mut gpu.font_system, (8.0 * scale).round(), "\u{E711}", FG);
        let ghost_buf = Buffer::new(&mut gpu.font_system, metrics);
        let history = load_history();
        let mut s = State {
            gpu,
            proxy,
            tabs: Vec::new(),
            active: 0,
            next_id: 0,
            shell_args: std::env::args().skip(1).collect(),
            history,
            metrics,
            chip_metrics,
            plus,
            close,
            hover: None,
            ghost_buf,
            ghost: String::new(),
            rects: Vec::new(),
            card_top: 0.0,
            editor_origin: (0.0, 0.0),
            chip_x: Vec::new(),
            ghost_pos: (0.0, 0.0),
            cell,
            scale,
            mods: ModifiersState::empty(),
            mouse: (0.0, 0.0),
            dragging: false,
            selecting: false,
            term_top: 0.0,
            last_click: None,
            focused: true,
            blink_on: true,
            next_blink: Instant::now() + BLINK,
            clipboard: arboard::Clipboard::new().ok(),
            window,
        };
        s.new_tab();
        s
    }

    fn px(&self, v: f32) -> f32 {
        (v * self.scale).round()
    }

    fn tab(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    fn tab_index(&self, id: usize) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    fn new_tab(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        let (program, args, integrated, kind) = shell_cmd(self.shell_args.clone());
        let wslenv = match std::env::var("WSLENV") {
            Ok(v) if !v.is_empty() => format!("{v}:PROMPT_COMMAND/u"),
            _ => "PROMPT_COMMAND/u".into(),
        };
        let opts = tty::Options {
            shell: Some(tty::Shell::new(program, args)),
            // Every tab starts in the user's home, like Warp.
            working_directory: std::env::var_os("USERPROFILE").map(Into::into),
            drain_on_exit: false,
            // WSLENV forwards PROMPT_COMMAND into WSL.
            env: HashMap::from([
                ("PROMPT_COMMAND".into(), BASH_HOOK.into()),
                ("PROMPT".into(), CMD_HOOK.into()),
                ("WSLENV".into(), wslenv),
            ]),
            escape_args: true,
        };
        let (rows, cols) = self.grid_size();
        let proxy = Proxy(self.proxy.clone(), id);
        let term = Arc::new(FairMutex::new(Term::new(term::Config::default(), &Size(rows, cols), proxy.clone())));
        let pty = tty::new(&opts, window_size(rows, cols, self.cell), 0).expect("no se pudo abrir el shell");
        let pty_loop = PtyLoop::new(term.clone(), proxy, pty, false, false).expect("pty loop");
        let sender = pty_loop.channel();
        pty_loop.spawn();

        let fs = &mut self.gpu.font_system;
        let mut term_buf = Buffer::new(fs, self.metrics);
        term_buf.set_monospace_width(Some(self.cell.0));
        let mut editor = Editor::new(Buffer::new(fs, self.metrics));
        editor.with_buffer_mut(|b| b.set_monospace_width(Some(self.cell.0)));
        let (icon, _, fg) = kind.badge();
        let badge = icon_buf(fs, (ICON * self.scale).round(), icon, fg);
        self.tabs.push(Tab {
            id,
            term,
            pty: sender,
            kind,
            shell_label: String::new(),
            badge,
            term_buf,
            chips: Vec::new(),
            editor,
            term_dirty: true,
            cell_bgs: Vec::new(),
            glyphs: Vec::new(),
            term_cursor: None,
            shift: 0,
            blocks: Vec::new(),
            headers: Vec::new(),
            idle: !integrated,
            integrated,
            undo: Vec::new(),
            typing: false,
            hist_pos: self.history.len(),
            cwd: None,
        });
        self.active = self.tabs.len() - 1;
        self.layout();
    }

    /// Closes a tab; returns false when it was the last one.
    fn close_tab(&mut self, i: usize) -> bool {
        let t = self.tabs.remove(i);
        let _ = t.pty.send(Msg::Shutdown);
        self.hover = None;
        if self.tabs.is_empty() {
            return false;
        }
        self.active = self.active.min(self.tabs.len() - 1);
        self.tab().term_dirty = true;
        self.window.request_redraw();
        true
    }

    fn switch(&mut self, i: usize) {
        self.active = i;
        self.tab().term_dirty = true;
        self.dragging = false;
        self.window.request_redraw();
    }

    fn content_left(&self) -> f32 {
        self.px(SIDEBAR) + self.px(MARGIN)
    }

    fn pill_h(&self) -> f32 {
        self.chip_metrics.line_height + self.px(6.0)
    }

    /// Card height for a given number of editor lines.
    fn card_h(&self, lines: usize) -> f32 {
        self.px(CARD_PAD) * 2.0 + self.pill_h() + self.px(8.0) + lines as f32 * self.cell.1
    }

    /// Rows/cols for the area above a one-line input card.
    fn grid_size(&self) -> (usize, usize) {
        let (w, h) = (self.gpu.config.width as f32, self.gpu.config.height as f32);
        let rows = ((h - self.card_h(1) - self.px(MARGIN) * 3.0) / self.cell.1).floor().max(1.0) as usize;
        let cols = ((w - self.content_left() - self.px(MARGIN)) / self.cell.0).floor().max(2.0) as usize;
        (rows, cols)
    }

    fn layout(&mut self) {
        let (rows, cols) = self.grid_size();
        let w = self.gpu.config.width as f32;
        let inner = w - self.content_left() - self.px(MARGIN + CARD_PAD * 2.0);
        let (cell, eh) = (self.cell, self.cell.1 * MAX_EDITOR_LINES as f32);
        for t in &mut self.tabs {
            t.term_buf.set_size(Some(w), Some(rows as f32 * cell.1));
            t.editor.with_buffer_mut(|b| b.set_size(Some(inner), Some(eh)));
            t.term.lock().resize(Size(rows, cols));
            let _ = t.pty.send(Msg::Resize(window_size(rows, cols, cell)));
            t.term_dirty = true;
        }
        self.window.request_redraw();
    }

    fn wake_cursor(&mut self) {
        self.blink_on = true;
        self.next_blink = Instant::now() + BLINK;
    }

    fn on_prompt(&mut self, i: usize, shell: &str, cwd: &str) {
        let kind = Kind::from_name(shell);
        let fs = &mut self.gpu.font_system;
        let t = &mut self.tabs[i];
        t.idle = true;
        t.term_dirty = true;
        t.cwd = win_path(cwd);
        t.shell_label = match shell {
            "ps" => "PowerShell".into(),
            s => s.strip_prefix("wsl:").map_or_else(|| s.to_string(), |d| format!("WSL {d}")),
        };
        if kind != t.kind {
            t.kind = kind;
            let (icon, _, fg) = kind.badge();
            t.badge = icon_buf(fs, (ICON * self.scale).round(), icon, fg);
        }
        let (proxy, id, cwd) = (self.proxy.clone(), t.id, cwd.to_string());
        std::thread::spawn(move || {
            let _ = proxy.send_event(Ev::Info(id, probe(&cwd)));
        });
    }

    fn set_info(&mut self, i: usize, info: Info) {
        let t = &self.tabs[i];
        let mut chips: Vec<(&str, u32, String)> = vec![("❯", t.kind.color(), t.shell_label.clone())];
        if let Some(n) = info.node {
            chips.push(("⬢", NODE, n));
        }
        chips.push(("🗀", ACCENT, info.path));
        if let Some(b) = info.branch {
            chips.push(("⎇", GIT, b));
            chips.push(("±", if info.changes > 0 { DIRTY } else { MUTED }, info.changes.to_string()));
        }
        // One buffer per chip: spaces next to fallback-font icons shape to ~0 width, so gaps can't be text.
        let attrs = mono();
        let fs = &mut self.gpu.font_system;
        self.tabs[i].chips = chips
            .iter()
            .map(|(icon, c, label)| {
                let mut b = Buffer::new(fs, self.chip_metrics);
                let label = format!(" {label}");
                b.set_rich_text(
                    [(*icon, attrs.clone().color(color(*c))), (label.as_str(), attrs.clone().color(color(0xC8CCD4)))],
                    &attrs,
                    Shaping::Advanced,
                    None,
                );
                b.shape_until_scroll(fs, false);
                b
            })
            .collect();
        self.window.request_redraw();
    }

    fn submit(&mut self) {
        let State { gpu, tabs, active, history, .. } = self;
        let t = &mut tabs[*active];
        let cmd = t.text();
        if cmd.trim().is_empty() {
            return;
        }
        if history.last() != Some(&cmd) {
            history.push(cmd.clone());
        }
        t.hist_pos = history.len();
        t.set_text(&mut gpu.font_system, "");
        t.undo.clear();
        {
            let mut term = t.term.lock();
            term.scroll_display(Scroll::Bottom);
            // The prompt is invisible, so the shell echoes the command where the cursor is now.
            let abs = term.history_size() + term.grid().cursor.point.line.0.max(0) as usize;
            t.blocks.push((abs, cmd.lines().next().unwrap_or("").trim().to_string(), local_time()));
        }
        // Each line goes as if typed and followed by Enter.
        t.send(format!("{}\r", cmd.replace('\n', "\r")).into_bytes());
        t.idle = !t.integrated;
        t.term_dirty = true;
    }

    /// Copies and clears the output selection; false when there is none.
    fn copy_output(&mut self) -> bool {
        let text = {
            let mut term = self.tab().term.lock();
            let text = term.selection_to_string().filter(|s| !s.is_empty());
            term.selection = None;
            text
        };
        let Some(text) = text else { return false };
        self.tab().term_dirty = true;
        if let Some(cb) = self.clipboard.as_mut() {
            let _ = cb.set_text(text);
        }
        true
    }

    fn suggestion(&self) -> Option<String> {
        let t = &self.tabs[self.active];
        if !t.idle || !t.cursor_at_end() {
            return None;
        }
        suggest(&self.history, &t.text()).map(str::to_string)
    }

    fn editor_key(&mut self, ev: &KeyEvent) {
        let (ctrl, shift) = (self.mods.control_key(), self.mods.shift_key());
        let suggestion = self.suggestion();
        if let Key::Named(NamedKey::Enter) = ev.logical_key {
            if !shift {
                return self.submit();
            }
        }
        let State { gpu, tabs, active, history, clipboard, .. } = self;
        let fs = &mut gpu.font_system;
        let t = &mut tabs[*active];
        let single_line = !t.text().contains('\n');
        // Tab completes a path first: history suggestions may come from another shell (`Documents\` from
        // PowerShell is a line continuation in bash).
        let sep = if matches!(t.kind, Kind::Bash | Kind::Wsl) { '/' } else { '\\' };
        let path = (ev.logical_key == Key::Named(NamedKey::Tab))
            .then(|| t.cwd.as_deref().filter(|_| t.cursor_at_end()).and_then(|c| complete_path(&t.text(), Path::new(c), sep)))
            .flatten();
        if let Some(new) = path {
            let old = t.text();
            t.undo.push(old);
            return t.set_text(fs, &new);
        }
        if let Key::Named(n) = &ev.logical_key {
            let word = |w, c| if ctrl { w } else { c };
            match n {
                NamedKey::Enter => t.edit(fs, Action::Enter),
                NamedKey::Backspace => t.edit(fs, Action::Backspace),
                NamedKey::Delete => t.edit(fs, Action::Delete),
                // → or Tab at the end of the line accepts the suggestion.
                NamedKey::ArrowRight | NamedKey::Tab if suggestion.is_some() => {
                    t.edit(fs, Action::Motion(Motion::BufferEnd));
                    t.editor.insert_string(&suggestion.unwrap(), None);
                }
                NamedKey::ArrowLeft => t.motion(fs, word(Motion::LeftWord, Motion::Left), shift),
                NamedKey::ArrowRight => t.motion(fs, word(Motion::RightWord, Motion::Right), shift),
                NamedKey::Home => t.motion(fs, word(Motion::BufferStart, Motion::Home), shift),
                NamedKey::End => t.motion(fs, word(Motion::BufferEnd, Motion::End), shift),
                NamedKey::ArrowUp if single_line && t.hist_pos > 0 => {
                    t.hist_pos -= 1;
                    let h = history[t.hist_pos].clone();
                    t.set_text(fs, &h);
                }
                NamedKey::ArrowDown if single_line && t.hist_pos < history.len() => {
                    t.hist_pos += 1;
                    let h = history.get(t.hist_pos).cloned().unwrap_or_default();
                    t.set_text(fs, &h);
                }
                NamedKey::ArrowUp => t.motion(fs, Motion::Up, shift),
                NamedKey::ArrowDown => t.motion(fs, Motion::Down, shift),
                NamedKey::Escape => t.editor.set_selection(Selection::None),
                NamedKey::Tab => {}
                _ => insert_text(t, fs, ev),
            }
            return;
        }
        if !ctrl {
            return insert_text(t, fs, ev);
        }
        let Key::Character(s) = &ev.logical_key else { return };
        match s.to_lowercase().as_str() {
            "a" => {
                t.editor.action(fs, Action::Motion(Motion::BufferStart));
                let c = t.editor.cursor();
                t.editor.set_selection(Selection::Normal(c));
                t.editor.action(fs, Action::Motion(Motion::BufferEnd));
            }
            k @ ("c" | "x") => {
                if let (Some(sel), Some(cb)) = (t.editor.copy_selection(), clipboard.as_mut()) {
                    let _ = cb.set_text(sel);
                }
                if k == "x" {
                    let old = t.text();
                    t.undo.push(old);
                    t.editor.delete_selection();
                }
            }
            "v" => {
                if let Some(p) = clipboard.as_mut().and_then(|cb| cb.get_text().ok()) {
                    let old = t.text();
                    t.undo.push(old);
                    t.editor.delete_selection();
                    t.editor.insert_string(&p.replace("\r\n", "\n"), None);
                }
            }
            "z" => {
                if let Some(old) = t.undo.pop() {
                    t.set_text(fs, &old);
                }
                t.typing = false;
            }
            _ => {}
        }
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: u32, alpha: f32, radius: f32) {
        let (sw, sh) = (self.gpu.config.width as f32, self.gpu.config.height as f32);
        let (x0, y0) = (x / sw * 2.0 - 1.0, 1.0 - y / sh * 2.0);
        let (x1, y1) = ((x + w) / sw * 2.0 - 1.0, 1.0 - (y + h) / sh * 2.0);
        let c = rgb(c);
        let v = [linear(c.r), linear(c.g), linear(c.b), alpha, x, y, w, h, radius.min(w / 2.0).min(h / 2.0)];
        for (px, py) in [(x0, y0), (x1, y0), (x0, y1), (x0, y1), (x1, y0), (x1, y1)] {
            self.rects.extend([px, py]);
            self.rects.extend(v);
        }
    }

    /// Y of the i-th sidebar slot (tabs, then the + button).
    fn slot_y(&self, i: usize) -> f32 {
        self.px(MARGIN) + i as f32 * self.slot_pitch()
    }

    fn slot_pitch(&self) -> f32 {
        self.px(BADGE) + self.px(8.0)
    }

    /// Builds this frame's rectangles; returns the terminal text's top y.
    fn compose(&mut self) -> f32 {
        self.rects.clear();
        let (w, h) = (self.gpu.config.width as f32, self.gpu.config.height as f32);
        let (cw, ch) = self.cell;
        let (m, pad, left) = (self.px(MARGIN), self.px(CARD_PAD), self.content_left());
        let sb = self.px(SIDEBAR);

        // Sidebar: one badge per tab, the active one ringed.
        self.rect(0.0, 0.0, sb, h, SIDEBAR_BG, 1.0, 0.0);
        self.rect(sb - 1.0, 0.0, 1.0, h, BORDER, 1.0, 0.0);
        let (bs, bx) = (self.px(BADGE), (sb - self.px(BADGE)) / 2.0);
        let badges: Vec<(u32, bool)> =
            self.tabs.iter().enumerate().map(|(i, t)| (t.kind.badge().1, i == self.active)).collect();
        for (i, (bg, active)) in badges.into_iter().enumerate() {
            let y = self.slot_y(i);
            if active {
                let r = self.px(2.0);
                self.rect(bx - r, y - r, bs + 2.0 * r, bs + 2.0 * r, ACCENT, 1.0, self.px(10.0));
            }
            self.rect(bx, y, bs, bs, bg, if active { 1.0 } else { 0.55 }, self.px(8.0));
        }
        if let Some((x, y, d)) = self.hover.map(|i| self.close_box(i)) {
            self.rect(x - 1.0, y - 1.0, d + 2.0, d + 2.0, SIDEBAR_BG, 1.0, d / 2.0 + 1.0);
            self.rect(x, y, d, d, PILL, 1.0, d / 2.0);
        }
        let y = self.slot_y(self.tabs.len());
        self.rect(bx, y, bs, bs, PILL, 1.0, self.px(8.0));

        let fs = &mut self.gpu.font_system;
        let t = &mut self.tabs[self.active];
        if t.term_dirty {
            t.build_term(fs, cw);
        }
        t.editor.shape_as_needed(fs, false);
        let lines = t.editor.with_buffer(|b| b.layout_runs().count()).clamp(1, MAX_EDITOR_LINES);
        let (shift, idle, term_cursor) = (t.shift, t.idle, t.term_cursor);
        let headers: Vec<i32> = t.headers.iter().map(|h| h.0).collect();
        let bgs = t.cell_bgs.clone();
        let card_h = self.card_h(lines);
        let card_top = h - m - card_h;
        // A taller editor covers the oldest rows instead of resizing the shell.
        let term_y = m + shift as f32 * ch - (card_h - self.card_h(1));
        self.card_top = card_top;
        self.term_top = term_y;

        // Command blocks: a band with an accent bar behind the echoed command, and a rule through
        // the blank row the prompt leaves above it.
        for r in headers {
            let y = term_y + r as f32 * ch;
            self.rect(left - self.px(4.0), y, w - left - m + self.px(4.0), ch, BLOCK, 1.0, self.px(4.0));
            self.rect(left - self.px(4.0), y, self.px(3.0), ch, ACCENT, 0.9, self.px(1.5));
            if r > 0 {
                self.rect(left - self.px(4.0), y - (ch / 2.0).round(), w - left - m + self.px(4.0), 1.0, BORDER, 1.0, 0.0);
            }
        }
        for (c, r, bg) in bgs {
            let col = (bg.r as u32) << 16 | (bg.g as u32) << 8 | bg.b as u32;
            self.rect(left + c as f32 * cw, term_y + r as f32 * ch, cw, ch, col, 1.0, 0.0);
        }
        let show_cursor = self.blink_on || !self.focused;
        if let (Some((c, r)), false, true) = (term_cursor, idle, show_cursor) {
            self.rect(left + c as f32 * cw, term_y + r as f32 * ch, cw, ch, ACCENT, 0.7, self.px(2.0));
        }

        // Input card with a 1px border.
        let b = self.px(1.0).max(1.0);
        let radius = self.px(RADIUS);
        self.rect(left - b, card_top - b, w - left - m + 2.0 * b, card_h + 2.0 * b, BORDER, 1.0, radius + b);
        self.rect(left, card_top, w - left - m, card_h, CARD, 1.0, radius);

        // Chip pills, sized from each chip's laid-out text.
        let (pill_h, inset, gap) = (self.pill_h(), self.px(9.0), self.px(6.0));
        let widths: Vec<f32> = self.tabs[self.active].chips.iter().map(line_w).collect();
        let mut x = left + pad;
        self.chip_x.clear();
        for tw in widths {
            self.rect(x, card_top + pad, tw + inset * 2.0, pill_h, PILL, 1.0, pill_h / 2.0);
            self.chip_x.push(x + inset);
            x += tw + inset * 2.0 + gap;
        }

        let origin = (left + pad, card_top + pad + pill_h + self.px(8.0));
        self.editor_origin = origin;
        let t = &self.tabs[self.active];
        let mut sel = Vec::new();
        if let Some((a, z)) = t.editor.selection_bounds() {
            t.editor.with_buffer(|buf| {
                for run in buf.layout_runs() {
                    for (x, sw) in run.highlight(a, z) {
                        sel.push((origin.0 + x, origin.1 + run.line_top, sw.max(cw / 2.0), run.line_height));
                    }
                }
            });
        }
        let cursor = t.editor.cursor_position();
        for (x, y, sw, sh) in sel {
            self.rect(x, y, sw, sh, SELECTION, 1.0, self.px(3.0));
        }
        if let (true, Some((x, y))) = (idle, cursor) {
            if show_cursor {
                let cw = self.px(2.0);
                self.rect(origin.0 + x as f32, origin.1 + y as f32 + self.px(2.0), cw, ch - self.px(4.0), ACCENT, 1.0, cw / 2.0);
            }
            self.ghost_pos = (origin.0 + x as f32 + self.px(1.0), origin.1 + y as f32);
        }

        // Inline suggestion from history, shown after the cursor.
        let ghost = self.suggestion().unwrap_or_default();
        if ghost != self.ghost {
            let fs = &mut self.gpu.font_system;
            self.ghost_buf.set_text(&ghost, &mono().color(color(GHOST)), Shaping::Advanced, None);
            self.ghost_buf.set_size(Some(w), Some(ch));
            self.ghost_buf.shape_until_scroll(fs, false);
            self.ghost = ghost;
        }
        term_y
    }

    fn render(&mut self) {
        let term_y = self.compose();
        let (m, pad, left) = (self.px(MARGIN), self.px(CARD_PAD), self.content_left());
        let (w, h) = (self.gpu.config.width, self.gpu.config.height);
        let (card_top, origin, ghost_pos) = (self.card_top, self.editor_origin, self.ghost_pos);
        let chip_y = card_top + pad + self.px(3.0);
        let (bs, bx) = (self.px(BADGE), (self.px(SIDEBAR) - self.px(BADGE)) / 2.0);
        let badge_pos: Vec<(f32, f32)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| (bx + (bs - line_w(&t.badge)) / 2.0, self.slot_y(i) + (bs - t.badge.metrics().line_height) / 2.0))
            .collect();
        let plus_pos = (bx + (bs - line_w(&self.plus)) / 2.0, self.slot_y(self.tabs.len()) + (bs - self.plus.metrics().line_height) / 2.0);
        let close_pos = self.hover.map(|i| {
            let (x, y, d) = self.close_box(i);
            (x + (d - line_w(&self.close)) / 2.0, y + (d - self.close.metrics().line_height) / 2.0)
        });
        let t = &self.tabs[self.active];
        let editor_color = color(if t.idle { FG } else { 0x5A5F6B });
        // Clip at the grid's last row, so a row pushed past it (the waiting prompt) stays hidden.
        let term_bottom = (term_y + (self.grid_size().0 as f32 - t.shift as f32) * self.cell.1).min(card_top - m / 2.0);
        let (time_right, ch) = (w as f32 - m - self.px(10.0), self.cell.1);
        let g = &mut self.gpu;
        g.viewport.update(&g.queue, Resolution { width: w, height: h });
        let res = t.editor.with_buffer(|eb| {
            let area = |buffer, left, top, bottom: f32, default_color| TextArea {
                buffer,
                left,
                top,
                scale: 1.0,
                bounds: TextBounds { left: 0, top: 0, right: w as i32, bottom: bottom as i32 },
                default_color,
                custom_glyphs: &[],
            };
            let ghost = (!self.ghost.is_empty()).then(|| area(&self.ghost_buf, ghost_pos.0, ghost_pos.1, h as f32, color(GHOST)));
            g.text.prepare(
                &g.device,
                &g.queue,
                &mut g.font_system,
                &mut g.atlas,
                &g.viewport,
                [
                    area(&t.term_buf, left, term_y, term_bottom, color(FG)),
                    area(eb, origin.0, origin.1, h as f32 - m, editor_color),
                    area(&self.plus, plus_pos.0, plus_pos.1, h as f32, color(MUTED)),
                ]
                .into_iter()
                .chain(ghost)
                .chain(close_pos.map(|(x, y)| area(&self.close, x, y, h as f32, color(FG))))
                .chain(t.glyphs.iter().map(|(x, r, b)| area(b, left + x, term_y + *r as f32 * ch, term_bottom, color(FG))))
                .chain(t.headers.iter().map(|(r, b)| area(b, time_right - line_w(b), term_y + *r as f32 * ch, term_bottom, color(MUTED))))
                .chain(t.chips.iter().zip(&self.chip_x).map(|(b, &x)| area(b, x, chip_y, h as f32, color(MUTED))))
                .chain(self.tabs.iter().zip(&badge_pos).map(|(tb, &(x, y))| area(&tb.badge, x, y, h as f32, color(FG)))),
                &mut g.swash,
            )
        });
        if res.is_err() {
            return;
        }
        let frame = match g.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Suboptimal(_)
            | wgpu::CurrentSurfaceTexture::Lost => {
                g.surface.configure(&g.device, &g.config);
                self.window.request_redraw();
                return;
            }
            _ => return,
        };
        let view = frame.texture.create_view(&Default::default());
        let vbuf = g.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: as_bytes(&self.rects),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let mut enc = g.device.create_command_encoder(&Default::default());
        {
            let bg = rgb(BG);
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: linear(bg.r) as f64,
                            g: linear(bg.g) as f64,
                            b: linear(bg.b) as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if !self.rects.is_empty() {
                pass.set_pipeline(&g.rect_pipeline);
                pass.set_vertex_buffer(0, vbuf.slice(..));
                pass.draw(0..(self.rects.len() / RECT_FLOATS) as u32, 0..1);
            }
            let _ = g.text.render(&g.atlas, &g.viewport, &mut pass);
        }
        g.queue.submit(Some(enc.finish()));
        g.queue.present(frame);
        g.atlas.trim();
    }

    fn editor_point(&self) -> (i32, i32) {
        ((self.mouse.0 - self.editor_origin.0) as i32, (self.mouse.1 - self.editor_origin.1) as i32)
    }

    /// Grid point and cell side under the mouse in the terminal output.
    fn term_point(&mut self) -> (Point, Side) {
        let (cw, ch) = self.cell;
        let x = ((self.mouse.0 - self.content_left()) / cw).max(0.0);
        let row = ((self.mouse.1 - self.term_top) / ch).floor().max(0.0) as i32;
        let term = self.tab().term.lock();
        let (rows, cols) = (term.screen_lines() as i32, term.columns());
        let line = Line(row.min(rows - 1) - term.grid().display_offset() as i32);
        let side = if x.fract() < 0.5 { Side::Left } else { Side::Right };
        (Point::new(line, Column((x as usize).min(cols - 1))), side)
    }

    /// Tab whose badge is under the mouse, when there is more than one (the last one closes with Ctrl+Shift+W).
    fn hovered_tab(&self) -> Option<usize> {
        let i = ((self.mouse.1 - self.px(MARGIN)) / self.slot_pitch()).floor();
        let inside = self.mouse.0 < self.px(SIDEBAR) && i >= 0.0 && self.mouse.1 - self.slot_y(i as usize) < self.px(BADGE);
        (inside && (i as usize) < self.tabs.len() && self.tabs.len() > 1).then_some(i as usize)
    }

    /// The close button over a badge's top-right corner: (x, y, diameter).
    fn close_box(&self, i: usize) -> (f32, f32, f32) {
        let (d, bs) = (self.px(16.0), self.px(BADGE));
        let x = (self.px(SIDEBAR) + bs) / 2.0 - d + self.px(5.0);
        (x, self.slot_y(i) - self.px(5.0), d)
    }

    fn click(&mut self) {
        if let Some(i) = self.hover {
            let (x, y, d) = self.close_box(i);
            if (x..x + d).contains(&self.mouse.0) && (y..y + d).contains(&self.mouse.1) {
                self.close_tab(i);
                self.hover = self.hovered_tab();
                self.window.request_redraw();
                return;
            }
        }
        if self.mouse.0 < self.px(SIDEBAR) {
            let slot = ((self.mouse.1 - self.px(MARGIN)) / self.slot_pitch()).floor();
            match slot as usize {
                _ if slot < 0.0 => {}
                i if i < self.tabs.len() => self.switch(i),
                i if i == self.tabs.len() => self.new_tab(),
                _ => {}
            }
            return;
        }
        if self.mouse.1 < self.card_top {
            let (p, side) = self.term_point();
            self.tab().term.lock().selection = Some(TermSel::new(SelectionType::Simple, p, side));
            self.tab().term_dirty = true;
            self.selecting = true;
            self.window.request_redraw();
            return;
        }
        // A click in the editor drops the output selection, so Ctrl+C copies the editor again.
        self.tab().term.lock().selection = None;
        self.tab().term_dirty = true;
        if !self.tabs[self.active].idle {
            return;
        }
        let (x, y) = self.editor_point();
        let double = self.last_click.is_some_and(|t| t.elapsed().as_millis() < 400);
        let a = if double { Action::DoubleClick { x, y } } else { Action::Click { x, y } };
        let State { gpu, tabs, active, .. } = self;
        tabs[*active].editor.action(&mut gpu.font_system, a);
        tabs[*active].typing = false;
        self.last_click = Some(Instant::now());
        self.dragging = !double;
        self.wake_cursor();
        self.window.request_redraw();
    }
}

fn insert_text(t: &mut Tab, fs: &mut FontSystem, ev: &KeyEvent) {
    if let Some(text) = &ev.text {
        for c in text.chars().filter(|c| !c.is_control()) {
            t.edit(fs, Action::Insert(c));
        }
    }
}

fn as_bytes(v: &[f32]) -> &[u8] {
    // SAFETY: f32 has no padding and any byte pattern is a valid u8.
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

fn measure_cell(fs: &mut FontSystem, metrics: Metrics) -> f32 {
    let mut b = Buffer::new(fs, metrics);
    b.set_text("M", &mono(), Shaping::Advanced, None);
    b.shape_until_scroll(fs, false);
    b.layout_runs().next().and_then(|r| r.glyphs.first()).map(|g| g.w).unwrap_or(metrics.font_size * 0.55)
}

struct Size(usize, usize);
impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.0
    }
    fn screen_lines(&self) -> usize {
        self.0
    }
    fn columns(&self) -> usize {
        self.1
    }
}

fn window_size(rows: usize, cols: usize, cell: (f32, f32)) -> WindowSize {
    WindowSize { num_lines: rows as u16, num_cols: cols as u16, cell_width: cell.0 as u16, cell_height: cell.1 as u16 }
}

impl Gpu {
    async fn new(window: Arc<Window>, event_loop: &ActiveEventLoop) -> Self {
        let size = window.inner_size();
        let mut desc = wgpu::InstanceDescriptor::new_with_display_handle(Box::new(event_loop.owned_display_handle()));
        // DX12 only: also initializing Vulkan/GL drivers cost ~150 MB of RAM.
        desc.backends = wgpu::Backends::DX12;
        let instance = wgpu::Instance::new(desc);
        let surface = instance.create_surface(window).expect("surface");
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions { compatible_surface: Some(&surface), ..Default::default() })
            .await
            .expect("no GPU adapter");
        let (device, queue) = adapter.request_device(&Default::default()).await.expect("device");
        let format = wgpu::TextureFormat::Bgra8UnormSrgb;
        let mailbox = surface.get_capabilities(&adapter).present_modes.contains(&wgpu::PresentMode::Mailbox);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            // Lowest latency without tearing when available.
            present_mode: if mailbox { wgpu::PresentMode::Mailbox } else { wgpu::PresentMode::Fifo },
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            view_formats: vec![],
            desired_maximum_frame_latency: 1,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: None,
            source: wgpu::ShaderSource::Wgsl(RECT_SHADER.into()),
        });
        let rect_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: None,
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (RECT_FLOATS * 4) as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4, 2 => Float32x4, 3 => Float32],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });

        let fonts = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into()) + "\\Fonts\\";
        let mut db = glyphon::fontdb::Database::new();
        for f in FONTS {
            let _ = db.load_font_file(format!("{fonts}{f}"));
        }
        let font_system = FontSystem::new_with_locale_and_db("en-US".into(), db);
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text = TextRenderer::new(&mut atlas, &device, Default::default(), None);
        Gpu { device, queue, surface, config, rect_pipeline, font_system, swash: SwashCache::new(), viewport, atlas, text }
    }
}

struct App {
    proxy: EventLoopProxy<Ev>,
    state: Option<State>,
}

impl ApplicationHandler<Ev> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_none() {
            self.state = Some(State::new(event_loop, self.proxy.clone()));
        }
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        if let (StartCause::ResumeTimeReached { .. }, Some(s)) = (cause, &mut self.state) {
            s.blink_on = !s.blink_on;
            s.next_blink = Instant::now() + BLINK;
            s.window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(s) = &self.state {
            // No timer while unfocused: zero wakeups when the window is in the background.
            event_loop.set_control_flow(if s.focused { ControlFlow::WaitUntil(s.next_blink) } else { ControlFlow::Wait });
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Ev) {
        let Some(s) = &mut self.state else { return };
        let (Ev::Term(id, _) | Ev::Info(id, _)) = &event;
        let Some(i) = s.tab_index(*id) else { return };
        match event {
            Ev::Info(_, info) => s.set_info(i, info),
            Ev::Term(_, TermEvent::Wakeup) => {
                s.tabs[i].term_dirty = true;
                if i == s.active {
                    s.window.request_redraw();
                }
            }
            Ev::Term(_, TermEvent::Title(t)) => {
                if let Some((shell, cwd)) = parse_idle(&t) {
                    s.on_prompt(i, shell, cwd);
                    s.window.request_redraw();
                }
            }
            Ev::Term(_, TermEvent::PtyWrite(t)) => s.tabs[i].send(t.into_bytes()),
            Ev::Term(_, TermEvent::ChildExit(_) | TermEvent::Exit) => {
                if !s.close_tab(i) {
                    event_loop.exit();
                }
            }
            Ev::Term(..) => {}
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(s) = &mut self.state else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                s.gpu.config.width = size.width.max(1);
                s.gpu.config.height = size.height.max(1);
                s.gpu.surface.configure(&s.gpu.device, &s.gpu.config);
                s.layout();
            }
            WindowEvent::Focused(f) => {
                s.focused = f;
                s.wake_cursor();
                s.window.request_redraw();
            }
            WindowEvent::RedrawRequested => s.render(),
            WindowEvent::ModifiersChanged(m) => s.mods = m.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let (ctrl, shift) = (s.mods.control_key(), s.mods.shift_key());
                let key = match &event.logical_key {
                    Key::Character(c) => c.to_lowercase(),
                    _ => String::new(),
                };
                // Tab shortcuts work in both modes, so they use Ctrl+Shift (free in shells and vim).
                if ctrl && shift && key == "t" {
                    s.new_tab();
                } else if ctrl && shift && key == "w" {
                    if !s.close_tab(s.active) {
                        event_loop.exit();
                    }
                } else if ctrl && event.logical_key == Key::Named(NamedKey::Tab) {
                    let n = s.tabs.len();
                    s.switch(if shift { (s.active + n - 1) % n } else { (s.active + 1) % n });
                } else if ctrl && key == "c" && s.copy_output() {
                    // Copied the output selection instead of sending Ctrl+C, like Windows Terminal.
                } else if s.tab().idle {
                    s.editor_key(&event);
                } else {
                    let t = s.tab();
                    let app_cursor = t.term.lock().mode().contains(TermMode::APP_CURSOR);
                    if let Some(b) = key_bytes(&event, s.mods, app_cursor) {
                        let t = s.tab();
                        t.term.lock().scroll_display(Scroll::Bottom);
                        t.send(b);
                    }
                }
                s.wake_cursor();
                s.window.request_redraw();
            }
            WindowEvent::CursorLeft { .. } if s.hover.is_some() => {
                s.hover = None;
                s.window.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                s.mouse = (position.x as f32, position.y as f32);
                // The close button only shows while a tab is hovered, and stays while reaching for it.
                let over_close = s.hover.is_some_and(|i| {
                    let (x, y, d) = s.close_box(i);
                    (x..x + d).contains(&s.mouse.0) && (y..y + d).contains(&s.mouse.1)
                });
                let hover = if over_close { s.hover } else { s.hovered_tab() };
                if hover != s.hover {
                    s.hover = hover;
                    s.window.request_redraw();
                }
                if s.selecting {
                    let (p, side) = s.term_point();
                    let t = s.tab();
                    if let Some(sel) = t.term.lock().selection.as_mut() {
                        sel.update(p, side);
                    }
                    t.term_dirty = true;
                    s.window.request_redraw();
                }
                if s.dragging {
                    let (x, y) = s.editor_point();
                    let State { gpu, tabs, active, .. } = s;
                    tabs[*active].editor.action(&mut gpu.font_system, Action::Drag { x, y });
                    s.window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                if state == ElementState::Pressed {
                    s.click();
                } else {
                    s.dragging = false;
                    s.selecting = false;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (y * 3.0) as i32,
                    MouseScrollDelta::PixelDelta(p) => (p.y as f32 / s.cell.1) as i32,
                };
                let t = s.tab();
                t.term.lock().scroll_display(Scroll::Delta(lines));
                t.term_dirty = true;
                s.window.request_redraw();
            }
            _ => {}
        }
    }
}

fn main() {
    let event_loop = EventLoop::<Ev>::with_user_event().build().expect("event loop");
    let proxy = event_loop.create_proxy();
    event_loop.run_app(&mut App { proxy, state: None }).expect("run");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_256() {
        assert_eq!(index256(1), rgb(ANSI[1]));
        assert_eq!(index256(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(index256(231), Rgb { r: 255, g: 255, b: 255 });
        assert_eq!(index256(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(index256(255), Rgb { r: 238, g: 238, b: 238 });
    }

    #[test]
    fn shell_hooks() {
        let (p, a, i, k) = shell_cmd(vec![]);
        assert_eq!((p.as_str(), i, k), ("powershell.exe", true, Kind::Ps));
        assert_eq!(a.last().unwrap(), PS_HOOK);
        let (_, a, i, k) = shell_cmd(vec!["C:\\Windows\\System32\\cmd.exe".into()]);
        assert_eq!((a.len(), i, k), (0, true, Kind::Cmd));
        let (p, a, _, k) = shell_cmd(vec!["C:\\Program Files\\Git\\bin\\bash.exe".into(), "-l".into()]);
        assert_eq!((p.as_str(), a, k), ("\"C:\\Program Files\\Git\\bin\\bash.exe\"", vec!["-l".to_string()], Kind::Bash));
        assert!(!shell_cmd(vec!["nu".into()]).2);
    }

    #[test]
    fn idle_titles() {
        assert_eq!(parse_idle("zhur-idle123?ps?C:\\x"), Some(("ps", "C:\\x")));
        assert_eq!(parse_idle("zhur-idle9?wsl:Ubuntu?/home/a?b"), Some(("wsl:Ubuntu", "/home/a?b")));
        assert_eq!(parse_idle("user@host: ~"), None);
        assert_eq!(Kind::from_name("wsl:Ubuntu"), Kind::Wsl);
    }

    #[test]
    fn suggestions() {
        let h: Vec<String> = ["git status", "git push", "ls"].map(String::from).to_vec();
        assert_eq!(suggest(&h, "git"), Some(" push"));
        assert_eq!(suggest(&h, "git s"), Some("tatus"));
        assert_eq!(suggest(&h, "ls"), None);
        assert_eq!(suggest(&h, ""), None);
    }

    #[test]
    fn path_completion() {
        let d = std::env::temp_dir().join("zhur-complete-test");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(d.join("Program Files")).unwrap();
        std::fs::write(d.join("notes-a.txt"), "").unwrap();
        std::fs::write(d.join("notes-b.txt"), "").unwrap();
        let c = |t| complete_path(t, &d, '\\');
        assert_eq!(c("cd sr").as_deref(), Some("cd src\\"));
        assert_eq!(c("ls ./SR").as_deref(), Some("ls ./src/"));
        assert_eq!(c("cat no").as_deref(), Some("cat notes-"));
        assert_eq!(c("cat notes-"), None);
        assert_eq!(c("cd prog").as_deref(), Some("cd \"Program Files\\"));
        assert_eq!(c("cd \"prog").as_deref(), Some("cd \"Program Files\\"));
        assert_eq!(c("cd zz"), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cwd_to_windows() {
        assert_eq!(win_path("C:\\Users\\x").as_deref(), Some("C:\\Users\\x"));
        assert_eq!(win_path("/c/Users/x").as_deref(), Some("C:\\Users\\x"));
        assert_eq!(win_path("/mnt/d/repo/a").as_deref(), Some("D:\\repo\\a"));
        assert_eq!(win_path("/c").as_deref(), Some("C:\\"));
        assert_eq!(win_path("/home/x"), None);
        assert_eq!(win_path("/"), None);
    }
}
