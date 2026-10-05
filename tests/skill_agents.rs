//! The real CLI, with every agent home and the session bus isolated from the user.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

struct Machine {
    root: PathBuf,
}

impl Machine {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("omacharts-agents-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        for dir in ["home", "bin", "data", "config", "cache", "tmp"] {
            std::fs::create_dir(root.join(dir)).unwrap();
        }
        Self { root }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_omacharts"));
        command
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("PATH", self.root.join("bin"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("TMPDIR", self.root.join("tmp"))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}/no-bus", self.root.display()),
            );
        command
    }

    fn skill(&self, verb: &str, flags: &[&str], home: Option<&Path>, code: i32) -> Value {
        let mut command = self.command();
        command.args(["skill", verb, "--json"]).args(flags);
        if let Some(home) = home {
            command.env("HERMES_HOME", home);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(code), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn agent<'a>(report: &'a Value, name: &str, state: &str, path: &Path) -> &'a Value {
    let agents = report["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1, "{report}");
    assert_eq!(agents[0]["agent"], name);
    assert_eq!(agents[0]["state"], state);
    assert_eq!(agents[0]["path"], path.to_string_lossy().as_ref());
    &agents[0]
}

#[test]
fn hermes_explicit_home_roundtrips_only_the_requested_shared_skill() {
    let machine = Machine::new();
    let home = machine.root.join("selected-profile");
    let at = home.join("skills/omacharts");
    let report = machine.skill("status", &["--hermes"], Some(&home), 0);
    agent(&report, "Hermes", "absent", &at);
    assert!(!home.exists(), "status must not create an agent home");

    let report = machine.skill("install", &["--hermes"], Some(&home), 0);
    agent(&report, "Hermes", "installed", &at);
    let source = PathBuf::from(report["source"].as_str().unwrap());
    assert_eq!(at.read_link().unwrap(), source);
    assert!(
        at.join("SKILL.md").is_file(),
        "the shared skill must be readable"
    );
    assert_eq!(
        std::fs::read(at.join("SKILL.md")).unwrap(),
        std::fs::read(source.join("SKILL.md")).unwrap()
    );

    let report = machine.skill("install", &["--hermes"], Some(&home), 0);
    agent(&report, "Hermes", "installed", &at);
    assert_eq!(std::fs::read_dir(home.join("skills")).unwrap().count(), 1);
    let report = machine.skill("status", &["--hermes"], Some(&home), 0);
    agent(&report, "Hermes", "installed", &at);
    let report = machine.skill("uninstall", &["--hermes"], Some(&home), 0);
    agent(&report, "Hermes", "absent", &at);
    assert!(at.symlink_metadata().is_err());
    assert!(!home.join("skills").exists());
    for other in [".hermes", ".claude", ".codex"] {
        assert!(!machine.root.join("home").join(other).exists());
    }
}

#[test]
fn hermes_unset_or_empty_home_falls_back_under_home() {
    for home in [None, Some(Path::new(""))] {
        let machine = Machine::new();
        let at = machine.root.join("home/.hermes/skills/omacharts");
        let report = machine.skill("install", &["--hermes"], home, 0);
        agent(&report, "Hermes", "installed", &at);
        assert!(at.join("SKILL.md").is_file());
        let report = machine.skill("uninstall", &["--hermes"], home, 0);
        agent(&report, "Hermes", "absent", &at);
    }
}

#[test]
fn bare_install_with_no_agents_writes_no_skills() {
    let machine = Machine::new();
    let report = machine.skill("install", &[], None, 0);
    assert_eq!(report["agents"], serde_json::json!([]));
    for dir in [".claude", ".codex", ".hermes"] {
        assert!(!machine.root.join("home").join(dir).exists());
    }
    let output = machine
        .command()
        .args(["skill", "install"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains(".hermes"), "{text}");
    assert!(text.contains("hermes on PATH"), "{text}");
}

#[test]
fn hermes_config_directory_is_detected_without_installing_on_status() {
    for selected in [false, true] {
        let machine = Machine::new();
        let home = if selected {
            machine.root.join("profile")
        } else {
            machine.root.join("home/.hermes")
        };
        std::fs::create_dir(&home).unwrap();
        let env = selected.then_some(home.as_path());
        let at = home.join("skills/omacharts");
        let report = machine.skill("status", &[], env, 0);
        agent(&report, "Hermes", "absent", &at);
        assert!(!home.join("skills").exists(), "detection is read-only");
        let report = machine.skill("install", &[], env, 0);
        agent(&report, "Hermes", "installed", &at);
    }
}

#[test]
fn hermes_on_path_is_detected_without_running_the_agent() {
    let machine = Machine::new();
    // Detection looks for a file, just as it does for the existing agents.
    std::fs::write(machine.root.join("bin/hermes"), "not an executable").unwrap();
    let home = machine.root.join("profile-not-started");
    let at = home.join("skills/omacharts");
    let report = machine.skill("status", &[], Some(&home), 0);
    agent(&report, "Hermes", "absent", &at);
    assert!(!home.exists());
    let report = machine.skill("install", &[], Some(&home), 0);
    agent(&report, "Hermes", "installed", &at);
}

#[test]
fn selected_hermes_home_does_not_detect_or_touch_other_profiles() {
    let machine = Machine::new();
    let default = machine.root.join("home/.hermes");
    let sibling = default.join("profiles/other/skills");
    std::fs::create_dir_all(&sibling).unwrap();
    std::fs::write(sibling.join("sentinel"), "untouched").unwrap();
    let selected = machine.root.join("selected");
    let report = machine.skill("install", &[], Some(&selected), 0);
    assert_eq!(
        report["agents"],
        serde_json::json!([]),
        "only the selected home counts"
    );
    assert!(!selected.exists());
    let report = machine.skill("install", &["--hermes"], Some(&selected), 0);
    agent(
        &report,
        "Hermes",
        "installed",
        &selected.join("skills/omacharts"),
    );
    assert!(!default.join("skills").exists());
    assert_eq!(
        std::fs::read_to_string(sibling.join("sentinel")).unwrap(),
        "untouched"
    );
    assert_eq!(std::fs::read_dir(&sibling).unwrap().count(), 1);
}

#[test]
fn existing_agents_remain_selectable_and_share_one_source_with_hermes() {
    let machine = Machine::new();
    for dir in [".claude", ".codex", ".hermes"] {
        std::fs::create_dir(machine.root.join("home").join(dir)).unwrap();
    }
    for (flag, name, dir) in [
        ("--claude", "Claude", ".claude"),
        ("--codex", "Codex", ".codex"),
        ("--hermes", "Hermes", ".hermes"),
    ] {
        let at = machine.root.join("home").join(dir).join("skills/omacharts");
        let report = machine.skill("install", &[flag], None, 0);
        agent(&report, name, "installed", &at);
        let report = machine.skill("uninstall", &[flag], None, 0);
        agent(&report, name, "absent", &at);
    }
    let report = machine.skill("install", &["--claude", "--codex", "--hermes"], None, 0);
    assert_eq!(report["agents"].as_array().unwrap().len(), 3);
    let source = PathBuf::from(report["source"].as_str().unwrap());
    for dir in [".claude", ".codex", ".hermes"] {
        assert_eq!(
            machine
                .root
                .join("home")
                .join(dir)
                .join("skills/omacharts")
                .read_link()
                .unwrap(),
            source
        );
    }
    let report = machine.skill("status", &[], None, 0);
    assert_eq!(report["agents"].as_array().unwrap().len(), 3);
    let report = machine.skill("uninstall", &[], None, 0);
    assert_eq!(report["agents"].as_array().unwrap().len(), 3);
}

#[test]
fn to_directory_overrides_hermes_and_every_other_selection() {
    let machine = Machine::new();
    let to = machine.root.join("project-skills");
    let home = machine.root.join("profile");
    for verb in ["install", "status", "uninstall"] {
        let report = machine.skill(
            verb,
            &["--hermes", "--codex", "--to", to.to_str().unwrap()],
            Some(&home),
            0,
        );
        assert_eq!(report["agents"].as_array().unwrap().len(), 1);
        assert!(report["agents"][0]["agent"].is_null());
        assert_eq!(
            report["agents"][0]["path"],
            to.join("omacharts").to_string_lossy().as_ref()
        );
    }
    assert!(!home.exists());
    assert!(!machine.root.join("home/.codex").exists());
    assert!(!to.exists());
}

#[test]
fn hermes_occupied_unowned_paths_are_neither_overwritten_nor_removed() {
    for kind in ["file", "directory", "symlink"] {
        let machine = Machine::new();
        let home = machine.root.join("profile");
        let at = home.join("skills/omacharts");
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        let sentinel = machine.root.join("unowned");
        std::fs::write(&sentinel, "keep this").unwrap();
        match kind {
            "file" => std::fs::write(&at, "keep this").unwrap(),
            "directory" => {
                std::fs::create_dir(&at).unwrap();
                std::fs::write(at.join("SKILL.md"), "keep this").unwrap();
            }
            _ => std::os::unix::fs::symlink(&sentinel, &at).unwrap(),
        }
        for (verb, code) in [("install", 5), ("status", 0), ("uninstall", 5)] {
            let report = machine.skill(verb, &["--hermes"], Some(&home), code);
            let listed = agent(&report, "Hermes", "occupied", &at);
            assert_eq!(listed["refused"], verb != "status");
            let preserved = if kind == "directory" {
                at.join("SKILL.md")
            } else {
                at.clone()
            };
            assert_eq!(std::fs::read_to_string(preserved).unwrap(), "keep this");
            if kind == "symlink" {
                assert_eq!(at.read_link().unwrap(), sentinel);
            }
        }
    }
}

#[test]
fn hermes_dangling_owned_link_is_reported_and_uninstalled() {
    let machine = Machine::new();
    let home = machine.root.join("profile");
    let at = home.join("skills/omacharts");
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    let gone = machine.root.join("removed-package/agents/skills/omacharts");
    std::os::unix::fs::symlink(gone, &at).unwrap();
    let report = machine.skill("status", &["--hermes"], Some(&home), 0);
    agent(&report, "Hermes", "dangling", &at);
    let report = machine.skill("uninstall", &["--hermes"], Some(&home), 0);
    agent(&report, "Hermes", "absent", &at);
    assert!(at.symlink_metadata().is_err());
}

#[test]
fn generated_surface_and_help_expose_hermes_and_keep_examples_executable() {
    let machine = Machine::new();
    let output = machine
        .command()
        .args(["surface", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let surface: Value = serde_json::from_slice(&output.stdout).unwrap();
    for verb in ["install", "status", "uninstall"] {
        let command = format!("skill {verb}");
        let spec = surface["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|spec| spec["command"] == command)
            .unwrap();
        let flag = spec["flags"]
            .as_array()
            .unwrap()
            .iter()
            .find(|flag| flag["name"] == "hermes")
            .unwrap();
        assert_eq!(flag["type"], "switch");
        let output = machine
            .command()
            .args(["skill", verb, "--help"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("--hermes"), "{text}");
        assert!(text.contains(spec["example"].as_str().unwrap()), "{text}");
        // Run the authoritative example with the existing directory escape hatch.
        let output = machine
            .command()
            .args(spec["example"].as_str().unwrap().split_whitespace().skip(1))
            .arg("--to")
            .arg(machine.root.join("examples"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    assert!(
        !machine.root.join("home/.hermes").exists(),
        "surface discovery is not opt-in"
    );
}
