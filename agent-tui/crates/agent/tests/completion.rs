//! `agent complete` and the scripts `agent completions` prints, run against
//! a scratch home, state, worktrees root and a fake `zellij`. Nothing here
//! reads the user's state or session host.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const AGENT: &str = env!("CARGO_BIN_EXE_agent");

struct Scratch {
    dir: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("a temp dir");
        let scratch = Self { dir };
        for agent in ["api/fix", "api/old", "api/idle", "web/zed", "ta\tb/odd"] {
            std::fs::create_dir_all(scratch.path("wt").join(agent).join(".git")).expect("creates");
        }
        std::fs::create_dir_all(scratch.path("state/agent/records")).expect("creates");
        std::fs::write(
            scratch.path("state/agent/records/solo.json"),
            r#"{"harness":"claude"}"#,
        )
        .expect("writes");
        std::fs::write(scratch.path("state/agent/pinned"), "pinned-one\n").expect("writes");
        std::fs::create_dir_all(scratch.path("bin")).expect("creates");
        let zellij = scratch.path("bin/zellij");
        std::fs::write(
            &zellij,
            "#!/bin/sh\nprintf 'fix [Created 1h ago]\\nold [Created 1h ago] (EXITED - attach to resurrect)\\nstray [Created 1h ago]\\ngone [Created 1h ago] (EXITED - attach to resurrect)\\n'\n",
        )
        .expect("writes");
        Command::new("chmod")
            .arg("+x")
            .arg(&zellij)
            .status()
            .expect("chmods");
        std::fs::create_dir_all(scratch.path("zj")).expect("creates");
        std::fs::create_dir_all(scratch.path("home")).expect("creates");
        scratch
    }

    fn path(&self, rest: &str) -> PathBuf {
        self.dir.path().join(rest)
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let agent_dir = Path::new(AGENT).parent().expect("a directory");
        let path = format!(
            "{}:{}:{}",
            self.path("bin").display(),
            agent_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new(program);
        cmd.env("HOME", self.path("home"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("AGENT_WORKTREES", self.path("wt"))
            .env("ZELLIJ_SOCKET_DIR", self.path("zj"))
            .env_remove("ZELLIJ_SESSION_NAME")
            .env("PATH", path)
            .current_dir(self.dir.path());
        cmd
    }

    fn complete(&self, args: &[&str]) -> String {
        let out = self
            .command(AGENT)
            .arg("complete")
            .args(args)
            .output()
            .expect("runs");
        assert!(out.status.success(), "{}", stderr(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn script(&self, shell: &str) -> String {
        let out = self
            .command(AGENT)
            .args(["completions", shell])
            .output()
            .expect("runs");
        assert!(out.status.success(), "{}", stderr(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Whether `shell` can be run here. Without it the test says it skipped,
/// unless `AGENT_TEST_SHELLS` is set, where a missing shell fails the test.
fn shell_or_skip(shell: &str) -> bool {
    if Command::new(shell).arg("--version").output().is_ok() {
        return true;
    }
    assert!(
        std::env::var_os("AGENT_TEST_SHELLS").is_none(),
        "AGENT_TEST_SHELLS is set but {shell} isn't installed"
    );
    eprintln!("skipped: {shell} isn't installed (set AGENT_TEST_SHELLS=1 to require it)");
    false
}

#[test]
fn live_agents_are_listed_first_then_exited_then_those_without_a_session() {
    let scratch = Scratch::new();
    let listed = scratch.complete(&["agents", "--notes"]);
    assert_eq!(
        listed,
        "fix\tlive · api\n\
         old\texited · api\n\
         idle\tno session · api\n\
         odd\tno session · ta b\n\
         solo\tno session\n\
         zed\tno session · web\n"
    );
}

#[test]
fn without_notes_each_line_is_the_bare_name_and_none_holds_a_tab() {
    let scratch = Scratch::new();
    let listed = scratch.complete(&["agents"]);
    assert_eq!(
        listed.lines().collect::<Vec<_>>(),
        ["fix", "old", "idle", "odd", "solo", "zed"]
    );
    assert!(!listed.contains('\t'));
}

#[test]
fn an_unreachable_host_leaves_every_agent_unknown_and_listed_anyway() {
    let scratch = Scratch::new();
    std::fs::write(scratch.path("bin/zellij"), "#!/bin/sh\nexit 3\n").expect("writes");
    let listed = scratch.complete(&["agents", "--notes"]);
    assert!(listed.contains("fix\tunknown · api\n"), "{listed}");
    assert!(!listed.contains("live"));
}

#[test]
fn host_sessions_and_pins_that_match_no_known_agent_are_not_offered() {
    let listed = Scratch::new().complete(&["agents"]);
    for foreign in ["stray", "gone", "pinned-one"] {
        assert!(
            !listed.lines().any(|l| l == foreign),
            "{foreign} in {listed}"
        );
    }
}

#[test]
fn a_hung_session_host_costs_the_states_and_not_the_prompt() {
    let scratch = Scratch::new();
    std::fs::write(scratch.path("bin/zellij"), "#!/bin/sh\nexec sleep 30\n").expect("writes");
    let started = std::time::Instant::now();
    let out = scratch
        .command(AGENT)
        .args(["complete", "agents", "--notes"])
        .output()
        .expect("runs");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(out.status.success(), "{}", stderr(&out));
    let listed = String::from_utf8_lossy(&out.stdout);
    assert!(listed.contains("fix\tunknown · api\n"), "{listed}");
    assert!(stderr(&out).contains("agent complete:"), "{}", stderr(&out));
}

#[test]
fn harnesses_are_the_registrys_ids() {
    assert_eq!(Scratch::new().complete(&["harnesses"]), "claude\n");
}

#[test]
fn repos_are_the_listed_ones_then_those_the_config_finds() {
    let scratch = Scratch::new();
    let listed = scratch.path("listed");
    std::fs::create_dir_all(listed.join(".git")).expect("creates");
    std::fs::write(
        scratch.path("state/agent/repos"),
        format!("{}\n", listed.display()),
    )
    .expect("writes");
    let clones = scratch.path("clones");
    std::fs::create_dir_all(clones.join("found/.git")).expect("creates");
    std::fs::create_dir_all(scratch.path("config/agent")).expect("creates");
    std::fs::write(
        scratch.path("config/agent/config.toml"),
        format!("[repos]\ndirs = [\"{}\"]\n", clones.display()),
    )
    .expect("writes");
    let found = clones.join("found").canonicalize().expect("exists");
    assert_eq!(
        scratch.complete(&["repos", "--notes"]),
        format!("{}\tlisted\n{}\tfound\n", listed.display(), found.display())
    );
}

#[test]
fn branches_are_those_of_the_repo_containing_the_cwd() {
    let scratch = Scratch::new();
    let repo = scratch.path("repo");
    std::fs::create_dir_all(&repo).expect("creates");
    let git = |args: &[&str]| {
        let status = scratch
            .command("git")
            .current_dir(&repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .status()
            .expect("runs");
        assert!(status.success(), "{args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["commit", "-q", "--allow-empty", "-m", "init"]);
    git(&["branch", "feature/x"]);
    let out = scratch
        .command(AGENT)
        .args(["complete", "branches", "--notes"])
        .current_dir(&repo)
        .output()
        .expect("runs");
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "feature/x\tlocal\nmain\tlocal\n"
    );
}

#[test]
fn outside_a_repo_there_are_no_branches() {
    assert_eq!(Scratch::new().complete(&["branches"]), "");
}

#[test]
fn hidden_subcommands_are_in_none_of_the_scripts() {
    let scratch = Scratch::new();
    for shell in ["fish", "bash", "zsh", "elvish", "powershell"] {
        let script = scratch.script(shell);
        for hidden in ["complete", "exec", "sweep"] {
            let offered = script.lines().any(|line| {
                line.contains(&format!("-a \"{hidden}\""))
                    || line.contains(&format!("'{hidden}'"))
                    || line.trim_start().starts_with(&format!("{hidden})"))
                    || line.contains(&format!("\"{hidden}\""))
                    || line.contains(&format!("{hidden}:"))
            });
            assert!(!offered, "{shell} offers {hidden}");
        }
        assert!(
            script.contains("attach"),
            "{shell} kept the static commands"
        );
    }
    let bash = scratch.script("bash");
    let opts = bash
        .lines()
        .find(|l| l.contains("opts=\"-h -V --harness"))
        .expect("top-level opts");
    assert!(!opts.contains("complete"), "{opts}");
}

#[test]
fn complete_is_hidden_from_help() {
    let scratch = Scratch::new();
    let help = scratch.command(AGENT).arg("--help").output().expect("runs");
    assert!(!String::from_utf8_lossy(&help.stdout).contains("complete  "));
}

const ATTACH_LIKE: &str = "attach rm restart pin unpin dump prompt";

#[test]
fn the_fish_script_keeps_the_static_entries_and_adds_a_dynamic_one_per_kind() {
    let script = Scratch::new().script("fish");
    assert!(script.contains("function __fish_agent_using_subcommand"));
    assert!(script.contains("-l headless -d"));
    assert!(script.contains(&format!(
        "complete -c agent -n \"__fish_agent_using_subcommand {ATTACH_LIKE}; and __fish_agent_wants_word\" -f -k -a \"(agent complete agents --notes 2>/dev/null)\""
    )));
    assert!(script.contains(
        "complete -c agent -n \"__fish_agent_using_subcommand checkout co; and __fish_agent_wants_word\" -f -k -a \"(__fish_agent_branches)\""
    ));
    assert!(script.contains(
        "complete -c agent -n \"__fish_agent_using_subcommand repo attach checkout co\" -l repo -r -a \"(agent complete repos --notes 2>/dev/null)\""
    ));
    assert!(script.contains(
        "complete -c agent -l harness -x -a \"(agent complete harnesses --notes 2>/dev/null)\""
    ));
}

#[test]
fn bash_and_zsh_scripts_wrap_the_static_function_and_name_each_kind() {
    let scratch = Scratch::new();
    for shell in ["bash", "zsh"] {
        let script = scratch.script(shell);
        assert!(script.contains("_agent() {"), "{shell}");
        assert!(script.contains("_agent_dynamic() {"), "{shell}");
        assert!(
            script.contains(&format!(
                "{}) (( positionals == 0 )) && kind=agents",
                ATTACH_LIKE.replace(' ', "|")
            )),
            "{shell}"
        );
        assert!(
            script.contains("checkout|co) (( positionals == 0 )) && kind=branches"),
            "{shell}"
        );
        assert!(script.contains("--repo) kind=repos"), "{shell}");
        assert!(script.contains("--harness) kind=harnesses"), "{shell}");
    }
}

#[test]
fn elvish_and_powershell_scripts_stay_static() {
    let scratch = Scratch::new();
    for shell in ["elvish", "powershell"] {
        assert!(!scratch.script(shell).contains("agent complete"), "{shell}");
    }
}

fn syntax_check(shell: &str, flag: &str) {
    if !shell_or_skip(shell) {
        return;
    }
    let scratch = Scratch::new();
    let file = scratch.path(&format!("agent.{shell}"));
    std::fs::write(&file, scratch.script(shell)).expect("writes");
    let out = Command::new(shell)
        .arg(flag)
        .arg(&file)
        .output()
        .expect("runs");
    assert!(out.status.success(), "{shell}: {}", stderr(&out));
}

#[test]
fn the_bash_script_passes_a_syntax_check() {
    syntax_check("bash", "-n");
}

#[test]
fn the_zsh_script_passes_a_syntax_check() {
    syntax_check("zsh", "-n");
}

#[test]
fn the_fish_script_passes_a_syntax_check() {
    syntax_check("fish", "--no-execute");
}

fn fish_offers(scratch: &Scratch, line: &str) -> Vec<String> {
    let out = scratch
        .command("fish")
        .args(["--no-config", "-c"])
        .arg(format!(
            "agent completions fish | source; complete -C '{line}'"
        ))
        .output()
        .expect("runs");
    assert!(out.status.success(), "{}", stderr(&out));
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(String::from)
        .collect()
}

#[test]
fn fish_offers_names_live_first_where_a_name_is_next_and_not_after_it() {
    if !shell_or_skip("fish") {
        return;
    }
    let scratch = Scratch::new();
    let names: Vec<_> = fish_offers(&scratch, "agent attach ")
        .into_iter()
        .map(|line| line.split('\t').next().unwrap_or_default().to_string())
        .collect();
    assert_eq!(names, ["fix", "old", "idle", "odd", "solo", "zed"]);
    assert!(fish_offers(&scratch, "agent attach -e hi f")[0].starts_with("fix\tlive · api"));
    let after_name = fish_offers(&scratch, "agent attach fix ");
    assert!(
        !after_name.iter().any(|l| l.starts_with("old")),
        "{after_name:?}"
    );
    let prompt_text = fish_offers(&scratch, "agent prompt fix ");
    assert!(
        !prompt_text.iter().any(|l| l.starts_with("old")),
        "{prompt_text:?}"
    );
    for subcommand in ["rm", "restart", "pin", "unpin", "dump", "prompt"] {
        let offered = fish_offers(&scratch, &format!("agent {subcommand} "));
        assert!(
            offered.iter().any(|l| l.starts_with("fix\tlive")),
            "{subcommand}"
        );
    }
}

#[test]
fn fish_still_offers_flags_and_the_harness_ids() {
    if !shell_or_skip("fish") {
        return;
    }
    let scratch = Scratch::new();
    let flags = fish_offers(&scratch, "agent attach --");
    assert!(flags.iter().any(|l| l.starts_with("--seed")), "{flags:?}");
    assert!(!flags.iter().any(|l| l.starts_with("fix")), "{flags:?}");
    assert_eq!(fish_offers(&scratch, "agent --harness "), ["claude"]);
    assert_eq!(
        fish_offers(&scratch, "agent attach x --harness "),
        ["claude"]
    );
}

#[test]
fn bash_completes_the_name_after_attach_and_the_value_after_repo() {
    if !shell_or_skip("bash") {
        return;
    }
    let scratch = Scratch::new();
    let file = scratch.path("agent.bash");
    std::fs::write(&file, scratch.script("bash")).expect("writes");
    let offered = |words: &str, cword: usize| {
        let out = scratch
            .command("bash")
            .arg("-c")
            .arg(format!(
                "source '{}'; COMP_WORDS=({words}); COMP_CWORD={cword}; _agent_dynamic agent '' x; printf '%s\\n' \"${{COMPREPLY[@]}}\"",
                file.display()
            ))
            .output()
            .expect("runs");
        assert!(out.status.success(), "{}", stderr(&out));
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(String::from)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        offered("agent attach ''", 2),
        ["fix", "old", "idle", "odd", "solo", "zed"]
    );
    assert_eq!(offered("agent attach f", 2), ["fix"]);
    assert!(!offered("agent attach fix ''", 3).iter().any(|l| l == "old"));
    assert_eq!(offered("agent --harness ''", 2), ["claude"]);
}

#[test]
fn zsh_describes_the_name_after_attach_with_its_note_and_falls_back_after_it() {
    if !shell_or_skip("zsh") {
        return;
    }
    let scratch = Scratch::new();
    let file = scratch.path("agent.zsh");
    std::fs::write(&file, scratch.script("zsh")).expect("writes");
    let run = |words: &str, current: usize| {
        let out = scratch
            .command("zsh")
            .arg("-f")
            .arg("-c")
            .arg(format!(
                "compdef() {{ }}; source '{}'; _agent() {{ print FALLBACK }}; _describe() {{ print -l -- \"${{(@P)${{@[-1]}}}}\" }}; words=({words}); CURRENT={current}; _agent_dynamic",
                file.display()
            ))
            .output()
            .expect("runs");
        assert!(out.status.success(), "{}", stderr(&out));
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(String::from)
            .collect::<Vec<_>>()
    };
    let names = run("agent attach ''", 3);
    assert_eq!(names[0], "fix:live · api");
    assert_eq!(names[1], "old:exited · api");
    assert_eq!(run("agent attach fix ''", 4), ["FALLBACK"]);
    assert_eq!(run("agent --harness ''", 3), ["claude"]);
    assert_eq!(run("agent attach -", 3), ["FALLBACK"]);
}

fn list_repos(scratch: &Scratch, names: &[&str]) -> Vec<PathBuf> {
    let paths: Vec<PathBuf> = names
        .iter()
        .map(|name| scratch.path("repos").join(name))
        .collect();
    for path in &paths {
        std::fs::create_dir_all(path.join(".git")).expect("creates");
    }
    let contents = paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(scratch.path("state/agent/repos"), contents).expect("writes");
    paths
}

fn bash_offers(scratch: &Scratch, words: &str, cword: usize) -> Vec<String> {
    let file = scratch.path("agent.bash");
    std::fs::write(&file, scratch.script("bash")).expect("writes");
    let out = scratch
        .command("bash")
        .arg("-c")
        .arg(format!(
            "source '{}'; COMP_WORDS=({words}); COMP_CWORD={cword}; _agent_dynamic agent '' x; printf '%s\\n' \"${{COMPREPLY[@]}}\"",
            file.display()
        ))
        .output()
        .expect("runs");
    assert!(out.status.success(), "{}", stderr(&out));
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(String::from)
        .collect()
}

#[test]
fn bash_offers_repo_paths_with_glob_characters_and_spaces_exactly_once_and_escaped() {
    if !shell_or_skip("bash") {
        return;
    }
    let scratch = Scratch::new();
    let paths = list_repos(&scratch, &["a[1]", "my repo", "wild*", "wild2"]);
    let all = bash_offers(&scratch, "agent attach --repo ''", 3);
    assert_eq!(all.len(), 4, "{all:?}");
    for (offered, path) in all.iter().zip(&paths) {
        let escaped = path
            .to_str()
            .expect("utf8")
            .replace('[', "\\[")
            .replace(' ', "\\ ")
            .replace('*', "\\*")
            .replace(']', "\\]");
        assert_eq!(offered, &escaped);
    }
    let typed = format!("{}/my\\\\ re", scratch.path("repos").display());
    let narrowed = bash_offers(&scratch, &format!("agent attach --repo {typed}"), 3);
    assert_eq!(narrowed.len(), 1, "{narrowed:?}");
    assert!(narrowed[0].ends_with("my\\ repo"), "{narrowed:?}");
}

#[test]
fn bash_treats_what_follows_a_double_dash_as_positionals() {
    if !shell_or_skip("bash") {
        return;
    }
    let scratch = Scratch::new();
    assert!(
        bash_offers(&scratch, "agent attach -- ''", 3)
            .iter()
            .any(|l| l == "fix")
    );
    assert!(
        bash_offers(&scratch, "agent attach -- x ''", 4)
            .iter()
            .all(|l| l != "fix")
    );
}

#[test]
fn fish_counts_what_follows_a_double_dash_as_positionals() {
    if !shell_or_skip("fish") {
        return;
    }
    let scratch = Scratch::new();
    let after_flag_like = fish_offers(&scratch, "agent attach -- -x ");
    assert!(
        !after_flag_like.iter().any(|l| l.starts_with("fix")),
        "{after_flag_like:?}"
    );
    let before = fish_offers(&scratch, "agent attach -- ");
    assert!(
        before.iter().any(|l| l.starts_with("fix\tlive")),
        "{before:?}"
    );
}

#[test]
fn fish_lists_the_branches_of_the_repo_named_by_a_typed_repo_flag() {
    if !shell_or_skip("fish") {
        return;
    }
    let scratch = Scratch::new();
    let repo = scratch.path("repo");
    std::fs::create_dir_all(&repo).expect("creates");
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["commit", "-q", "--allow-empty", "-m", "init"],
        vec!["branch", "elsewhere"],
    ] {
        let status = scratch
            .command("git")
            .current_dir(&repo)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .status()
            .expect("runs");
        assert!(status.success());
    }
    let offered = fish_offers(
        &scratch,
        &format!("agent checkout --repo {} ", repo.display()),
    );
    assert_eq!(offered, ["elsewhere\tlocal", "main\tlocal"]);
    let offered = fish_offers(
        &scratch,
        &format!("agent checkout --repo={} ", repo.display()),
    );
    assert_eq!(offered, ["elsewhere\tlocal", "main\tlocal"]);
}

#[test]
fn zsh_with_nothing_to_list_falls_back_instead_of_offering_an_empty_name() {
    if !shell_or_skip("zsh") {
        return;
    }
    let scratch = Scratch::new();
    std::fs::remove_dir_all(scratch.path("wt")).expect("removes");
    std::fs::remove_dir_all(scratch.path("state/agent/records")).expect("removes");
    std::fs::write(scratch.path("bin/zellij"), "#!/bin/sh\nexit 0\n").expect("writes");
    let file = scratch.path("agent.zsh");
    std::fs::write(&file, scratch.script("zsh")).expect("writes");
    let out = scratch
        .command("zsh")
        .arg("-f")
        .arg("-c")
        .arg(format!(
            "compdef() {{ }}; source '{}'; _agent() {{ print FALLBACK }}; _describe() {{ print DESCRIBED }}; words=(agent attach ''); CURRENT=3; _agent_dynamic",
            file.display()
        ))
        .output()
        .expect("runs");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "FALLBACK\n");
}
