//! Native CLI routing against a headless primary on a PRIVATE session bus.
//! No GTK window, real session bus, credentials, or user agent homes are used.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gio::glib;
use gio::prelude::*;
use omacharts::{cli, store::Store};
use serde_json::Value;

const APP_ID: &str = "com.jorgemanrubia.Omacharts";
const CASE: &str = "skill_commands_stay_with_caller_when_primary_owns_bus_name";
const ROOT_VAR: &str = "OMACHARTS_PRIVATE_BUS_TEST_ROOT";

struct Machine(PathBuf);

impl Machine {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "omacharts-private-bus-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        for dir in ["home", "bin", "data", "config", "cache", "tmp", "runtime"] {
            std::fs::create_dir(root.join(dir)).unwrap();
        }
        Self(root)
    }

    fn command(&self, executable: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(executable);
        command
            .env_clear()
            .env("HOME", self.0.join("home"))
            .env("PATH", self.0.join("bin"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_CACHE_HOME", self.0.join("cache"))
            .env("XDG_RUNTIME_DIR", self.0.join("runtime"))
            .env("TMPDIR", self.0.join("tmp"));
        command
    }

    fn native(&self, args: &[&str], home: &Path) -> Output {
        let output = self
            .command(env!("CARGO_BIN_EXE_omacharts"))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                std::env::var_os("DBUS_SESSION_BUS_ADDRESS").unwrap(),
            )
            .env("HERMES_HOME", home)
            .args(args)
            .output()
            .unwrap();
        println!("native {args:?}: {output:?}");
        output
    }

    fn skill(&self, verb: &str, flags: &[&str], home: &Path) -> Value {
        let mut args = vec!["skill", verb, "--json"];
        args.extend_from_slice(flags);
        let output = self.native(&args, home);
        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Primary(Child);

impl Drop for Primary {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn assert_agent(report: &Value, state: &str, at: &Path) {
    assert_eq!(report["agents"].as_array().unwrap().len(), 1, "{report}");
    assert_eq!(report["agents"][0]["agent"], "Hermes", "{report}");
    assert_eq!(report["agents"][0]["state"], state, "{report}");
    assert_eq!(
        report["agents"][0]["path"],
        at.to_string_lossy().as_ref(),
        "skill must use the invoking caller's home, not the primary's: {report}"
    );
}

#[test]
fn skill_commands_stay_with_caller_when_primary_owns_bus_name() {
    // Re-exec the test under a fresh bus; the outer process never connects to
    // an inherited session bus. Only a missing dbus-run-session can skip it.
    if std::env::var_os(ROOT_VAR).is_none() {
        let machine = Machine::new();
        let output = machine
            .command("/usr/bin/dbus-run-session")
            .arg("--dbus-daemon=/usr/bin/dbus-daemon")
            .arg("--")
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", CASE, "--nocapture"])
            .env(ROOT_VAR, &machine.0)
            .output();
        let output = match output {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("SKIP private bus regression: /usr/bin/dbus-run-session unavailable");
                return;
            }
            other => other.unwrap(),
        };
        print!("{}", String::from_utf8_lossy(&output.stdout));
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(
            output.status.success(),
            "private bus case failed: {output:?}"
        );
        return;
    }

    let machine = Machine(PathBuf::from(std::env::var_os(ROOT_VAR).unwrap()));
    let caller_home = machine.0.join("caller-profile");
    let primary_home = machine.0.join("primary-profile");
    std::fs::create_dir(&primary_home).unwrap();
    let sentinel = primary_home.join("sentinel");
    std::fs::write(&sentinel, "primary stays untouched").unwrap();
    let primary_log = machine.0.join("received.jsonl");
    std::fs::write(&primary_log, "").unwrap();
    let mut primary = Primary(
        machine
            .command(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "headless_primary", "--nocapture"])
            .env(ROOT_VAR, &machine.0)
            .env("HERMES_HOME", &primary_home)
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                std::env::var_os("DBUS_SESSION_BUS_ADDRESS").unwrap(),
            )
            .stdout(Stdio::from(
                std::fs::File::create(machine.0.join("primary.log")).unwrap(),
            ))
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !machine.0.join("ready").exists() {
        assert!(
            primary.0.try_wait().unwrap().is_none(),
            "primary exited before registration"
        );
        assert!(Instant::now() < deadline, "primary registration timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).unwrap();
    let reply = bus
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&(APP_ID,).into()),
            None,
            gio::DBusCallFlags::NONE,
            1000,
            gio::Cancellable::NONE,
        )
        .unwrap();
    assert_eq!(reply.child_value(0).get::<bool>(), Some(true));
    println!("PRIVATE BUS primary owns {APP_ID}; primary home differs from caller home");

    // Native control probes prove this is a reachable primary, not just an
    // unavailable-bus fixture. It acknowledges dispatch without simulating UI.
    for args in [["chart", "show"], ["watchlist", "list"], ["status", "show"]] {
        let output = machine.native(&args, &caller_home);
        assert_eq!(output.status.code(), Some(73), "{output:?}");
        assert_eq!(output.stdout, b"headless primary received control\n");
        assert!(output.stderr.is_empty(), "{output:?}");
    }
    let controls = std::fs::read(&primary_log).unwrap();
    assert_eq!(String::from_utf8_lossy(&controls).lines().count(), 3);

    let at = caller_home.join("skills/omacharts");
    let report = machine.skill("status", &["--hermes"], &caller_home);
    assert_agent(&report, "absent", &at);
    assert!(
        !caller_home.exists(),
        "status must not create the caller profile"
    );
    let report = machine.skill("install", &["--hermes"], &caller_home);
    assert_agent(&report, "installed", &at);
    assert_eq!(
        at.read_link().unwrap(),
        PathBuf::from(report["source"].as_str().unwrap())
    );
    assert!(at.join("SKILL.md").is_file());
    let report = machine.skill("status", &["--hermes"], &caller_home);
    assert_agent(&report, "installed", &at);
    let report = machine.skill("uninstall", &["--hermes"], &caller_home);
    assert_agent(&report, "absent", &at);
    assert!(at.symlink_metadata().is_err());
    assert!(!caller_home.join("skills").exists());

    // The whole skill noun stays local: explicit --to, text, help, and parser
    // errors still use the existing dispatcher rather than a separate parser.
    let to = machine.0.join("project-skills");
    for verb in ["install", "status", "uninstall"] {
        let report = machine.skill(
            verb,
            &["--hermes", "--to", to.to_str().unwrap()],
            &caller_home,
        );
        assert!(report["agents"][0]["agent"].is_null());
        assert_eq!(
            report["agents"][0]["path"],
            to.join("omacharts").to_string_lossy().as_ref()
        );
    }
    assert!(!to.exists());
    let output = machine.native(&["skill", "status", "--hermes"], &caller_home);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains(at.to_str().unwrap()));
    let output = machine.native(&["skill", "install", "--help"], &caller_home);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("--hermes"));
    for args in [
        vec!["skill", "unknown"],
        vec!["skill", "install", "--unknown"],
    ] {
        let output = machine.native(&args, &caller_home);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(!output.stderr.is_empty());
    }
    assert_eq!(
        std::fs::read(&primary_log).unwrap(),
        controls,
        "no skill command may reach primary"
    );
    assert_eq!(
        std::fs::read_to_string(&sentinel).unwrap(),
        "primary stays untouched"
    );
    assert_eq!(std::fs::read_dir(&primary_home).unwrap().count(), 1);
    for dir in [".hermes", ".claude", ".codex"] {
        assert!(!machine.0.join("home").join(dir).exists());
    }
    assert!(
        primary.0.try_wait().unwrap().is_none(),
        "primary must remain alive"
    );
    println!(
        "PRIVATE BUS PASS: caller-only roundtrip; primary home untouched; only 3 controls forwarded"
    );
}

#[test]
#[ignore = "private-bus fixture subprocess, launched by the regression test"]
fn headless_primary() {
    let root =
        PathBuf::from(std::env::var_os(ROOT_VAR).expect("fixture requires isolated test root"));
    assert_eq!(
        std::env::var_os("HERMES_HOME").unwrap(),
        root.join("primary-profile")
    );
    assert!(std::env::var_os("DISPLAY").is_none());
    assert!(std::env::var_os("WAYLAND_DISPLAY").is_none());
    let app = gio::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    let log = root.join("received.jsonl");
    app.connect_command_line(move |_, command_line| {
        let args: Vec<String> = command_line
            .arguments()
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let mut file = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        writeln!(file, "{}", serde_json::to_string(&args).unwrap()).unwrap();
        if args.get(1).is_some_and(|arg| arg == "skill") {
            // Match the GUI's existing cli::run with PRIMARY process env.
            let outcome = cli::run(&args, &Store::memory().unwrap(), None);
            command_line.print_literal(&outcome.out);
            command_line.printerr_literal(&outcome.err);
            glib::ExitCode::from(outcome.code)
        } else {
            command_line.print_literal("headless primary received control\n");
            glib::ExitCode::from(73)
        }
    });
    app.register(gio::Cancellable::NONE).unwrap();
    assert!(!app.is_remote(), "fixture must actually own the name");
    let _hold = app.hold();
    std::fs::write(root.join("ready"), "registered primary").unwrap();
    let main_loop = glib::MainLoop::new(None, false);
    let stop = main_loop.clone();
    glib::timeout_add_seconds_local_once(30, move || stop.quit());
    main_loop.run();
}
