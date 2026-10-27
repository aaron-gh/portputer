//! Installs the system components, as root: the agent in
//! /Library/Application Support/Portputer, the helper daemon, and the agent
//! launchd starts at the login window.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

const DIR: &str = "/Library/Application Support/Portputer";
const AGENT: &str = "/Library/Application Support/Portputer/portputer-agent";
const DAEMON_LABEL: &str = "com.portputer.helper";
const DAEMON_PLIST: &str = "/Library/LaunchDaemons/com.portputer.helper.plist";
const LOGIN_LABEL: &str = "com.portputer.loginwindow";
const LOGIN_PLIST: &str = "/Library/LaunchAgents/com.portputer.loginwindow.plist";
const LOG_DIR: &str = "/Library/Logs/Portputer";

fn plist(label: &str, args: &[&str], extra: &str, log: &str) -> String {
    let args: String = args.iter().map(|a| format!("\t\t<string>{a}</string>\n")).collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\
\t<key>Label</key>\n\t<string>{label}</string>\n\
\t<key>ProgramArguments</key>\n\t<array>\n{args}\t</array>\n\
\t<key>RunAtLoad</key>\n\t<true/>\n\
\t<key>KeepAlive</key>\n\t<true/>\n\
\t<key>StandardErrorPath</key>\n\t<string>{LOG_DIR}/{log}</string>\n\
{extra}</dict>\n</plist>\n"
    )
}

fn fail(message: String) -> ! {
    eprintln!("{message}");
    std::process::exit(1)
}

fn require_root() {
    if unsafe { libc::geteuid() } != 0 {
        fail("Run as root.".into());
    }
}

fn write(path: &str, text: &str, mode: u32) {
    std::fs::write(path, text).unwrap_or_else(|e| fail(format!("{path}: {e}")));
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

fn launchctl(args: &[&str]) -> bool {
    Command::new("/bin/launchctl").args(args).status().is_ok_and(|s| s.success())
}

pub fn install() -> ! {
    require_root();
    let me = std::env::current_exe().unwrap_or_else(|e| fail(e.to_string()));
    std::fs::create_dir_all(DIR).unwrap_or_else(|e| fail(format!("{DIR}: {e}")));
    std::fs::create_dir_all(LOG_DIR).ok();
    if me != Path::new(AGENT) {
        // Copied to a temporary name and renamed, as the running agent may be
        // the old copy.
        let tmp = format!("{AGENT}.new");
        std::fs::copy(&me, &tmp).unwrap_or_else(|e| fail(format!("copying the agent: {e}")));
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755));
        std::fs::rename(&tmp, AGENT).unwrap_or_else(|e| fail(format!("{AGENT}: {e}")));
    }
    write(DAEMON_PLIST, &plist(DAEMON_LABEL, &[AGENT, "helper"], "", "helper.log"), 0o644);
    let login_extra = "\t<key>LimitLoadToSessionType</key>\n\t<string>LoginWindow</string>\n";
    write(LOGIN_PLIST, &plist(LOGIN_LABEL, &[AGENT, "serve", "--login-window"], login_extra, "loginwindow.log"), 0o644);
    launchctl(&["bootout", &format!("system/{DAEMON_LABEL}")]);
    if !launchctl(&["bootstrap", "system", DAEMON_PLIST]) {
        fail("launchctl couldn't start the helper.".into());
    }
    println!("Installed.");
    std::process::exit(0)
}

pub fn uninstall() -> ! {
    require_root();
    launchctl(&["bootout", &format!("system/{DAEMON_LABEL}")]);
    for path in [DAEMON_PLIST, LOGIN_PLIST, AGENT] {
        let _ = std::fs::remove_file(path);
    }
    let _ = std::fs::remove_dir_all(DIR);
    let _ = std::fs::remove_file(portputer_core::platform::macos::HELPER_SOCKET);
    println!("Removed.");
    std::process::exit(0)
}
