//! Putting the agent skill where Claude will find it.
//!
//! Omacharts ships a skill — `claude-plugin/skills/omacharts/SKILL.md` — whose
//! job is discovery. `omacharts surface --json` already describes every
//! command perfectly, but only once something has thought to ask; and
//! `AGENTS.md` is only read by an agent already inside this repo. The skill is
//! what makes an agent anywhere on the machine reach for omacharts when
//! somebody says "what's semis doing".
//!
//! That leaves the question of how it gets there, and the answer is: only ever
//! because somebody asked for it, in as many words.
//!
//! **This never runs as a side effect.** Not from the package's post-install,
//! not from `bin/install`, not from the first launch. Somebody installing a
//! charting app has not agreed to have their agent's configuration written
//! into, and a charting app is not entitled to assume otherwise. It is one
//! explicit command, it says exactly which path it wrote, and
//! `skill uninstall` takes it back out — an opt-in nobody can reverse is one
//! they should not have been offered.
//!
//! **A symlink, not a copy.** The risk this whole thing is designed against is
//! a skill that drifts from the CLI, because a confidently wrong skill is
//! worse than none: the file is version-controlled beside the surface it
//! describes, and a test runs its examples. A symlink into
//! `/usr/share/omacharts` keeps that true through every package upgrade, where
//! a copy would quietly go stale the first time the surface changed. The cost
//! is that removing the package leaves a dangling link — which is the right
//! way round to fail. A dangling symlink loads nothing and `skill status`
//! names it; a stale copy keeps answering, wrongly.

use std::path::{Path, PathBuf};

use super::{Fault, EXIT_ERROR};

/// The skill's directory inside the plugin, relative to the plugin root.
const WITHIN_PLUGIN: &str = "skills/omacharts";

/// What the skill is called where it is installed, and inside the plugin.
const NAME: &str = "omacharts";

/// The last components of a path that is one of ours.
///
/// How `install` and `uninstall` tell a link they put there from whatever else
/// somebody may have at that name. Anything that is not this is left alone and
/// reported, rather than replaced: the one thing worse than not installing is
/// deleting somebody's own skill of the same name.
const OURS: [&str; 3] = ["claude-plugin", "skills", NAME];

/// Where the skill lives on this machine, packaged or in a working tree.
///
/// Walked out from the binary rather than compiled in, so the same binary
/// works from `/usr/bin` with the package's copy under `/usr/share`, and out
/// of `target/release` with the repo's own.
pub fn source() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(exe) = exe.as_deref().and_then(Path::parent) {
        for ancestor in exe.ancestors() {
            // A working tree: target/release/omacharts up to the repo root.
            roots.push(ancestor.join("claude-plugin"));
            // An install prefix: /usr/bin/omacharts beside /usr/share.
            roots.push(ancestor.join("share").join("omacharts").join("claude-plugin"));
        }
    }
    // The ordinary install, for a binary reached through a symlink on PATH
    // whose own location says nothing about where the package put its data.
    roots.push(PathBuf::from("/usr/share/omacharts/claude-plugin"));

    roots.into_iter().map(|root| root.join(WITHIN_PLUGIN)).find(|dir| dir.join("SKILL.md").is_file())
}

/// The skills directory to act on.
///
/// `--to` first, so a test and a person with a project-scoped
/// `.claude/skills` are served by the same path; then `CLAUDE_CONFIG_DIR`,
/// which is Claude Code's own variable and the only honest way to find a
/// configuration that is not in the usual place.
pub fn skills_dir(to: Option<&str>) -> PathBuf {
    if let Some(dir) = to.filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::store::home().join(".claude"));
    base.join("skills")
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

/// Does this path name a skill inside an Omacharts plugin directory?
fn is_ours(target: &Path) -> bool {
    let tail: Vec<String> = target
        .components()
        .rev()
        .take(OURS.len())
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    tail.iter().rev().map(String::as_str).eq(OURS)
}

/// Link the skill into a skills directory.
pub fn install(skills: &Path, as_json: bool) -> Result<String, Fault> {
    let from = source().ok_or_else(missing_source)?;
    let at = skills.join(NAME);

    match look(&at) {
        Found::Ours(target) if target == from => {
            return said(as_json, "installed", &at, Some(&from), format!(
                "already installed: {} -> {}",
                at.display(),
                from.display()
            ));
        }
        // Ours, but pointing at another copy of the plugin — a working tree
        // after the package landed, say. Re-pointed rather than refused:
        // leaving somebody stuck is not caution, and nothing of theirs is at
        // risk.
        Found::Ours(target) => {
            std::fs::remove_file(&at).map_err(|e| failed("replace", &at, &e))?;
            link(&from, &at)?;
            return said(as_json, "installed", &at, Some(&from), format!(
                "repointed {} from {} to {}",
                at.display(),
                target.display(),
                from.display()
            ));
        }
        Found::Theirs(what) => {
            return Err(Fault::refused(format!(
                "{} is already {}, and not something Omacharts put there; \
                 move it aside and run this again",
                at.display(),
                what
            )))
        }
        Found::Nothing => {}
    }

    std::fs::create_dir_all(skills).map_err(|e| failed("create", skills, &e))?;
    link(&from, &at)?;
    said(as_json, "installed", &at, Some(&from), format!("{} -> {}", at.display(), from.display()))
}

/// Take it back out, leaving nothing behind.
pub fn uninstall(skills: &Path, as_json: bool) -> Result<String, Fault> {
    let at = skills.join(NAME);
    match look(&at) {
        Found::Nothing => {
            said(as_json, "absent", &at, None, format!("nothing of ours at {}", at.display()))
        }
        Found::Theirs(what) => Err(Fault::refused(format!(
            "{} is {}, and not something Omacharts put there; \
             it has been left alone — remove it yourself if you meant to",
            at.display(),
            what
        ))),
        Found::Ours(_) => {
            std::fs::remove_file(&at).map_err(|e| failed("remove", &at, &e))?;
            // The directory too, but only if we are what was in it. Somebody
            // else's skills are not ours to tidy.
            let emptied = std::fs::read_dir(skills).map(|mut d| d.next().is_none()).unwrap_or(false);
            if emptied {
                let _ = std::fs::remove_dir(skills);
            }
            said(as_json, "absent", &at, None, format!("removed {}", at.display()))
        }
    }
}

/// Where the skill is, where it would go, and whether the two are joined up.
pub fn status(skills: &Path, as_json: bool) -> Result<String, Fault> {
    let from = source();
    let at = skills.join(NAME);
    let (state, text) = match look(&at) {
        Found::Nothing => (
            "absent",
            format!("not installed; `omacharts skill install` would write {}", at.display()),
        ),
        Found::Theirs(what) => (
            "occupied",
            format!("{} is {}, and not something Omacharts put there", at.display(), what),
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
        Found::Ours(target) => {
            ("installed", format!("installed: {} -> {}", at.display(), target.display()))
        }
    };
    said(as_json, state, &at, from.as_deref(), text)
}

fn link(from: &Path, at: &Path) -> Result<(), Fault> {
    std::os::unix::fs::symlink(from, at).map_err(|e| failed("link", at, &e))
}

/// Every answer carries the two paths, because "it worked" is not an answer
/// somebody can check and an agent cannot look at the screen to see where.
fn said(
    as_json: bool,
    state: &str,
    at: &Path,
    source: Option<&Path>,
    text: String,
) -> Result<String, Fault> {
    match as_json {
        false => Ok(format!("{text}\n")),
        true => Ok(format!(
            "{}\n",
            serde_json::json!({
                "state": state,
                "path": at.to_string_lossy(),
                "source": source.map(|s| s.to_string_lossy()),
            })
        )),
    }
}

fn missing_source() -> Fault {
    Fault::new(
        EXIT_ERROR,
        "the skill that ships with Omacharts is not on this machine; \
         expected /usr/share/omacharts/claude-plugin/skills/omacharts"
            .to_string(),
    )
}

fn failed(what: &str, path: &Path, error: &std::io::Error) -> Fault {
    Fault::new(EXIT_ERROR, format!("could not {what} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch skills directory. Never the real one: a test must not write
    /// into somebody's agent configuration any more than an installer may.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omacharts-skill-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("skills")
    }

    /// The whole opt-in, there and back again. The "and back again" is the
    /// half that matters: an opt-in people cannot reverse is one they should
    /// not have taken.
    #[test]
    fn the_skill_can_be_installed_and_removed_leaving_nothing_behind() {
        let skills = scratch("roundtrip");
        let at = skills.join(NAME);

        let out = install(&skills, false).expect("a source to link");
        assert!(out.contains(&at.display().to_string()), "it has to say where: {out}");
        assert!(at.symlink_metadata().unwrap().is_symlink());
        assert!(at.join("SKILL.md").is_file(), "the link has to reach the skill");

        assert!(status(&skills, false).unwrap().contains("installed:"));

        assert!(uninstall(&skills, false).unwrap().contains("removed"));
        assert!(at.symlink_metadata().is_err(), "nothing left at the path");
        assert!(!skills.exists(), "and not the directory we made either");
    }

    /// Running it twice is not an error and does not stack up links.
    #[test]
    fn installing_twice_says_so_rather_than_failing() {
        let skills = scratch("idempotent");
        install(&skills, false).unwrap();
        let again = install(&skills, false).unwrap();
        assert!(again.contains("already installed"), "{again}");
        assert_eq!(std::fs::read_dir(&skills).unwrap().count(), 1);
        uninstall(&skills, false).unwrap();
    }

    /// Somebody's own `omacharts` skill is not ours to overwrite, and a
    /// refusal that does not say what to do about it is half a refusal.
    #[test]
    fn a_skill_already_there_that_is_not_ours_is_refused_rather_than_clobbered() {
        let skills = scratch("occupied");
        let at = skills.join(NAME);
        std::fs::create_dir_all(&at).unwrap();
        std::fs::write(at.join("SKILL.md"), "mine, not yours").unwrap();

        let fault = install(&skills, false).expect_err("a refusal");
        assert_eq!(fault.code, super::super::EXIT_REFUSED);
        assert!(fault.message.contains("move it aside"), "{}", fault.message);
        assert_eq!(
            std::fs::read_to_string(at.join("SKILL.md")).unwrap(),
            "mine, not yours",
            "theirs has to survive"
        );

        // And uninstall will not take it away either.
        let fault = uninstall(&skills, false).expect_err("a refusal");
        assert_eq!(fault.code, super::super::EXIT_REFUSED);
        assert!(at.join("SKILL.md").is_file());
        let _ = std::fs::remove_dir_all(skills.parent().unwrap());
    }

    /// The package removed from under a link that outlived it. Reported as
    /// what it is, because an install that loads nothing is not an install.
    #[test]
    fn a_link_whose_package_has_gone_is_reported_as_dangling() {
        let skills = scratch("dangling");
        std::fs::create_dir_all(&skills).unwrap();
        let gone = std::env::temp_dir().join("omacharts-gone/claude-plugin/skills/omacharts");
        std::os::unix::fs::symlink(&gone, skills.join(NAME)).unwrap();

        let out = status(&skills, false).unwrap();
        assert!(out.contains("not there any more"), "{out}");

        // Ours, so uninstall clears it rather than refusing.
        assert!(uninstall(&skills, false).unwrap().contains("removed"));
        let _ = std::fs::remove_dir_all(skills.parent().unwrap());
    }

    /// Only a link into a plugin directory counts as ours. Everything else is
    /// somebody's, and the difference is what keeps `uninstall` safe.
    #[test]
    fn only_a_link_into_a_plugin_directory_is_treated_as_ours() {
        assert!(is_ours(Path::new("/usr/share/omacharts/claude-plugin/skills/omacharts")));
        assert!(is_ours(Path::new("/home/x/src/omacharts/claude-plugin/skills/omacharts")));
        assert!(!is_ours(Path::new("/home/x/my-skills/omacharts")));
        assert!(!is_ours(Path::new("/usr/share/other/claude-plugin/skills/other")));
    }

    /// `--to` beats the environment, and the default is Claude's own place.
    #[test]
    fn the_destination_is_claudes_skills_directory_unless_told_otherwise() {
        assert_eq!(skills_dir(Some("/tmp/elsewhere")), PathBuf::from("/tmp/elsewhere"));
        let home = crate::store::home();
        match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
            None => assert_eq!(skills_dir(None), home.join(".claude").join("skills")),
            Some(dir) => assert_eq!(skills_dir(None), PathBuf::from(dir).join("skills")),
        }
    }

    /// The skill the command installs has to be the one in this repo, because
    /// that is the one `every_command_in_the_agent_skill_is_a_command_that_runs`
    /// holds to the command surface.
    #[test]
    fn the_source_it_finds_is_the_skill_in_this_repo() {
        let found = source().expect("the repo's own claude-plugin");
        assert!(found.join("SKILL.md").is_file(), "{}", found.display());
    }

    /// Everything in the plugin is a file the package installs.
    ///
    /// `skill install` links at the directory, so a file the PKGBUILD forgot
    /// is not a missing file in a listing somewhere — it is a plugin that does
    /// not load on every machine that installed the package, while working
    /// perfectly in the tree it was written in. The three files are named one
    /// by one in the PKGBUILD on purpose; this is what keeps that honest when
    /// a fourth arrives.
    #[test]
    fn every_file_in_the_plugin_is_one_the_package_installs() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let pkgbuild = std::fs::read_to_string(root.join("packaging/aur/PKGBUILD")).unwrap();

        for file in files_under(&root.join("claude-plugin")) {
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
