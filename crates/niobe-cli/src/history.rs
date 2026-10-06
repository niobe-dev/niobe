// SPDX-License-Identifier: Apache-2.0
// Copyright (c) Viacheslav Shynkarenko

//! The repository's earlier sessions, read for the shell's history: the
//! prompts Up recalls and the sessions the history dialog can open.
//!
//! Read on a thread of its own, because listing the `claude` CLI's sessions
//! reads every transcript it keeps for the repository — about a second on a
//! repository with a few hundred — and the shell has a frame to draw every
//! tick.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::SystemTime;

use niobe_bridge_claude::transcript;
use niobe_tui::clock::Clock;
use niobe_tui::history::{History, Past, PastPrompt, PastSession, Target};

use crate::repo;

/// The history of the repository at `root`, and of the `claude` CLI's
/// sessions in `transcripts`, where it keeps any.
#[derive(Debug)]
pub struct StoreHistory {
    root: PathBuf,
    transcripts: Option<PathBuf>,
    clock: Clock,
    reading: Option<Receiver<Past>>,
}

impl StoreHistory {
    /// The history of `root`, with the CLI's sessions read from
    /// `transcripts`, each dated by `clock`.
    pub fn new(root: &Path, transcripts: Option<PathBuf>, clock: Clock) -> Self {
        Self {
            root: root.to_path_buf(),
            transcripts,
            clock,
            reading: None,
        }
    }
}

impl History for StoreHistory {
    fn load(&mut self) {
        if self.reading.is_some() {
            return;
        }
        let (sent, received) = mpsc::channel();
        let (root, transcripts, clock) = (
            self.root.clone(),
            self.transcripts.clone(),
            self.clock.clone(),
        );
        let started = std::thread::Builder::new()
            .name("history".to_owned())
            .spawn(move || {
                // Nobody left to hand it to is a shell that has closed.
                let _ = sent.send(read(&root, transcripts.as_deref(), &clock));
            });
        match started {
            Ok(_) => self.reading = Some(received),
            Err(error) => {
                let (sent, received) = mpsc::channel();
                let _ = sent.send(Past {
                    unread: Some(format!("the earlier sessions could not be read: {error}")),
                    ..Past::default()
                });
                self.reading = Some(received);
            }
        }
    }

    fn drain(&mut self) -> Option<Past> {
        let received = match self.reading.as_ref()?.try_recv() {
            Ok(past) => past,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Past {
                unread: Some("the earlier sessions could not be read".to_owned()),
                ..Past::default()
            },
        };
        self.reading = None;
        Some(received)
    }
}

/// Everything there is to go back to in `root`, and in the CLI's sessions in
/// `transcripts`.
///
/// What cannot be read is said in [`Past::unread`] and the rest is still
/// listed, as `niobe sessions` lists Niobe's own when the CLI's will not read.
fn read(root: &Path, transcripts: Option<&Path>, clock: &Clock) -> Past {
    let mut past = Past::default();
    let mut unread = Vec::new();
    let dated = |at: SystemTime| Some(clock.at(at));

    let mut carried = HashSet::new();
    match recorded(root) {
        Ok(Some(store)) => {
            past.prompts = store
                .prompts
                .into_iter()
                .map(|prompt| PastPrompt {
                    text: prompt.text,
                    at: dated(prompt.at),
                    session: Target::Recorded(prompt.session.to_string()),
                })
                .collect();
            past.sessions = store
                .sessions
                .into_iter()
                // A session nothing was recorded in has nothing to carry on.
                .filter(|session| session.events > 0)
                .map(|session| PastSession {
                    target: Target::Recorded(session.id.to_string()),
                    last: dated(session.last_at.unwrap_or(session.started_at)),
                    first_prompt: session.first_prompt,
                    title: session.title,
                    open_elsewhere: store.held.contains(&session.id),
                })
                .collect();
            carried = store.carried;
        }
        Ok(None) => {}
        Err(error) => unread.push(error),
    }

    // A conversation Niobe drove is in the CLI's store too; it is listed once,
    // as the session that recorded it, which is the one that carries it on
    // with what Niobe saw of it. One a program drove otherwise is left out:
    // its prompts are a script's, and opening it would put them where Up
    // recalls the operator's own. `niobe sessions` still lists it by id.
    match transcripts.map(transcript::list).transpose() {
        Ok(listed) => past.sessions.extend(
            listed
                .unwrap_or_default()
                .into_iter()
                .filter(|transcript| !carried.contains(&transcript.id) && !transcript.scripted)
                .map(|transcript| PastSession {
                    target: Target::Claude(transcript.id),
                    last: transcript.last_at.and_then(dated),
                    first_prompt: transcript.first_prompt,
                    // The CLI writes its title further into the file than a
                    // list reads; opening the session reads it in.
                    title: None,
                    open_elsewhere: false,
                }),
        ),
        Err(error) => unread.push(error.to_string()),
    }

    // Newest first, and a session with no date after every dated one.
    past.sessions
        .sort_by(|a, b| b.last.map(|at| at.at()).cmp(&a.last.map(|at| at.at())));
    past.unread = (!unread.is_empty()).then(|| unread.join("; "));
    past
}

/// What the session store of `root` holds of its sessions.
struct Recorded {
    prompts: Vec<niobe_store::StoredPrompt>,
    sessions: Vec<niobe_store::SessionSummary>,
    /// The conversations its sessions carried on, by the backend's ids.
    carried: HashSet<String>,
    /// The sessions another niobe is recording now.
    held: HashSet<niobe_store::SessionId>,
}

/// The store's prompts, sessions and conversations; `None` where nothing has
/// been recorded in `root`.
fn recorded(root: &Path) -> Result<Option<Recorded>, String> {
    let Some(store) = repo::read_existing_store(root)? else {
        return Ok(None);
    };
    let failed = |e: niobe_store::StoreError| e.to_string();
    let sessions = store.sessions().map_err(failed)?;
    // A hold that cannot be looked at is listed as free: opening it says
    // why it cannot be, where the list could only leave it out.
    let held = sessions
        .iter()
        .filter(|session| session.events > 0)
        .map(|session| session.id)
        .filter(|&id| store.is_held(id).unwrap_or(false))
        .collect();
    Ok(Some(Recorded {
        prompts: store.prompts().map_err(failed)?,
        sessions,
        held,
        carried: store
            .conversations()
            .map_err(failed)?
            .into_iter()
            .map(|(_, conversation)| conversation)
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use niobe_core::event::{Backend, Event, SessionMeta};

    fn user(text: &str) -> Event {
        Event::UserMessage {
            text: text.to_owned(),
        }
    }

    fn meta(conversation: &str) -> Event {
        Event::SessionMeta(SessionMeta {
            backend: Backend::Claude,
            profile: "max".to_owned(),
            model: "opus-5".to_owned(),
            backend_session: Some(conversation.to_owned()),
        })
    }

    fn clock() -> Clock {
        Clock::fixed(0).expect("UTC is an offset")
    }

    /// A repository whose store holds one session that carried on `conv-1`,
    /// and a `claude` directory with that conversation and one of its own.
    fn repository() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().expect("a temporary directory");
        let store = repo::open_or_create_store(root.path()).expect("the store is made");
        let session = store.create_session().expect("a session is created");
        for event in [meta("conv-1"), user("add etag support"), user("and a test")] {
            store.append(session, &event).expect("append");
        }
        let empty = store.create_session().expect("a session is created");
        let _ = empty;

        let claude = tempfile::tempdir().expect("a temporary directory");
        let dir = transcript::directory(claude.path(), root.path());
        std::fs::create_dir_all(&dir).expect("the project directory is made");
        for id in ["conv-1", "own-2"] {
            std::fs::write(
                dir.join(format!("{id}.jsonl")),
                r#"{"type":"user","message":{"role":"user","content":"rename the crate"}}"#,
            )
            .expect("the transcript is written");
        }
        (root, claude, dir)
    }

    #[test]
    fn the_store_and_the_clis_own_sessions_are_read_together_newest_first() {
        let (root, _claude, dir) = repository();

        let past = read(root.path(), Some(&dir), &clock());

        let prompts: Vec<&str> = past.prompts.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(prompts, ["and a test", "add etag support"]);
        assert!(past.prompts.iter().all(|p| p.at.is_some()));
        let targets: HashSet<Target> = past.sessions.iter().map(|s| s.target.clone()).collect();
        assert_eq!(
            targets,
            HashSet::from([
                Target::Recorded("1".to_owned()),
                Target::Claude("own-2".to_owned()),
            ]),
            "the conversation Niobe recorded is listed once, as Niobe's, and the \
             session with nothing in it not at all"
        );
        assert_eq!(past.unread, None);
    }

    #[test]
    fn a_claude_session_a_program_ran_is_not_offered_unless_niobe_carried_it_on() {
        let (root, _claude, dir) = repository();
        let entered = |entrypoint: &str, text: &str| {
            format!(
                r#"{{"type":"user","entrypoint":"{entrypoint}","message":{{"role":"user","content":{text:?}}}}}"#
            )
        };
        for (id, entrypoint, text) in [
            ("typed-3", "cli", "rename the crate"),
            (
                "scripted-4",
                "sdk-cli",
                "Commit it now: stage exactly the seven files",
            ),
            ("conv-1", "sdk-cli", "add etag support"),
        ] {
            std::fs::write(dir.join(format!("{id}.jsonl")), entered(entrypoint, text))
                .expect("the transcript is written");
        }

        let past = read(root.path(), Some(&dir), &clock());

        let targets: HashSet<Target> = past.sessions.iter().map(|s| s.target.clone()).collect();
        assert_eq!(
            targets,
            HashSet::from([
                Target::Recorded("1".to_owned()),
                Target::Claude("own-2".to_owned()),
                Target::Claude("typed-3".to_owned()),
            ]),
            "the scripted session is left out, and the one Niobe drove is listed once, \
             from the store"
        );
        assert!(
            past.sessions
                .iter()
                .filter_map(|s| s.first_prompt.as_deref())
                .chain(past.prompts.iter().map(|p| p.text.as_str()))
                .all(|said| !said.starts_with("Commit it now")),
            "nothing the program wrote is offered to recall"
        );
    }

    #[test]
    fn a_session_another_niobe_is_recording_is_listed_as_open_elsewhere() {
        let (root, _claude, dir) = repository();
        let store = repo::open_existing_store(root.path())
            .expect("the store opens")
            .expect("there is a store");
        let session = "1".parse().expect("a number is a session id");
        let recorder = niobe_store::Recorder::resume(store, session).expect("nobody holds it");

        let open = |past: Past| -> HashSet<(Target, bool)> {
            past.sessions
                .into_iter()
                .map(|s| (s.target, s.open_elsewhere))
                .collect()
        };
        assert_eq!(
            open(read(root.path(), Some(&dir), &clock())),
            HashSet::from([
                (Target::Recorded("1".to_owned()), true),
                (Target::Claude("own-2".to_owned()), false),
            ])
        );

        drop(recorder);
        assert_eq!(
            open(read(root.path(), Some(&dir), &clock())),
            HashSet::from([
                (Target::Recorded("1".to_owned()), false),
                (Target::Claude("own-2".to_owned()), false),
            ])
        );
    }

    #[test]
    fn a_session_the_backend_titled_is_read_back_with_its_title() {
        let (root, _claude, dir) = repository();
        let store = repo::open_existing_store(root.path())
            .expect("the store opens")
            .expect("there is a store");
        let session = "1".parse().expect("a number is a session id");
        store
            .append(
                session,
                &Event::Titled {
                    title: "Etag support".to_owned(),
                },
            )
            .expect("append");

        let past = read(root.path(), Some(&dir), &clock());

        let titled = past
            .sessions
            .iter()
            .find(|s| s.target == Target::Recorded("1".to_owned()))
            .expect("the recorded session is listed");
        assert_eq!(titled.title.as_deref(), Some("Etag support"));
    }

    #[test]
    fn a_repository_with_nothing_recorded_reads_as_nothing_rather_than_a_failure() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let past = read(root.path(), None, &clock());
        assert_eq!(past, Past::default());
    }

    #[test]
    fn transcripts_that_cannot_be_read_are_said_and_the_store_is_still_listed() {
        let (root, claude, _dir) = repository();
        let blocked = claude.path().join("a file, not a directory");
        std::fs::write(&blocked, "").expect("the file is written");

        let past = read(root.path(), Some(&blocked), &clock());

        assert_eq!(past.sessions.len(), 1);
        assert!(past.unread.is_some());
    }

    #[test]
    fn a_load_arrives_through_drain_once_and_a_second_load_waits_on_the_first() {
        let (root, _claude, dir) = repository();
        let mut history = StoreHistory::new(root.path(), Some(dir), clock());

        history.load();
        history.load();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let past = loop {
            if let Some(past) = history.drain() {
                break past;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the load never landed"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(past.prompts.len(), 2);
        assert_eq!(history.drain(), None, "one load, one answer");
    }
}
