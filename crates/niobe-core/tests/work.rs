// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! Shell commands and tool names that are easy to misread, each read as the
//! program that did the work and the kind of work it did.

use niobe_core::work::{Work, of_call, of_command};

fn read(command: &str) -> (Work, Vec<String>) {
    let reading = of_command(command);
    (reading.work, reading.specifics)
}

fn named(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn reads_as(table: &[(&str, Work, &[&str])]) {
    for (command, work, specifics) in table {
        assert_eq!(
            read(command),
            (work.clone(), named(specifics)),
            "{command:?}"
        );
    }
}

#[test]
fn an_option_that_takes_a_value_is_not_read_as_an_in_place_edit() {
    reads_as(&[
        ("perl -Mstrict -e 'print 1'", Work::Other, &["script"]),
        ("perl -Mwarnings -ne 'print' f", Work::Other, &["script"]),
        ("perl -I lib -e 'print 1'", Work::Other, &["script"]),
        ("sed -e 's/i/x/' f", Work::Exploring, &["sed"]),
        ("perl -pi -e 's/a/b/' f.rs", Work::Editing, &["perl -i"]),
        ("perl -i -pe 's/a/b/' f.rs", Work::Editing, &["perl -i"]),
        ("sed -Ei 's/a/b/' f.rs", Work::Editing, &["sed -i"]),
    ]);
}

#[test]
fn a_command_handed_to_a_shell_on_the_line_is_read_as_the_command() {
    reads_as(&[
        ("bash -c 'cargo test'", Work::Building, &["cargo test"]),
        (
            "sh -c \"git commit -m x\"",
            Work::VersionControl,
            &["git commit"],
        ),
        (
            "bash -lc 'cd crates && cargo build'",
            Work::Building,
            &["cargo build"],
        ),
    ]);
}

#[test]
fn a_script_fed_to_an_interpreter_by_a_here_document_is_a_script() {
    reads_as(&[
        ("python3 <<'EOF'\nimport os\nEOF", Work::Other, &["script"]),
        ("python3 - <<EOF\nimport os\nEOF", Work::Other, &["script"]),
        ("node <<< 'console.log(1)'", Work::Other, &["script"]),
    ]);
}

#[test]
fn a_program_run_through_a_launcher_is_named_as_itself() {
    reads_as(&[
        ("python -m pytest -x", Work::Building, &["pytest"]),
        (
            "python3 -m pip install requests",
            Work::Building,
            &["pip install"],
        ),
        ("npx jest", Work::Building, &["jest"]),
        ("npx --yes tsc --noEmit", Work::Building, &["tsc"]),
        ("pnpm exec vitest run", Work::Building, &["vitest"]),
        (
            "bundle exec rspec spec/a_spec.rb",
            Work::Building,
            &["rspec"],
        ),
        ("deno test", Work::Building, &["deno test"]),
        ("pip install requests", Work::Building, &["pip install"]),
    ]);
}

#[test]
fn an_option_value_before_the_subcommand_is_not_the_subcommand() {
    reads_as(&[
        ("npm --prefix web run build", Work::Building, &["npm run"]),
        (
            "cargo --manifest-path x/Cargo.toml test",
            Work::Building,
            &["cargo test"],
        ),
    ]);
}

#[test]
fn a_redirection_before_the_program_is_not_the_program() {
    reads_as(&[
        ("> out.txt cargo test", Work::Building, &["cargo test"]),
        ("2>/dev/null ls", Work::Exploring, &["ls"]),
    ]);
}

#[test]
fn tee_to_a_device_saves_nothing() {
    reads_as(&[
        ("cat f | tee /dev/stderr", Work::Exploring, &["cat", "tee"]),
        ("cat f | tee", Work::Exploring, &["cat", "tee"]),
        ("cat f | tee -a log.txt", Work::Editing, &["cat", "tee"]),
    ]);
}

fn server(tool: &str) -> Work {
    of_call(tool, None, None, "").work
}

#[test]
fn an_mcp_server_is_named_in_full_and_never_with_nothing() {
    assert_eq!(
        server("mcp__claude_ai_Google_Drive__search"),
        Work::Server("Google Drive".to_owned())
    );
    assert_eq!(
        server("mcp__claude_ai_Notion__notion-fetch"),
        Work::Server("Notion".to_owned())
    );
    for tool in ["mcp____x", "mcp___x__y", "mcp__claude_ai___search"] {
        assert!(!server(tool).label().trim().is_empty(), "{tool}");
    }
}

#[test]
fn an_mcp_server_named_as_a_kind_of_work_is_not_drawn_as_that_kind() {
    let built_in = [
        Work::Exploring,
        Work::Editing,
        Work::Building,
        Work::VersionControl,
        Work::Web,
        Work::Delegating,
        Work::Housekeeping,
        Work::Other,
    ];
    for name in ["Web", "Other", "Delegating"] {
        let work = server(&format!("mcp__{name}__fetch"));
        assert!(
            built_in.iter().all(|kind| kind.label() != work.label()),
            "{name}: {work:?}"
        );
    }
}
