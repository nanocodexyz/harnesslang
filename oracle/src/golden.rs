use std::collections::BTreeSet;
use std::path::Path;

use nanocode_code_mode_engine::JournalEntry;
use serde_json::{Value, json};

use crate::cells::{self, Header};
use crate::djs::{self, Inputs, Next, Run, Script, Terminal, View};

const SESSION: &str = "golden";
const ORIGIN: &str = "1/1";
const YIELD_MS: u64 = 10_000;
pub const NESTED_TOOLS: [(&str, &str); 3] = [
    ("read", "none"),
    ("edit", "idempotent"),
    ("bash", "external"),
];

#[derive(Clone, Copy)]
enum Order {
    Oldest,
    Newest,
}

struct Case {
    name: &'static str,
    script: &'static str,
    order: Order,
    unknown: &'static [u64],
    failed: &'static [u64],
    timers_first: bool,
}

const CASES: [Case; 10] = [
    Case {
        name: "sequential_classes",
        script: "const a = await tools.read({ path: \"a\" });\nconst b = await tools.edit({ path: \"a\", body: a });\nconst c = await tools.bash({ command: \"ls\" });\ntext([a, b, c].join(\" | \"));",
        order: Order::Oldest,
        unknown: &[],
        failed: &[],
        timers_first: false,
    },
    Case {
        name: "parallel_out_of_order",
        script: "const results = await Promise.all([tools.read({ path: \"1\" }), tools.edit({ path: \"2\", body: \"x\" }), tools.bash({ command: \"3\" })]);\ntext(results.join(\" | \"));",
        order: Order::Newest,
        unknown: &[],
        failed: &[],
        timers_first: false,
    },
    Case {
        name: "parallel_in_order",
        script: "const results = await Promise.all([1, 2, 3, 4].map((n) => tools.read({ path: String(n) })));\ntext(results.join(\" | \"));",
        order: Order::Oldest,
        unknown: &[],
        failed: &[],
        timers_first: false,
    },
    Case {
        name: "timers",
        script: "await new Promise((resolve) => setTimeout(resolve, 5));\nconst a = await tools.read({ path: \"t\" });\nsetTimeout(() => text(\"timer 2\"), 20);\nawait new Promise((resolve) => setTimeout(resolve, 50));\ntext(a);",
        order: Order::Oldest,
        unknown: &[],
        failed: &[],
        timers_first: false,
    },
    Case {
        name: "timer_before_result",
        script: "const call = tools.bash({ command: \"slow\" });\nawait new Promise((resolve) => setTimeout(resolve, 10));\ntext(\"after timer\");\ntext(await call);",
        order: Order::Oldest,
        unknown: &[],
        failed: &[],
        timers_first: true,
    },
    Case {
        name: "clock_random",
        script: "const t = Date.now();\nconst m = performance.now();\nconst r = Math.random();\nconst a = await tools.read({ path: String(t) });\ntext([typeof t, typeof m, r >= 0 && r < 1, a].join(\" | \"));",
        order: Order::Oldest,
        unknown: &[],
        failed: &[],
        timers_first: false,
    },
    Case {
        name: "unknown_external",
        script: "try {\n  await tools.bash({ command: \"deploy\" });\n  text(\"known\");\n} catch (error) {\n  text(\"unknown: \" + error.outcome + \" \" + error.code);\n}\ntext(await tools.read({ path: \"after\" }));",
        order: Order::Oldest,
        unknown: &[1],
        failed: &[],
        timers_first: false,
    },
    Case {
        name: "unknown_in_parallel",
        script: "const settled = await Promise.allSettled([tools.bash({ command: \"a\" }), tools.bash({ command: \"b\" }), tools.edit({ path: \"c\", body: \"d\" })]);\ntext(settled.map((s) => s.status + \":\" + (s.value ?? s.reason.outcome)).join(\" | \"));",
        order: Order::Oldest,
        unknown: &[2],
        failed: &[],
        timers_first: false,
    },
    Case {
        name: "failed_call_throws",
        script: "const a = await tools.read({ path: \"missing\" });\ntext(a);",
        order: Order::Oldest,
        unknown: &[],
        failed: &[1],
        timers_first: false,
    },
    Case {
        name: "script_throws",
        script: "const a = await tools.edit({ path: \"p\", body: \"b\" });\ntext(a);\nthrow new Error(\"boom after \" + a);",
        order: Order::Oldest,
        unknown: &[],
        failed: &[],
        timers_first: false,
    },
];

struct Respond<'a> {
    case: &'a Case,
}

pub fn pending_timer(entries: &[JournalEntry]) -> bool {
    let mut armed = BTreeSet::new();
    for entry in entries {
        match entry {
            JournalEntry::Timeout { id, .. } => {
                armed.insert(*id);
            }
            JournalEntry::Timer { id } => {
                armed.remove(id);
            }
            _ => {}
        }
    }
    !armed.is_empty()
}

fn call(entries: &[JournalEntry], wanted: u64) -> (&str, &Value) {
    entries
        .iter()
        .find_map(|entry| match entry {
            JournalEntry::Call {
                id, name, input, ..
            } if *id == wanted => Some((name.as_str(), input)),
            _ => None,
        })
        .expect("an open call has its Call entry in the published journal")
}

impl Inputs for Respond<'_> {
    fn next(&mut self, view: &View<'_>) -> Next {
        let timer = pending_timer(view.entries);
        if self.case.timers_first && timer {
            return Next::Fire;
        }
        let pick = match self.case.order {
            Order::Oldest => view.open.first(),
            Order::Newest => view.open.last(),
        };
        let Some(&id) = pick else {
            return if timer { Next::Fire } else { Next::Stop };
        };
        let (name, input) = call(view.entries, id);
        let (encoded, success) = if self.case.unknown.contains(&id) {
            (cells::unknown_outcome(), false)
        } else if self.case.failed.contains(&id) {
            (
                json!({ "error": format!("{name} failed") }).to_string(),
                false,
            )
        } else {
            (json!(format!("{name}:{input}")).to_string(), true)
        };
        Next::Deliver {
            id,
            encoded,
            success,
        }
    }
}

pub struct Follow<'a> {
    pub expected: &'a [JournalEntry],
}

impl Inputs for Follow<'_> {
    fn next(&mut self, view: &View<'_>) -> Next {
        match self.expected.get(view.entries.len()) {
            Some(JournalEntry::Result {
                id,
                encoded,
                success,
            }) if view.open.contains(id) => Next::Deliver {
                id: *id,
                encoded: encoded.clone(),
                success: *success,
            },
            Some(JournalEntry::Timer { .. }) => Next::Fire,
            _ => Next::Stop,
        }
    }
}

fn normalized(entries: &[JournalEntry]) -> Vec<JournalEntry> {
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| match entry {
            JournalEntry::Monotonic { .. } => JournalEntry::Monotonic {
                value: index as f64,
            },
            other => other.clone(),
        })
        .collect()
}

pub fn diverged(entries: &[JournalEntry]) -> Option<Vec<JournalEntry>> {
    let mut changed = entries.to_vec();
    let position = changed
        .iter()
        .position(|entry| matches!(entry, JournalEntry::Call { .. }))?;
    if let JournalEntry::Call { input, .. } = &mut changed[position] {
        *input = json!({ "diverged": position });
    }
    Some(changed)
}

pub fn terminal_json(terminal: &Terminal) -> Value {
    match terminal {
        Terminal::Done => json!({ "done": true }),
        Terminal::Threw(message) => json!({ "threw": message }),
        Terminal::Diverged(reason) => json!({ "diverged": reason }),
        Terminal::Waiting(position) => json!({ "waiting_at": position }),
    }
}

fn script(case: &Case, index: usize) -> Script {
    Script {
        id: index as u64 + 1,
        script: case.script.to_owned(),
        tools: NESTED_TOOLS
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .collect(),
        seed: index as u64 + 1,
        wall_ms: 0,
    }
}

pub fn resumes(cell: &Script, entries: &[JournalEntry], whole: &Run) -> Result<(), String> {
    for cut in 0..=entries.len() {
        let mut follow = Follow { expected: entries };
        let resumed = djs::run(cell, Some(entries[..cut].to_vec()), &mut follow)?;
        if resumed.terminal != whole.terminal || resumed.text != whole.text {
            return Err(format!(
                "replaying the first {cut} entries then running live ends {:?} {:?}, the whole run {:?} {:?}",
                resumed.terminal, resumed.text, whole.terminal, whole.text
            ));
        }
        if let Some(difference) = cells::first_difference(&resumed.entries, entries) {
            return Err(format!("replaying the first {cut} entries: {difference}"));
        }
    }
    Ok(())
}

pub fn divergence(cell: &Script, entries: &[JournalEntry]) -> Result<Value, String> {
    let Some(changed) = diverged(entries) else {
        return Ok(Value::Null);
    };
    let mut follow = Follow { expected: &changed };
    let run = djs::run(cell, Some(changed.clone()), &mut follow)?;
    let Terminal::Diverged(_) = &run.terminal else {
        return Err(format!(
            "a changed Call input replays to {:?}",
            run.terminal
        ));
    };
    Ok(terminal_json(&run.terminal))
}

fn replay_matches(cell: &Script, entries: &[JournalEntry], live: &Run) -> Result<(), String> {
    let mut follow = Follow { expected: entries };
    let replay = djs::run(cell, Some(entries.to_vec()), &mut follow)?;
    if replay.terminal != live.terminal || replay.text != live.text || replay.entries != entries {
        return Err(format!(
            "replay ends {:?} {:?}, the live run {:?} {:?}",
            replay.terminal, replay.text, live.terminal, live.text
        ));
    }
    if !entries.is_empty() && !replay.replayed {
        return Err("djs finished the replay without reporting it".to_owned());
    }
    Ok(())
}

pub struct Lived {
    pub name: &'static str,
    pub cell: Script,
    pub entries: Vec<JournalEntry>,
    pub run: Run,
}

fn live(case: &Case, index: usize) -> Result<Lived, String> {
    let cell = script(case, index);
    let run = djs::run(&cell, None, &mut Respond { case })?;
    let entries = normalized(&run.entries);
    Ok(Lived {
        name: case.name,
        cell,
        entries,
        run,
    })
}

pub fn lived() -> Result<Vec<Lived>, String> {
    CASES
        .iter()
        .enumerate()
        .map(|(index, case)| live(case, index).map_err(|error| format!("{}: {error}", case.name)))
        .collect()
}

fn golden(case: &Case, index: usize) -> Result<Value, String> {
    let Lived {
        cell,
        entries,
        run: live,
        ..
    } = live(case, index)?;
    replay_matches(&cell, &entries, &live)?;
    resumes(&cell, &entries, &live)?;
    let divergence = divergence(&cell, &entries)?;
    let finished = matches!(live.terminal, Terminal::Done | Terminal::Threw(_));
    let meta = Header {
        session: SESSION.to_owned(),
        cell: cell.id,
        origin: ORIGIN.to_owned(),
        script: cell.script.clone(),
        tools: cell.tools.clone(),
        yield_ms: YIELD_MS,
    };
    let updates = cells::model_updates(&meta, &entries, finished);
    let folds = cells::check_prefixes(&updates, &entries, finished)?;
    cells::check_gap(&updates)?;
    Ok(json!({
        "name": case.name,
        "script": case.script,
        "tools": NESTED_TOOLS.iter().map(|(name, class)| json!({ "name": name, "effect": class })).collect::<Vec<_>>(),
        "entries": entries,
        "text": live.text,
        "terminal": terminal_json(&live.terminal),
        "divergence": divergence,
        "updates": updates,
        "folds": folds.iter().map(|state| json!({
            "entries": state.entries,
            "calls": state.calls,
            "finished": state.finished,
            "unknown": state.unknown,
        })).collect::<Vec<_>>(),
    }))
}

pub fn write(directory: &Path) -> Result<usize, String> {
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("{}: cannot create: {error}", directory.display()))?;
    for (index, case) in CASES.iter().enumerate() {
        let value =
            golden(case, index).map_err(|error| format!("golden {}: {error}", case.name))?;
        let mut text = serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?;
        text.push('\n');
        let path = directory.join(format!("{}.json", case.name));
        std::fs::write(&path, text)
            .map_err(|error| format!("{}: cannot write: {error}", path.display()))?;
    }
    Ok(CASES.len())
}
