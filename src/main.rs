use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use nix::errno::Errno;
use nix::libc::{self, pid_t};
use nix::sys::signal::{self, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{getpid, Pid};

// Runlevels
#[derive(Debug, Clone, Copy, PartialEq)]
enum RunLevel {
    Halt = 0,
    Single = 1,
    Multi = 2,
    MultiWithNetwork = 3,
    Unused = 4,
    MultiWithNetworkAndX11 = 5,
    Reboot = 6,
}

impl RunLevel {
    fn from_char(c: char) -> Option<Self> {
        match c {
            '0' => Some(RunLevel::Halt),
            '1' | 'S' | 's' => Some(RunLevel::Single),
            '2' => Some(RunLevel::Multi),
            '3' => Some(RunLevel::MultiWithNetwork),
            '4' => Some(RunLevel::Unused),
            '5' => Some(RunLevel::MultiWithNetworkAndX11),
            '6' => Some(RunLevel::Reboot),
            _ => None,
        }
    }
}

// Process entry for tracking spawned processes
#[derive(Debug)]
struct ProcessEntry {
    pid: Pid,
    cmd: String,
    runlevels: Vec<RunLevel>,
    action: String,
    respawn: bool,
}

// Main init structure
struct Init {
    current_runlevel: RunLevel,
    processes: HashMap<Pid, ProcessEntry>,
    shutdown_requested: Arc<AtomicBool>,
    inittab_path: String,
}

impl Init {
    fn new() -> Self {
        Init {
            current_runlevel: RunLevel::Multi,
            processes: HashMap::new(),
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            inittab_path: "/etc/inittab".to_string(),
        }
    }

    // Parse /etc/inittab file
    fn parse_inittab(&self) -> Result<Vec<InitTabEntry>, Box<dyn std::error::Error>> {
        let file = File::open(&self.inittab_path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();

        for line in reader.lines() {
            let line = line?;
            let line = line.trim();

            // Skip comments and empty lines
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some(entry) = InitTabEntry::parse(line) {
                entries.push(entry);
            }
        }

        Ok(entries)
    }

    // Initialize system
    fn init_system(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        println!("INIT: Starting system initialization");

        // Become session leader
        unsafe {
            libc::setsid();
        }

        // Set up signal handlers
        self.setup_signal_handlers()?;

        // Parse inittab and start initial processes
        let entries = self.parse_inittab()?;
        for entry in entries {
            if entry.action == "sysinit" {
                self.run_process(&entry)?;
            }
        }

        // Start processes for current runlevel
        self.change_runlevel(self.current_runlevel)?;

        Ok(())
    }

    // Setup signal handlers
    fn setup_signal_handlers(&self) -> Result<(), Box<dyn std::error::Error>> {
        let shutdown_flag = Arc::clone(&self.shutdown_requested);

        // Handle SIGTERM, SIGINT for shutdown
        let shutdown_flag_term = Arc::clone(&shutdown_flag);
        unsafe {
            signal::signal(Signal::SIGTERM, signal::SigHandler::Handler(handle_shutdown_signal))?;
            signal::signal(Signal::SIGINT, signal::SigHandler::Handler(handle_shutdown_signal))?;
        }

        // Handle SIGCHLD for process reaping
        unsafe {
            signal::signal(Signal::SIGCHLD, signal::SigHandler::Handler(handle_sigchld))?;
        }

        Ok(())
    }

    // Change to a new runlevel
    fn change_runlevel(&mut self, new_level: RunLevel) -> Result<(), Box<dyn std::error::Error>> {
        println!("INIT: Changing to runlevel {}", new_level as i32);

        // Kill processes not in new runlevel
        let mut to_kill = Vec::new();
        for (pid, process) in &self.processes {
            if !process.runlevels.contains(&new_level) {
                to_kill.push(*pid);
            }
        }

        for pid in to_kill {
            self.kill_process(pid)?;
        }

        // Start new processes for this runlevel
        let entries = self.parse_inittab()?;
        for entry in entries {
            if entry.runlevels.contains(&new_level) &&
                (entry.action == "respawn" || entry.action == "once") {
                self.spawn_process(&entry)?;
            }
        }

        self.current_runlevel = new_level;

        // Handle special runlevels
        match new_level {
            RunLevel::Halt => self.shutdown_system(false)?,
            RunLevel::Reboot => self.shutdown_system(true)?,
            _ => {}
        }

        Ok(())
    }

    // Spawn a process
    fn spawn_process(&mut self, entry: &InitTabEntry) -> Result<(), Box<dyn std::error::Error>> {
        let mut cmd_parts = entry.process.split_whitespace();
        let program = cmd_parts.next().ok_or("Empty command")?;
        let args: Vec<&str> = cmd_parts.collect();

        let mut command = Command::new(program);
        command.args(&args);
        command.stdin(Stdio::null());
        command.stdout(Stdio::inherit());
        command.stderr(Stdio::inherit());

        // Set process group
        unsafe {
            command.pre_exec(|| {
                libc::setpgid(0, 0);
                Ok(())
            });
        }

        let child = command.spawn()?;
        let pid = Pid::from_raw(child.id() as pid_t);

        let process_entry = ProcessEntry {
            pid,
            cmd: entry.process.clone(),
            runlevels: entry.runlevels.clone(),
            action: entry.action.clone(),
            respawn: entry.action == "respawn",
        };

        self.processes.insert(pid, process_entry);
        println!("INIT: Started process {} with PID {}", entry.process, pid);

        Ok(())
    }

    // Run a process and wait for it to complete
    fn run_process(&self, entry: &InitTabEntry) -> Result<(), Box<dyn std::error::Error>> {
        let mut cmd_parts = entry.process.split_whitespace();
        let program = cmd_parts.next().ok_or("Empty command")?;
        let args: Vec<&str> = cmd_parts.collect();

        let status = Command::new(program)
            .args(&args)
            .status()?;

        if !status.success() {
            eprintln!("INIT: Process {} failed with status {}", entry.process, status);
        }

        Ok(())
    }

    // Kill a process
    fn kill_process(&mut self, pid: Pid) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(process) = self.processes.get(&pid) {
            println!("INIT: Terminating process {} (PID {})", process.cmd, pid);

            // Send SIGTERM first
            if let Err(e) = signal::kill(pid, Signal::SIGTERM) {
                eprintln!("INIT: Failed to send SIGTERM to {}: {}", pid, e);
            }

            // Wait a bit, then send SIGKILL if needed
            thread::sleep(Duration::from_secs(5));

            if let Err(e) = signal::kill(pid, Signal::SIGKILL) {
                // Process might have already exited
                if e != Errno::ESRCH {
                    eprintln!("INIT: Failed to send SIGKILL to {}: {}", pid, e);
                }
            }

            self.processes.remove(&pid);
        }

        Ok(())
    }

    // Reap child processes
    fn reap_children(&mut self) {
        loop {
            match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                Ok(WaitStatus::Exited(pid, status)) => {
                    println!("INIT: Process {} exited with status {}", pid, status);
                    if let Some(process) = self.processes.remove(&pid) {
                        // Respawn if needed
                        if process.respawn && process.runlevels.contains(&self.current_runlevel) {
                            println!("INIT: Respawning {}", process.cmd);
                            // Create a new InitTabEntry for respawning
                            let entry = InitTabEntry {
                                id: format!("respawn_{}", pid),
                                runlevels: process.runlevels,
                                action: process.action,
                                process: process.cmd,
                            };
                            if let Err(e) = self.spawn_process(&entry) {
                                eprintln!("INIT: Failed to respawn process: {}", e);
                            }
                        }
                    }
                }
                Ok(WaitStatus::Signaled(pid, signal, _)) => {
                    println!("INIT: Process {} killed by signal {:?}", pid, signal);
                    self.processes.remove(&pid);
                }
                Ok(_) => {} // Other status types
                Err(Errno::ECHILD) => break, // No more children
                Err(e) => {
                    eprintln!("INIT: waitpid error: {}", e);
                    break;
                }
            }
        }
    }

    // Shutdown the system
    fn shutdown_system(&self, reboot: bool) -> Result<(), Box<dyn std::error::Error>> {
        println!("INIT: System {} requested", if reboot { "reboot" } else { "shutdown" });

        // Kill all processes
        unsafe {
            libc::kill(-1, libc::SIGTERM);
        }

        thread::sleep(Duration::from_secs(5));

        unsafe {
            libc::kill(-1, libc::SIGKILL);
        }

        // Sync filesystems
        unsafe {
            libc::sync();
        }

        // Reboot or halt
        if reboot {
            unsafe {
                libc::reboot(libc::RB_AUTOBOOT);
            }
        } else {
            unsafe {
                libc::reboot(libc::RB_HALT_SYSTEM);
            }
        }

        Ok(())
    }

    // Main event loop
    fn run(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.init_system()?;

        while !self.shutdown_requested.load(Ordering::Relaxed) {
            self.reap_children();
            thread::sleep(Duration::from_millis(100));
        }

        self.change_runlevel(RunLevel::Halt)?;
        Ok(())
    }
}

// InitTab entry structure
#[derive(Debug, Clone)]
struct InitTabEntry {
    id: String,
    runlevels: Vec<RunLevel>,
    action: String,
    process: String,
}

impl InitTabEntry {
    fn parse(line: &str) -> Option<Self> {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() != 4 {
            return None;
        }

        let id = parts[0].to_string();
        let runlevels_str = parts[1];
        let action = parts[2].to_string();
        let process = parts[3].to_string();

        let mut runlevels = Vec::new();
        for c in runlevels_str.chars() {
            if let Some(level) = RunLevel::from_char(c) {
                runlevels.push(level);
            }
        }

        Some(InitTabEntry {
            id,
            runlevels,
            action,
            process,
        })
    }
}

// Signal handlers
extern "C" fn handle_shutdown_signal(_: libc::c_int) {
    // Signal shutdown - this would need to communicate with the main thread
    println!("INIT: Shutdown signal received");
}

extern "C" fn handle_sigchld(_: libc::c_int) {
    // Child process died - handled in main loop
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Check if we're PID 1
    if getpid() != Pid::from_raw(1) {
        eprintln!("Warning: Not running as PID 1");
    }

    let mut init = Init::new();
    init.run()
}

// Add to Cargo.toml:
// [dependencies]
// nix = "0.27"