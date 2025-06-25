use std::time::{SystemTime, UNIX_EPOCH};

// Standard configuration
const CHANGE_WAIT: bool = false; // Change runlevel while waiting for a process to exit?

// Debug and test modes
const DEBUG: bool = false;       // Debug code off
const INITDEBUG: bool = false;   // Fork at startup to debug init

// Constants
const INITPID: i32 = 1;          // pid of first process
const PIPE_FD: i32 = 10;         // File number of initfifo
const STATE_PIPE: i32 = 11;      // used to pass state through exec
const WAIT_BETWEEN_SIGNALS: u64 = 3; // default time to wait between TERM and KILL

// Failsafe configuration
const MAXSPAWN: u32 = 10;        // Max times respawned in..
const TESTTIME: u64 = 120;       // this much seconds
const SLEEPTIME: u64 = 300;      // Disable time

// Default path inherited by every child
const PATH_DEFAULT: &str = "/sbin:/usr/sbin:/bin:/usr/bin";

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
    pub wrote_wtmp_reboot: bool,
    pub wrote_utmp_reboot: bool,
    pub wrote_wtmp_rlevel: bool,
    pub wrote_utmp_rlevel: bool,
    pub curlevel: char,                 // Current runlevel
    pub prevlevel: char,                // Previous runlevel
}

impl InitState {
    pub fn new() -> Self {
        InitState {
            family: None,
            wrote_wtmp_reboot: false,
            wrote_utmp_reboot: false,
            wrote_wtmp_rlevel: false,
            wrote_utmp_rlevel: false,
            curlevel: 'S',   // single-user mode
            prevlevel: 'N',  // no previous runlevel
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

fn main() {
    println!("Copyright 2025 PalindromicBreadLoaf");
}