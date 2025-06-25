use std::time::{SystemTime, UNIX_EPOCH};
use std::sync::atomic::{AtomicBool, Ordering};

// Standard configuration
const CHANGE_WAIT: bool = false; // Change runlevel while waiting for a process to exit?
const INIT_PROGRAM: &str = "/sbin/init";
const VERSION: &str = "0.1.0";

// Debug and test modes
const DEBUG: bool = false;       // Debug code off
const INITDEBUG: bool = false;   // Fork at startup to debug init

// Constants
const INITPID: i32 = 1;          // pid of first process
const PIPE_FD: i32 = 10;         // File number of initfifo
const STATE_PIPE: i32 = 11;      // used to pass state through exec
const WAIT_BETWEEN_SIGNALS: u64 = 3; // default time to wait between TERM and KILL

// Sleep constants in milliseconds
const MINI_SLEEP: u64 = 10;
const SHORT_SLEEP: u64 = 5000;
const LONG_SLEEP: u64 = 30000;

// Failsafe configuration
const MAXSPAWN: u32 = 10;        // Max times respawned in...
const TESTTIME: u64 = 120;       // ...this many seconds
const SLEEPTIME: u64 = 300;      // Disable time

// State parser command constants and structures
const NR_EXTRA_ENV: usize = 16;

// Global atomic signals
static GOT_CONT: AtomicBool = AtomicBool::new(false);
static GOT_SIGNALS: AtomicBool = AtomicBool::new(false);

// Default path inherited by every child
const PATH_DEFAULT: &str = "/sbin:/usr/sbin:/bin:/usr/bin";

// Signature for re-exec fd
const SIGNATURE: &str = "12567362";

// Extra environment variables
pub struct ExtraEnv {
    pub vars: [Option<String>; NR_EXTRA_ENV],
}

impl ExtraEnv {
    pub fn new() -> Self {
        ExtraEnv {
            vars: [None, None, None, None, None, None, None, None,
                None, None, None, None, None, None, None, None],
        }
    }
}


// Actions to be taken by init
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InitAction {
    Respawn = 1,
    Wait = 2,
    Once = 3,
    Boot = 4,
    BootWait = 5,
    PowerFail = 6,
    PowerWait = 7,
    PowerOkWait = 8,
    CtrlAltDel = 9,
    Off = 10,
    OnDemand = 11,
    InitDefault = 12,
    SysInit = 13,
    PowerFailNow = 14,
    KbRequest = 15,
}

impl InitAction {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "respawn" => Some(InitAction::Respawn),
            "wait" => Some(InitAction::Wait),
            "once" => Some(InitAction::Once),
            "boot" => Some(InitAction::Boot),
            "bootwait" => Some(InitAction::BootWait),
            "powerfail" => Some(InitAction::PowerFail),
            "powerwait" => Some(InitAction::PowerWait),
            "powerokwait" => Some(InitAction::PowerOkWait),
            "ctrlaltdel" => Some(InitAction::CtrlAltDel),
            "off" => Some(InitAction::Off),
            "ondemand" => Some(InitAction::OnDemand),
            "initdefault" => Some(InitAction::InitDefault),
            "sysinit" => Some(InitAction::SysInit),
            "powerfailnow" => Some(InitAction::PowerFailNow),
            "kbrequest" => Some(InitAction::KbRequest),
            _ => None,
        }
    }
}

// String length constants
const INITTAB_ID: usize = 8;
const RUNLEVEL_LENGTH: usize = 12;
const ACTION_LENGTH: usize = 33;
const PROCESS_LENGTH: usize = 512;

// Values for the 'flags' field (using bitflags)
bitflags::bitflags! {
    #[derive(Debug, Clone, Copy)]
    pub struct ChildFlags: u32 {
        const RUNNING = 2;      // Process is still running
        const KILLME = 4;       // Kill this process
        const DEMAND = 8;       // "runlevels" a b c
        const FAILING = 16;     // process respawns rapidly
        const WAITING = 32;     // We're waiting for this process
        const ZOMBIE = 64;      // This process is already dead
        const XECUTED = 128;    // Set if spawned once or more times
    }
}

// Log levels
#[derive(Debug, Clone, Copy)]
pub enum LogLevel {
    Console = 1,        // L_CO - Log on the console
    Syslog = 2,         // L_SY - Log with syslog()
    Verbose = 3,        // L_VB - Log with both (L_CO|L_SY)
}

const NO_PROCESS: i32 = 0;

// Information about a process in the in-core inittab
#[derive(Debug, Clone)]
pub struct Child {
    pub flags: ChildFlags,              // Status of this entry
    pub exstat: i32,                    // Exit status of process
    pub pid: i32,                       // Pid of this process
    pub tm: u64,                        // When respawned last (Unix timestamp)
    pub count: u32,                     // Times respawned in the last 2 minutes
    pub id: String,                     // Inittab id (must be unique, max 8 chars)
    pub rlevel: String,                 // run levels (max 12 chars)
    pub action: InitAction,             // what to do
    pub process: String,                // The command line (max 512 chars)
    pub new: Option<Box<Child>>,        // New entry (after inittab re-read)
    pub next: Option<Box<Child>>,       // For the linked list
}

impl Child {
    pub fn new() -> Self {
        Child {
            flags: ChildFlags::empty(),
            exstat: 0,
            pid: NO_PROCESS,
            tm: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
            count: 0,
            id: String::new(),
            rlevel: String::new(),
            action: InitAction::Once,
            process: String::new(),
            new: None,
            next: None,
        }
    }

    pub fn from_inittab_line(line: &str) -> Option<Self> {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() != 4 {
            return None;
        }

        let id = parts[0];
        let runlevels = parts[1];
        let action_str = parts[2];
        let process = parts[3];

        // Validate lengths
        if id.len() > INITTAB_ID ||
            runlevels.len() > RUNLEVEL_LENGTH ||
            action_str.len() > ACTION_LENGTH ||
            process.len() > PROCESS_LENGTH {
            return None;
        }

        let action = InitAction::from_str(action_str)?;

        Some(Child {
            flags: ChildFlags::empty(),
            exstat: 0,
            pid: NO_PROCESS,
            tm: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
            count: 0,
            id: id.to_string(),
            rlevel: runlevels.to_string(),
            action,
            process: process.to_string(),
            new: None,
            next: None,
        })
    }

    pub fn should_run_at_level(&self, level: char) -> bool {
        self.rlevel.contains(level)
    }

    pub fn is_running(&self) -> bool {
        self.flags.contains(ChildFlags::RUNNING)
    }

    pub fn is_failing(&self) -> bool {
        self.flags.contains(ChildFlags::FAILING)
    }

    pub fn mark_running(&mut self) {
        self.flags.insert(ChildFlags::RUNNING);
    }

    pub fn mark_zombie(&mut self) {
        self.flags.remove(ChildFlags::RUNNING);
        self.flags.insert(ChildFlags::ZOMBIE);
    }

    pub fn mark_executed(&mut self) {
        self.flags.insert(ChildFlags::XECUTED);
    }
}

// Tokens in state parser
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StateToken {
    Ver = 1,
    End = 2,
    Rec = 3,
    Eor = 4,
    Lev = 5,
    Flag = 6,
    Action = 7,
    Process = 8,
    Pid = 9,
    Exs = 10,
    Eof = -1,
    Runlevel = -2,
    ThisLevel = -3,
    PrevLevel = -4,
    GotSign = -5,
    WroteWtmpReboot = -6,
    WroteUtmpReboot = -7,
    SlTime = -8,
    DidBoot = -9,
    WroteWtmpRlevel = -16,
    WroteUtmpRlevel = -17,
}

// Global state struct
#[derive(Debug)]
pub struct InitState {
    pub family: Option<Box<Child>>,     // LinkedList of children
    pub new_family: Option<Box<Child>>, // The list after inittab re-read
    pub wrote_wtmp_reboot: bool,
    pub wrote_utmp_reboot: bool,
    pub wrote_wtmp_rlevel: bool,
    pub wrote_utmp_rlevel: bool,
    pub curlevel: char,                 // Current runlevel
    pub prevlevel: char,                // Previous runlevel
    pub dfl_level: char,                // Default runlevel
    pub emerg_shell: bool,              // Start emergency shell?
    pub sleep_time: u64,                // Sleep time between TERM and KILL
    pub console_dev: Option<String>,    // Console device
    pub pipe_fd: i32,                   // /run/initctl
    pub did_boot: bool,                 // Is BOOT* done?
    pub reload: bool,                   // Should we do initialization stuff?
    pub myname: String,                 // What should we exec
    pub oops_error: i32,                // Used be re-exec. May be refactored out later
}

impl InitState {
    pub fn new() -> Self {
        InitState {
            family: None,
            new_family: None,
            wrote_wtmp_reboot: true,
            wrote_utmp_reboot: true,
            wrote_wtmp_rlevel: true,
            wrote_utmp_rlevel: true,
            curlevel: 'S',   // single-user mode
            prevlevel: 'N',  // no previous runlevel
            dfl_level: '0',  // Default runlevel
            emerg_shell: false,
            sleep_time: WAIT_BETWEEN_SIGNALS,
            console_dev: None,
            pipe_fd: -1,
            did_boot: false,
            reload: false,
            myname: INIT_PROGRAM.to_string(),
            oops_error: 0,
        }
    }

    // New children are added to the start of the list
    pub fn add_child(&mut self, mut child: Child) {
        child.next = self.family.take();
        self.family = Some(Box::new(child));
    }

    pub fn find_child_by_id(&self, id: &str) -> Option<&Child> {
        let mut current = self.family.as_ref();
        while let Some(child) = current {
            if child.id == id {
                return Some(child);
            }
            current = child.next.as_ref();
        }
        None
    }

    pub fn find_child_by_pid(&self, pid: i32) -> Option<&Child> {
        let mut current = self.family.as_ref();
        while let Some(child) = current {
            if child.pid == pid {
                return Some(child);
            }
            current = child.next.as_ref();
        }
        None
    }

    pub fn remove_child_by_pid(&mut self, pid: i32) -> Option<Child> {
        let mut current = &mut self.family;
        while let Some(child) = current {
            if child.pid == pid {
                let mut removed = current.take().unwrap();
                *current = removed.next.take();
                return Some(*removed);
            }
            current = &mut current.as_mut().unwrap().next;
        }
        None
    }
}

// Command lookup table for state parser
struct StateCommand {
    name: &'static str,
    cmd: StateToken,
}

const STATE_COMMANDS: &[StateCommand] = &[
    StateCommand { name: "VER", cmd: StateToken::Ver },
    StateCommand { name: "END", cmd: StateToken::End },
    StateCommand { name: "REC", cmd: StateToken::Rec },
    StateCommand { name: "EOR", cmd: StateToken::Eor },
    StateCommand { name: "LEV", cmd: StateToken::Lev },
    StateCommand { name: "FL ", cmd: StateToken::Flag },
    StateCommand { name: "AC ", cmd: StateToken::Action },
    StateCommand { name: "CMD", cmd: StateToken::Process },
    StateCommand { name: "PID", cmd: StateToken::Pid },
    StateCommand { name: "EXS", cmd: StateToken::Exs },
    StateCommand { name: "-RL", cmd: StateToken::Runlevel },
    StateCommand { name: "-TL", cmd: StateToken::ThisLevel },
    StateCommand { name: "-PL", cmd: StateToken::PrevLevel },
    StateCommand { name: "-SI", cmd: StateToken::GotSign },
    StateCommand { name: "-WR", cmd: StateToken::WroteWtmpReboot },
    StateCommand { name: "-WU", cmd: StateToken::WroteUtmpReboot },
    StateCommand { name: "-ST", cmd: StateToken::SlTime },
    StateCommand { name: "-DB", cmd: StateToken::DidBoot },
    StateCommand { name: "-LW", cmd: StateToken::WroteWtmpRlevel },
    StateCommand { name: "-LU", cmd: StateToken::WroteUtmpRlevel },
];

// Flag lookup table
struct FlagMapping {
    name: &'static str,
    mask: ChildFlags,
}

const FLAG_MAPPINGS: &[FlagMapping] = &[
    FlagMapping { name: "RU", mask: ChildFlags::RUNNING },
    FlagMapping { name: "DE", mask: ChildFlags::DEMAND },
    FlagMapping { name: "XD", mask: ChildFlags::XECUTED },
    FlagMapping { name: "WT", mask: ChildFlags::WAITING },
];

// FreeBSD specific code
#[cfg(target_os = "freebsd")]
mod freebsd_compat {
    const UTMP_FILE: &str = "/var/run/utmp";
    const RUN_LVL: i32 = 1;

    #[repr(C)]
    pub struct Utmp {
        pub ut_id: [u8; 4],
    }
}

// TODO: Implement prototypes
pub trait InitLogger {
    fn initlog(&self, level: LogLevel, msg: &str);
}

pub trait UtmpWriter {
    fn write_utmp_wtmp(&self, user: &str, id: &str, pid: i32, entry_type: i32, line: &str);
    fn write_wtmp(&self, user: &str, id: &str, pid: i32, entry_type: i32, line: &str);
}

pub trait TerminalController {
    fn set_term(&self, how: i32);
    fn print(&self, msg: &str);
}

pub trait WallMessenger {
    fn wall(&self, text: &str, remote: bool);
}

macro_rules! initdbg {
    ($level:expr, $fmt:expr $(, $args:expr)*) => {
        if DEBUG {
            // TODO: Call initlog
            eprintln!($fmt $(, $args)*);
        }
    };
}

// Signal handler helpers
pub fn set_got_cont() {
    GOT_CONT.store(true, Ordering::Relaxed);
}

pub fn got_cont() -> bool {
    GOT_CONT.load(Ordering::Relaxed)
}

pub fn clear_got_cont() {
    GOT_CONT.store(false, Ordering::Relaxed);
}

pub fn set_got_signals() {
    GOT_SIGNALS.store(true, Ordering::Relaxed);
}

pub fn got_signals() -> bool {
    GOT_SIGNALS.load(Ordering::Relaxed)
}

pub fn clear_got_signals() {
    GOT_SIGNALS.store(false, Ordering::Relaxed);
}

pub fn is_valid_runlevel(c: char) -> bool {
    matches!(c, '0'..='6' | 'S' | 's' | 'A'..='C' | 'a'..='c')
}

pub fn normalize_runlevel(c: char) -> char {
    match c {
        's' => 'S',
        'a' => 'A',
        'b' => 'B',
        'c' => 'C',
        _ => c,
    }
}

pub fn create_emergency_shell() -> Child {
    Child {
        flags: ChildFlags::WAITING,
        exstat: 0,
        pid: NO_PROCESS,
        tm: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        count: 0,
        id: "~~".to_string(),
        rlevel: "S".to_string(),
        action: InitAction::Once,
        process: "/sbin/sulogin".to_string(),
        new: None,
        next: None,
    }
}


// Poweroff child definition
pub fn create_poweroff_child() -> Child {
    Child {
        flags: ChildFlags::empty(),
        exstat: 0,
        pid: NO_PROCESS,
        tm: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        count: 0,
        id: "~~".to_string(),
        rlevel: "S".to_string(),
        action: InitAction::Once,
        process: "/sbin/shutdown -hP now".to_string(),
        new: None,
        next: None,
    }
}

pub fn is_power_action(action: InitAction) -> bool {
    matches!(action,
        InitAction::PowerWait |
        InitAction::PowerFail |
        InitAction::PowerOkWait |
        InitAction::PowerFailNow |
        InitAction::CtrlAltDel
    )
}

// Sleep function - sleeps for specified milliseconds
pub fn do_msleep(msec: u64) {
    std::thread::sleep(std::time::Duration::from_millis(msec));
}

// Non-failing memory allocation
pub fn imalloc(size: usize) -> Vec<u8> {
    loop {
        match std::panic::catch_unwind(|| vec![0u8; size]) {
            Ok(vec) => return vec,
            Err(_) => {
                eprintln!("out of memory");
                do_msleep(SHORT_SLEEP);
            }
        }
    }
}

// String duplication
pub fn istrdup(s: &str) -> String {
    loop {
        match std::panic::catch_unwind(|| s.to_string()) {
            Ok(string) => return string,
            Err(_) => {
                eprintln!("out of memory");
                do_msleep(SHORT_SLEEP);
            }
        }
    }
}

// Send state information to a file descriptor
pub fn send_state<W: std::io::Write>(mut writer: W, state: &InitState) -> std::io::Result<()> {
    use std::io::Write;

    writeln!(writer, "VER{}", VERSION)?;
    writeln!(writer, "-RL{}", state.curlevel)?;
    writeln!(writer, "-TL{}", state.curlevel)?; // thislevel same as curlevel in our implementation
    writeln!(writer, "-PL{}", state.prevlevel)?;
    writeln!(writer, "-SI{}", if got_signals() { 1 } else { 0 })?;
    writeln!(writer, "-WR{}", if state.wrote_wtmp_reboot { 1 } else { 0 })?;
    writeln!(writer, "-WU{}", if state.wrote_utmp_reboot { 1 } else { 0 })?;
    writeln!(writer, "-ST{}", state.sleep_time)?;
    writeln!(writer, "-DB{}", if state.did_boot { 1 } else { 0 })?;

    // Iterate through family list
    let mut current = state.family.as_ref();
    while let Some(child) = current {
        writeln!(writer, "REC{}", child.id)?;
        writeln!(writer, "LEV{}", child.rlevel)?;

        // Write flags
        for flag_mapping in FLAG_MAPPINGS {
            if child.flags.contains(flag_mapping.mask) {
                writeln!(writer, "FL {}", flag_mapping.name)?;
            }
        }

        writeln!(writer, "PID{}", child.pid)?;
        writeln!(writer, "EXS{}", child.exstat)?;

        // Write action
        let action_name = match child.action {
            InitAction::Respawn => "respawn",
            InitAction::Wait => "wait",
            InitAction::Once => "once",
            InitAction::Boot => "boot",
            InitAction::BootWait => "bootwait",
            InitAction::PowerFail => "powerfail",
            InitAction::PowerWait => "powerwait",
            InitAction::PowerOkWait => "powerokwait",
            InitAction::CtrlAltDel => "ctrlaltdel",
            InitAction::Off => "off",
            InitAction::OnDemand => "ondemand",
            InitAction::InitDefault => "initdefault",
            InitAction::SysInit => "sysinit",
            InitAction::PowerFailNow => "powerfailnow",
            InitAction::KbRequest => "kbrequest",
        };

        writeln!(writer, "AC {}", action_name)?;
        writeln!(writer, "CMD{}", child.process)?;
        writeln!(writer, "EOR")?;

        current = child.next.as_ref();
    }

    writeln!(writer, "END")?;
    Ok(())
}

// Re-implementation of get_string in C
pub fn get_string<R: std::io::Read>(reader: &mut R, max_size: usize) -> std::io::Result<String> {
    let mut result = String::new();
    let mut buf = [0u8; 1];

    while result.len() < max_size {
        match reader.read_exact(&mut buf) {
            Ok(()) => {
                let c = buf[0];
                if c == b'\n' {
                    break;
                }
                result.push(c as char);
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        }
    }

    Ok(result)
}

// Read and discard data until newline
pub fn get_void<R: std::io::Read>(reader: &mut R) -> std::io::Result<bool> {
    let mut buf = [0u8; 1];

    loop {
        match reader.read_exact(&mut buf) {
            Ok(()) => {
                if buf[0] == b'\n' {
                    return Ok(true);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(false),
            Err(e) => return Err(e),
        }
    }
}

// Read the next command from state pipe
pub fn get_cmd<R: std::io::Read>(reader: &mut R) -> std::io::Result<StateToken> {
    let mut cmd_buf = [0u8; 3];

    match reader.read_exact(&mut cmd_buf) {
        Ok(()) => {
            let cmd_str = std::str::from_utf8(&cmd_buf).unwrap_or("   ");

            for state_cmd in STATE_COMMANDS {
                if state_cmd.name == cmd_str {
                    return Ok(state_cmd.cmd);
                }
            }

            Ok(StateToken::Eof)
        }
        Err(_) => Ok(StateToken::Eof),
    }
}

fn main() {
    println!("Copyright 2025 PalindromicBreadLoaf");
}