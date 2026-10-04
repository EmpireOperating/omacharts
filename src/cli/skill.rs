//! Putting the agent skill where an agent will find it.
//!
//! Omacharts ships one skill — `agents/skills/omacharts/SKILL.md` —
//! whose job is discovery. `omacharts surface --json` already describes every
//! command perfectly, but only once something has thought to ask; and
//! `AGENTS.md` is only read by an agent already inside this repo. The skill is
//! what makes an agent anywhere on the machine reach for omacharts when
//! somebody says "what's semis doing".
//!
//! **One skill, installed in several places.** Claude reads
//! `~/.claude/skills/<name>/SKILL.md` and Codex reads
//! `$CODEX_HOME/skills/<name>/SKILL.md`, and the two want the same shape of
//! directory — frontmatter with a `name` and a `description`, prose below. So
//! there is one file and [`KNOWN`] is a list of places to point at it. A second
//! copy per agent would be a second thing to drift, which is the one failure
//! this whole arrangement is designed against.
//!
//! **This never runs as a side effect.** Not from the package's post-install,
//! not from `bin/install`, not from the first launch. Somebody installing a
//! charting app has not agreed to have their agent's configuration written
//! into, and a charting app is not entitled to assume otherwise. It is one
//! explicit command, it says exactly which paths it wrote, and
//! `skill uninstall` takes them back out — an opt-in nobody can reverse is one
//! they should not have been offered.
//!
//! **A symlink, not a copy.** The risk this is designed against is a skill that
//! drifts from the CLI, because a confidently wrong skill is worse than none:
//! the file is version-controlled beside the surface it describes, and a test
//! runs its examples. A symlink into `/usr/share/omacharts` keeps that true
//! through every package upgrade, where a copy would quietly go stale the first
//! time the surface changed. The cost is that removing the package leaves a
//! dangling link — which is the right way round to fail. A dangling symlink
//! loads nothing and `skill status` names it; a stale copy keeps answering,
//! wrongly. Codex evidently agrees: the skills directory it ships with is
//! itself full of symlinks into `/usr/share`.

use std::path::{Path, PathBuf};

use super::{Fault, Outcome, EXIT_ERROR, EXIT_OK, EXIT_REFUSED};

/// The skill's directory inside the plugin, relative to the plugin root.
const WITHIN_PLUGIN: &str = "skills/omacharts";

/// What the skill is called where it is installed, and inside the plugin.
const NAME: &str = "omacharts";

/// The directory the skill sits in, inside whatever ships it.
///
/// `claude-plugin` is what that directory was called for one release, before
/// the skill turned out to serve Codex just as well and the name stopped being
/// true. Both are accepted, so a link made by that version is still one this
/// version will take back out — an uninstall that refuses to recognise its own
/// earlier work would strand the very people who opted in first.
const PLUGIN_DIRS: [&str; 2] = ["agents", "claude-plugin"];

/// An agent that reads skills out of a directory.
///
/// Adding one is a row here and a flag in `spec`, and
/// `every_agent_has_a_flag_that_selects_it` fails until both exist. Nothing
/// else in this module knows how many there are.
pub struct Known {
    /// What it is called in the output, where a person reads it.
    pub name: &'static str,
    /// The flag that asks for this one alone.
    pub flag: &'static str,
    /// The agent's own variable for where its configuration lives.
    home_var: &'static str,
    /// Its directory under `$HOME` when that variable is unset.
    home_dir: &'static str,
    /// What it puts on `PATH`.
    binary: &'static str,
}

pub const KNOWN: &[Known] = &[
    Known {
        name: "Claude",
        flag: "claude",
        home_var: "CLAUDE_CONFIG_DIR",
        home_dir: ".claude",
        binary: "claude",
    },
    Known {
        name: "Codex",
        flag: "codex",
        home_var: "CODEX_HOME",
        home_dir: ".codex",
        binary: "codex",
    },
];

/// Somewhere the skill goes.
pub struct Agent {
    /// The agent it belongs to, or `None` for a directory somebody named
    /// outright with `--to`, which is nobody's in particular.
    pub name: Option<&'static str>,
    pub skills: PathBuf,
}

impl Agent {
    /// "for Codex", or nothing at all when a directory was named outright.
    fn whose(&self) -> String {
        match self.name {
            Some(name) => format!(" for {name}"),
            None => String::new(),
        }
    }
}

/// Where this agent keeps its skills.
fn skills_of(known: &Known) -> PathBuf {
    let home = std::env::var_os(known.home_var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::store::home().join(known.home_dir));
    home.join("skills")
}

/// Is this agent on this machine?
///
/// Either signal is enough, and that is deliberate. Installing for an agent
/// somebody does not use is a directory they will never look in; failing to
/// install for one they do use is the actual failure, and the one they would
/// have to notice and diagnose. So: a configuration directory, which means the
/// agent has run here and has state, **or** the command on `PATH`, which
/// catches an agent installed but not yet started.
fn present(known: &Known) -> bool {
    let config = std::env::var_os(known.home_var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::store::home().join(known.home_dir));
    config.is_dir() || on_path(known.binary)
}

fn on_path(binary: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| dir.join(binary).is_file())
}

/// Which directories to act on.
///
/// `--to` first and alone: it names one directory outright, which is the escape
/// hatch for a project's own `.claude/skills` and the only way a test can run
/// this without writing into the configuration of whoever is running it.
///
/// Then any agent named explicitly, **whether or not it looks present** —
/// somebody who types `--codex` has said what they mean, and second-guessing
/// them with a detection heuristic would be the rude version of helpful.
///
/// Otherwise every agent this machine has. That is the bare command, and it is
/// the agent-agnostic one on purpose: the skill is not Claude's.
pub fn chosen(selected: &[&str], to: Option<&str>) -> Vec<Agent> {
    if let Some(dir) = to.filter(|d| !d.is_empty()) {
        return vec![Agent { name: None, skills: PathBuf::from(dir) }];
    }
    KNOWN
        .iter()
        .filter(|known| match selected.is_empty() {
            false => selected.contains(&known.flag),
            true => present(known),
        })
        .map(|known| Agent { name: Some(known.name), skills: skills_of(known) })
        .collect()
}

/// What a `skill` verb did, one agent at a time.
struct Report {
    agent: Option<&'static str>,
    state: &'static str,
    path: PathBuf,
    line: String,
    /// Something was there that is not ours. Nothing was touched.
    refused: bool,
}

/// Run one of the three verbs over every chosen directory.
///
/// Returns an [`Outcome`] rather than a `Result` because a run over two agents
/// can half work, and neither "it worked" nor "it failed" is then true. The
/// lines that worked go to stdout, the refusals to stderr, and the exit status
/// reports the refusal — so a script reading only the code learns that
/// something it asked for did not happen, and one reading the lines learns
/// which.
pub fn run(verb: &str, agents: &[Agent], as_json: bool) -> Outcome {
    // Nobody to install for is a fact about the machine, not a failure. An
    // installer that exits non-zero on a perfectly reasonable state is being
    // rude about it — but it has to say what it looked for, or the answer is
    // unactionable.
    if agents.is_empty() {
        return Outcome::ok(match as_json {
            true => format!("{}\n", serde_json::json!({"agents": [], "source": source_json()})),
            false => format!("no agent found to install for; looked for {}\n", looked_for()),
        });
    }

    let from = match verb {
        // Only installing needs the skill to exist. Reporting on a link whose
        // package has gone is most of the point of `status`, and `uninstall`
        // has to work on exactly that.
        "install" => match source() {
            None => return Outcome::failed(missing_source()),
            Some(from) => Some(from),
        },
        _ => source(),
    };

    let mut reports = Vec::new();
    for agent in agents {
        let report = match verb {
            "install" => match from.as_deref() {
                Some(from) => install_one(agent, from),
                None => return Outcome::failed(missing_source()),
            },
            "uninstall" => uninstall_one(agent),
            _ => status_one(agent),
        };
        match report {
            Ok(report) => reports.push(report),
            Err(fault) => return Outcome::failed(fault),
        }
    }
    answer(reports, from.as_deref(), as_json)
}

fn install_one(agent: &Agent, from: &Path) -> Result<Report, Fault> {
    let at = agent.skills.join(NAME);
    let whose = agent.whose();

    match look(&at) {
        Found::Ours(target) if target == from => Ok(Report {
            agent: agent.name,
            state: "installed",
            line: format!("already installed{whose}: {} -> {}", at.display(), from.display()),
            path: at,
            refused: false,
        }),
        // Ours, but pointing at another copy of the plugin — a working tree
        // after the package landed, say. Re-pointed rather than refused:
        // leaving somebody stuck is not caution, and nothing of theirs is at
        // risk.
        Found::Ours(target) => {
            std::fs::remove_file(&at).map_err(|e| failed("replace", &at, &e))?;
            link(from, &at)?;
            Ok(Report {
                agent: agent.name,
                state: "installed",
                line: format!(
                    "repointed{whose}: {} from {} to {}",
                    at.display(),
                    target.display(),
                    from.display()
                ),
                path: at,
                refused: false,
            })
        }
        Found::Theirs(what) => Ok(Report {
            agent: agent.name,
            state: "occupied",
            line: format!(
                "not installed{whose}: {} is already {what}, and not something Omacharts \
                 put there; move it aside and run this again",
                at.display()
            ),
            path: at,
            refused: true,
        }),
        Found::Nothing => {
            std::fs::create_dir_all(&agent.skills)
                .map_err(|e| failed("create", &agent.skills, &e))?;
            link(from, &at)?;
            Ok(Report {
                agent: agent.name,
                state: "installed",
                line: format!("installed{whose}: {} -> {}", at.display(), from.display()),
                path: at,
                refused: false,
            })
        }
    }
}

fn uninstall_one(agent: &Agent) -> Result<Report, Fault> {
    let at = agent.skills.join(NAME);
    let whose = agent.whose();
    match look(&at) {
        Found::Nothing => Ok(Report {
            agent: agent.name,
            state: "absent",
            line: format!("nothing of ours{whose} at {}", at.display()),
            path: at,
            refused: false,
        }),
        Found::Theirs(what) => Ok(Report {
            agent: agent.name,
            state: "occupied",
            line: format!(
                "left alone{whose}: {} is {what}, and not something Omacharts put there \
                 — remove it yourself if you meant to",
                at.display()
            ),
            path: at,
            refused: true,
        }),
        Found::Ours(_) => {
            std::fs::remove_file(&at).map_err(|e| failed("remove", &at, &e))?;
            // The directory too, but only if we were what was in it. Somebody
            // else's skills are not ours to tidy, and neither is the agent's
            // configuration directory itself.
            let emptied = std::fs::read_dir(&agent.skills)
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false);
            if emptied {
                let _ = std::fs::remove_dir(&agent.skills);
            }
            Ok(Report {
                agent: agent.name,
                state: "absent",
                line: format!("removed{whose}: {}", at.display()),
                path: at,
                refused: false,
            })
        }
    }
}

fn status_one(agent: &Agent) -> Result<Report, Fault> {
    let at = agent.skills.join(NAME);
    let whose = agent.whose();
    let (state, line) = match look(&at) {
        Found::Nothing => (
            "absent",
            format!(
                "not installed{whose}; `omacharts skill install` would write {}",
                at.display()
            ),
        ),
        Found::Theirs(what) => (
            "occupied",
            format!(
                "{} is {what}, and not something Omacharts put there{whose}",
                at.display()
            ),
        ),
        // The package was removed from under a link that outlived it. Says so
        // rather than reporting an install that loads nothing.
        Found::Ours(target) if !target.is_dir() => (
            "dangling",
            format!(
                "{} points at {}, which is not there any more; \
                 `omacharts skill uninstall` clears it",
                at.display(),
                target.display()
            ),
        ),
        Found::Ours(target) => (
            "installed",
            format!("installed{whose}: {} -> {}", at.display(), target.display()),
        ),
    };
    Ok(Report { agent: agent.name, state, path: at, line, refused: false })
}

/// Everything that happened, in whichever form was asked for.
///
/// A refusal for one agent and a success for another is the case worth getting
/// right: both are said, and the status reports that not everything asked for
/// happened.
fn answer(reports: Vec<Report>, from: Option<&Path>, as_json: bool) -> Outcome {
    let refused = reports.iter().any(|report| report.refused);
    let code = match refused {
        true => EXIT_REFUSED,
        false => EXIT_OK,
    };

    if as_json {
        let agents: Vec<serde_json::Value> = reports
            .iter()
            .map(|report| {
                serde_json::json!({
                    "agent": report.agent,
                    "state": report.state,
                    "path": report.path.to_string_lossy(),
                    "refused": report.refused,
                })
            })
            .collect();
        let body = serde_json::json!({
            "agents": agents,
            "source": from.map(|p| p.to_string_lossy()),
        });
        // One document either way, so a caller parses stdout and reads the
        // code, rather than having to stitch two streams together.
        return Outcome { code, out: format!("{body}\n"), err: String::new() };
    }

    let say = |want: bool| {
        reports
            .iter()
            .filter(|report| report.refused == want)
            .map(|report| report.line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let done = say(false);
    let stopped = say(true);
    Outcome {
        code,
        out: match done.is_empty() {
            true => String::new(),
            false => format!("{done}\n"),
        },
        err: match stopped.is_empty() {
            true => String::new(),
            false => format!("omacharts: {stopped}\n"),
        },
    }
}

/// Where the skill lives on this machine, packaged or in a working tree.
///
/// Walked out from the binary rather than compiled in, so the same binary works
/// from `/usr/bin` with the package's copy under `/usr/share`, and out of
/// `target/release` with the repo's own.
pub fn source() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(exe) = exe.as_deref().and_then(Path::parent) {
        for ancestor in exe.ancestors() {
            // A working tree: target/release/omacharts up to the repo root.
            roots.push(ancestor.join("agents"));
            // An install prefix: /usr/bin/omacharts beside /usr/share.
            roots.push(ancestor.join("share").join("omacharts").join("agents"));
        }
    }
    // The ordinary install, for a binary reached through a symlink on PATH
    // whose own location says nothing about where the package put its data.
    roots.push(PathBuf::from("/usr/share/omacharts/agents"));

    roots
        .into_iter()
        .map(|root| root.join(WITHIN_PLUGIN))
        .find(|dir| dir.join("SKILL.md").is_file())
}

/// What is at the install path now.
enum Found {
    Nothing,
    /// A link we put there, and what it points at.
    Ours(PathBuf),
    /// Somebody else's file or directory. Never touched.
    Theirs(&'static str),
}

fn look(at: &Path) -> Found {
    let Ok(meta) = at.symlink_metadata() else { return Found::Nothing };
    if !meta.is_symlink() {
        return Found::Theirs(if meta.is_dir() { "a directory" } else { "a file" });
    }
    match at.read_link() {
        Err(_) => Found::Theirs("a symlink that cannot be read"),
        Ok(target) => match is_ours(&target) {
            true => Found::Ours(target),
            false => Found::Theirs("a symlink to somewhere else"),
        },
    }
}

/// Does this path name a skill inside a directory Omacharts ships?
fn is_ours(target: &Path) -> bool {
    let tail: Vec<String> = target
        .components()
        .rev()
        .take(3)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let mut tail = tail.into_iter().rev();
    let (Some(dir), Some(skills), Some(name)) = (tail.next(), tail.next(), tail.next()) else {
        return false;
    };
    PLUGIN_DIRS.contains(&dir.as_str()) && skills == "skills" && name == NAME
}

fn link(from: &Path, at: &Path) -> Result<(), Fault> {
    std::os::unix::fs::symlink(from, at).map_err(|e| failed("link", at, &e))
}

/// The places `install` would have looked, named so that "none" is actionable.
fn looked_for() -> String {
    let mut parts: Vec<String> = KNOWN
        .iter()
        .map(|known| skills_of(known).parent().unwrap_or(Path::new("")).display().to_string())
        .collect();
    parts.push(format!(
        "and for {} on PATH",
        KNOWN.iter().map(|known| known.binary).collect::<Vec<_>>().join(" or ")
    ));
    parts.join(", ")
}

fn source_json() -> serde_json::Value {
    match source() {
        Some(path) => serde_json::Value::String(path.to_string_lossy().into_owned()),
        None => serde_json::Value::Null,
    }
}

fn missing_source() -> Fault {
    Fault::new(
        EXIT_ERROR,
        "the skill that ships with Omacharts is not on this machine; \
         expected /usr/share/omacharts/agents/skills/omacharts"
            .to_string(),
    )
}

fn failed(what: &str, path: &Path, error: &std::io::Error) -> Fault {
    Fault::new(EXIT_ERROR, format!("could not {what} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch skills directory. Never a real one: a test must not write into
    /// somebody's agent configuration any more than an installer may.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("omacharts-skill-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Both agents, pointed at scratch directories. This is how the Codex path
    /// is exercised without a Codex on the machine or a write to `~/.codex`.
    fn both(root: &Path) -> Vec<Agent> {
        KNOWN
            .iter()
            .map(|known| Agent {
                name: Some(known.name),
                skills: root.join(known.home_dir).join("skills"),
            })
            .collect()
    }

    /// The whole opt-in, for every agent, there and back again. The "and back
    /// again" is the half that matters: an opt-in people cannot reverse is one
    /// they should not have taken.
    #[test]
    fn the_skill_installs_for_every_agent_and_comes_back_out_leaving_nothing() {
        let root = scratch("roundtrip");
        let agents = both(&root);

        let out = run("install", &agents, false);
        assert_eq!(out.code, EXIT_OK, "{}", out.err);
        for agent in &agents {
            let at = agent.skills.join(NAME);
            assert!(at.symlink_metadata().unwrap().is_symlink(), "{}", at.display());
            assert!(at.join("SKILL.md").is_file(), "the link has to reach the skill");
            assert!(out.out.contains(&at.display().to_string()), "it has to say where: {}", out.out);
        }
        // Named, so somebody reading two lines knows which is which.
        assert!(out.out.contains("for Claude"), "{}", out.out);
        assert!(out.out.contains("for Codex"), "{}", out.out);

        let shown = run("status", &agents, false);
        assert_eq!(shown.out.lines().filter(|l| l.contains("installed")).count(), 2);

        let gone = run("uninstall", &agents, false);
        assert_eq!(gone.code, EXIT_OK, "{}", gone.err);
        for agent in &agents {
            assert!(agent.skills.join(NAME).symlink_metadata().is_err(), "nothing left");
            assert!(!agent.skills.exists(), "and not the directory we made either");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// One skill file, reached from both. Two copies would be two things to
    /// drift, and the drift is the whole risk.
    #[test]
    fn every_agent_is_pointed_at_the_very_same_file() {
        let root = scratch("shared");
        let agents = both(&root);
        run("install", &agents, false);

        let targets: Vec<PathBuf> = agents
            .iter()
            .map(|agent| std::fs::canonicalize(agent.skills.join(NAME)).unwrap())
            .collect();
        assert_eq!(targets[0], targets[1], "both agents must read one file");
        assert_eq!(targets[0], std::fs::canonicalize(source().unwrap()).unwrap());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Running it twice is not an error and does not stack up links.
    #[test]
    fn installing_twice_says_so_rather_than_failing() {
        let root = scratch("idempotent");
        let agents = both(&root);
        run("install", &agents, false);
        let again = run("install", &agents, false);
        assert_eq!(again.code, EXIT_OK, "{}", again.err);
        assert_eq!(again.out.matches("already installed").count(), 2, "{}", again.out);
        for agent in &agents {
            assert_eq!(std::fs::read_dir(&agent.skills).unwrap().count(), 1);
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Installed for one, refused for the other. The case most likely to be
    /// reported as a flat success or a flat failure, and both would be lies.
    #[test]
    fn one_agent_refused_does_not_hide_the_other_one_working() {
        let root = scratch("partial");
        let agents = both(&root);
        // Somebody's own skill of the same name, under the second agent only.
        let theirs = agents[1].skills.join(NAME);
        std::fs::create_dir_all(&theirs).unwrap();
        std::fs::write(theirs.join("SKILL.md"), "mine, not yours").unwrap();

        let out = run("install", &agents, false);
        assert_eq!(out.code, EXIT_REFUSED, "a refusal somewhere has to reach the exit status");
        assert!(out.out.contains("installed for Claude"), "the one that worked: {}", out.out);
        assert!(out.err.contains("not installed for Codex"), "and the one that did not: {}", out.err);
        assert!(out.err.contains("move it aside"), "{}", out.err);

        assert!(agents[0].skills.join(NAME).symlink_metadata().unwrap().is_symlink());
        assert_eq!(
            std::fs::read_to_string(theirs.join("SKILL.md")).unwrap(),
            "mine, not yours",
            "theirs has to survive"
        );

        // And uninstall will not take theirs away either, while still
        // clearing ours.
        let gone = run("uninstall", &agents, false);
        assert_eq!(gone.code, EXIT_REFUSED);
        assert!(gone.out.contains("removed for Claude"), "{}", gone.out);
        assert!(gone.err.contains("left alone for Codex"), "{}", gone.err);
        assert!(theirs.join("SKILL.md").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A machine with no agents on it is a reasonable state, not a failure —
    /// but the answer has to say what was looked for or it is unactionable.
    #[test]
    fn finding_no_agent_is_not_an_error_and_says_what_it_looked_for() {
        let out = run("install", &[], false);
        assert_eq!(out.code, EXIT_OK, "an installer must not scold a machine with no agents");
        assert!(out.err.is_empty());
        for known in KNOWN {
            assert!(out.out.contains(known.binary), "{} is not named: {}", known.binary, out.out);
        }
    }

    /// The package removed from under a link that outlived it. Reported as what
    /// it is, because an install that loads nothing is not an install.
    #[test]
    fn a_link_whose_package_has_gone_is_reported_as_dangling() {
        let root = scratch("dangling");
        let agents = both(&root);
        let gone = std::env::temp_dir().join("omacharts-gone/agents/skills/omacharts");
        for agent in &agents {
            std::fs::create_dir_all(&agent.skills).unwrap();
            std::os::unix::fs::symlink(&gone, agent.skills.join(NAME)).unwrap();
        }

        let shown = run("status", &agents, false);
        assert_eq!(shown.out.matches("not there any more").count(), 2, "{}", shown.out);

        // Ours, so uninstall clears it rather than refusing.
        assert_eq!(run("uninstall", &agents, false).code, EXIT_OK);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every answer is one JSON document, including a partial one. A caller
    /// that had to stitch stdout and stderr back together to find out what
    /// happened would be the reason to have given it a `--json` at all.
    #[test]
    fn the_json_answer_names_every_agent_and_survives_a_refusal() {
        let root = scratch("json");
        let agents = both(&root);
        let theirs = agents[1].skills.join(NAME);
        std::fs::create_dir_all(&theirs).unwrap();

        let out = run("install", &agents, true);
        assert_eq!(out.code, EXIT_REFUSED);
        assert!(out.err.is_empty(), "the whole answer belongs on stdout: {}", out.err);
        let parsed: serde_json::Value = serde_json::from_str(&out.out).unwrap();
        let listed = parsed["agents"].as_array().unwrap();
        assert_eq!(listed.len(), KNOWN.len());
        assert_eq!(listed[0]["agent"], "Claude");
        assert_eq!(listed[0]["refused"], false);
        assert_eq!(listed[1]["agent"], "Codex");
        assert_eq!(listed[1]["refused"], true);
        assert!(parsed["source"].is_string());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Only a link into a plugin directory counts as ours. Everything else is
    /// somebody's, and the difference is what keeps `uninstall` safe.
    #[test]
    fn only_a_link_into_a_plugin_directory_is_treated_as_ours() {
        assert!(is_ours(Path::new("/usr/share/omacharts/agents/skills/omacharts")));
        assert!(is_ours(Path::new("/home/x/src/omacharts/agents/skills/omacharts")));
        // The spelling one release used. Still ours, so it can still be removed.
        assert!(is_ours(Path::new("/usr/share/omacharts/claude-plugin/skills/omacharts")));
        assert!(!is_ours(Path::new("/home/x/my-skills/omacharts")));
        assert!(!is_ours(Path::new("/usr/share/other/agents/skills/other")));
        assert!(!is_ours(Path::new("/usr/share/other/plugins/skills/omacharts")));
    }

    /// `--to` names one directory and nothing else, and says so without an
    /// agent's name attached, because it does not belong to one.
    #[test]
    fn a_directory_named_outright_is_the_only_one_used() {
        let only = chosen(&[], Some("/tmp/elsewhere"));
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].skills, PathBuf::from("/tmp/elsewhere"));
        assert_eq!(only[0].name, None);
        assert_eq!(only[0].whose(), "");
    }

    /// Naming an agent means that agent, present or not. Somebody who typed
    /// `--codex` has said what they mean.
    #[test]
    fn an_agent_named_explicitly_is_used_whether_or_not_it_looks_present() {
        let named = chosen(&["codex"], None);
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].name, Some("Codex"));
        assert!(named[0].skills.ends_with("skills"));

        assert_eq!(chosen(&["claude", "codex"], None).len(), 2);
    }

    /// Each agent's own variable decides where it is, because each agent reads
    /// that variable itself. Anything else would put the skill where the agent
    /// is not looking.
    #[test]
    fn each_agent_is_found_through_its_own_configuration_variable() {
        for known in KNOWN {
            let expected = match std::env::var_os(known.home_var).filter(|v| !v.is_empty()) {
                Some(dir) => PathBuf::from(dir).join("skills"),
                None => crate::store::home().join(known.home_dir).join("skills"),
            };
            assert_eq!(skills_of(known), expected, "{}", known.name);
        }
    }

    /// Every agent in the table can be asked for by name on the command line.
    /// A row here with no flag in `spec` is an agent nobody can select.
    #[test]
    fn every_agent_has_a_flag_that_selects_it() {
        for verb in ["status", "install", "uninstall"] {
            let spec = super::super::spec::verb("skill", verb).expect("a skill verb");
            for known in KNOWN {
                assert!(
                    spec.flags.iter().any(|flag| flag.long == known.flag),
                    "`skill {verb}` has no --{} for {}",
                    known.flag,
                    known.name
                );
            }
        }
    }

    /// The skill the command installs has to be the one in this repo, because
    /// that is the one `every_command_in_the_agent_skill_is_a_command_that_runs`
    /// holds to the command surface.
    #[test]
    fn the_source_it_finds_is_the_skill_in_this_repo() {
        let found = source().expect("the repo's own agents directory");
        assert!(found.join("SKILL.md").is_file(), "{}", found.display());
    }

    /// Everything in the plugin is a file the package installs.
    ///
    /// `skill install` links at the directory, so a file the PKGBUILD forgot is
    /// not a missing file in a listing somewhere — it is a plugin that does not
    /// load on every machine that installed the package, while working
    /// perfectly in the tree it was written in. The files are named one by one
    /// in the PKGBUILD on purpose; this is what keeps that honest when another
    /// arrives.
    #[test]
    fn every_file_in_the_plugin_is_one_the_package_installs() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let pkgbuild = std::fs::read_to_string(root.join("packaging/aur/PKGBUILD")).unwrap();

        for file in files_under(&root.join("agents")) {
            let relative = file.strip_prefix(&root).unwrap().to_string_lossy().into_owned();
            assert!(
                pkgbuild.contains(&relative),
                "the PKGBUILD does not install {relative}, so it would be missing from the package"
            );
        }
    }

    fn files_under(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else { return out };
        for entry in entries.flatten() {
            let path = entry.path();
            match path.is_dir() {
                true => out.extend(files_under(&path)),
                false => out.push(path),
            }
        }
        out
    }
}
