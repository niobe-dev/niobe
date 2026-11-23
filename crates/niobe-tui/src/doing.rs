// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! What a sub-agent is doing, in the few words its row at the foot of the
//! transcript has room for: `running cargo test · tail`, `reading ui.rs`.
//!
//! The call's own row in the transcript says it whole; this is the glance a
//! row shared with the agent's tag, task, model and clock can give it, so a
//! file is named by its last component and a shell call by the programs it
//! ran rather than by its command line.

use crate::app::Call;
use niobe_core::work::{Deed, deed};

/// What the call `call` to the tool its transcript row heads as `head` is
/// doing, while it runs.
pub(crate) fn brief(head: &str, call: &Call) -> String {
    if call.asked {
        return "waiting on a question".to_owned();
    }
    if let Some(programs) = &call.programs {
        return format!("running {programs}");
    }
    let what = call.what.as_str();
    let said = match deed(head) {
        Some(Deed::Read) => ("reading", file_name(what)),
        Some(Deed::Edited) => ("editing", file_name(what)),
        Some(Deed::Searched) => ("searching", what),
        Some(Deed::Listed) => ("listing", file_name(what)),
        Some(Deed::Fetched) => ("fetching", what),
        Some(Deed::SearchedWeb) => return "searching the web".to_owned(),
        None => return format!("using {head}"),
    };
    match said {
        (verb, "") => verb.to_owned(),
        (verb, object) => format!("{verb} {object}"),
    }
}

/// A path's last component, which is what tells one file from another at a
/// glance: the directories are the same for most of what an agent reads.
fn file_name(path: &str) -> &str {
    let path = path.trim_end_matches('/');
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(what: &str) -> Call {
        Call::started(what.to_owned(), None)
    }

    #[test]
    fn a_shell_call_is_named_by_the_programs_it_runs_not_its_command_line() {
        let mut shell = call(r#"echo "== mcp"; grep -rli --include='*.ts' mcp src"#);
        shell.programs = Some("echo · grep".to_owned());
        assert_eq!(brief("Bash", &shell), "running echo · grep");
    }

    #[test]
    fn a_file_is_named_by_its_last_component() {
        assert_eq!(
            brief("Read", &call("crates/niobe-tui/src/ui.rs")),
            "reading ui.rs"
        );
        assert_eq!(brief("Edit", &call("src/app.rs")), "editing app.rs");
        assert_eq!(brief("Write", &call("notes.md")), "editing notes.md");
    }

    #[test]
    fn a_search_says_what_it_looks_for() {
        assert_eq!(
            brief("Grep", &call("fn main in crates")),
            "searching fn main in crates"
        );
        assert_eq!(brief("WebSearch", &call("ratatui")), "searching the web");
        assert_eq!(
            brief("WebFetch", &call("https://example.com/a")),
            "fetching https://example.com/a"
        );
    }

    #[test]
    fn any_other_tool_is_named_as_the_transcript_names_it() {
        assert_eq!(brief("Notion·search", &call("q")), "using Notion·search");
    }

    #[test]
    fn a_call_waiting_on_a_question_says_so_rather_than_that_it_runs() {
        let mut asked = call("src/app.rs");
        asked.asked = true;
        assert_eq!(brief("Edit", &asked), "waiting on a question");
    }

    #[test]
    fn a_call_with_nothing_said_of_it_is_named_by_its_tool_alone() {
        assert_eq!(brief("Read", &call("")), "reading");
    }
}
