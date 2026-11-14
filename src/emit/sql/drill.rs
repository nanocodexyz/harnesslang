use std::collections::BTreeMap;

use crate::emit::sql::{self, Catalogue, Dialect, Literal};
use crate::ir::Program;
use crate::run::journal::{self, Record};
use crate::run::json::{self, Json};
use crate::run::mutation::Mutation as SimMutation;
use crate::sim::{self, trace::Trace};
use crate::target::Target;

pub const REPLAY_LEASE_MS: u64 = 0;
pub const DUMP_SEPARATOR: char = '\t';
pub const JOURNAL_DUMP_FIELDS: usize = 7;
pub const SESSION_DUMP_FIELDS: usize = 6;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Acquire {
        owner: String,
        epoch: u64,
        head: u64,
    },
    Commit {
        epoch: u64,
        head: u64,
        records: Vec<Record>,
        fenced: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub seq: u64,
    pub epoch: u64,
    pub record: Record,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub session: String,
    pub turns: u64,
    pub steps: Vec<Step>,
    pub rows: Vec<Row>,
    pub epoch: u64,
    pub finished: u64,
    pub checkpoint: u64,
}

struct Owners {
    target: Target,
    epoch: u64,
    heads: BTreeMap<u64, u64>,
}

impl Owners {
    fn acquire(&mut self, simulated: u64, rows: u64) -> u64 {
        self.epoch += 1;
        if self.target == Target::ServerPostgres {
            assert_eq!(
                simulated, self.epoch,
                "every simulated takeover is one database acquire"
            );
        }
        self.heads.insert(self.epoch, rows);
        self.epoch
    }

    fn writer(&self, simulated: u64) -> u64 {
        if self.target == Target::ServerPostgres {
            simulated
        } else {
            self.epoch
        }
    }
}

fn field<'a>(event: &'a Json, key: &str) -> Result<&'a Json, String> {
    event
        .get(key)
        .ok_or_else(|| format!("trace event lacks '{key}': {}", event.encode()))
}

fn number(event: &Json, key: &str) -> Result<u64, String> {
    field(event, key)?
        .as_u64()
        .ok_or_else(|| format!("trace event's '{key}' is not a number"))
}

fn text<'a>(event: &'a Json, key: &str) -> Result<&'a str, String> {
    field(event, key)?
        .as_str()
        .ok_or_else(|| format!("trace event's '{key}' is not text"))
}

pub fn plan(program: &Program, target: Target, seed: u64) -> Result<Plan, String> {
    sql::Dialect::of(target)?;
    let (run, trace) = sim::run_seed_traced(
        program,
        target,
        SimMutation::None,
        seed,
        Some(Trace::default()),
    );
    run.result
        .as_ref()
        .map_err(|violation| format!("seed {seed} fails the simulator: {violation:?}"))?;
    let trace = trace.expect("a traced run returns its trace");
    let session = format!("seed-{seed}");
    let mut owners = Owners {
        target,
        epoch: 0,
        heads: BTreeMap::new(),
    };
    let mut steps = Vec::new();
    let mut rows: Vec<Row> = Vec::new();
    let mut turns = 0;
    for line in &trace.lines {
        let event = json::decode(line)?;
        match text(&event, "event")? {
            "turn" => turns += 1,
            "run" | "owner" => {
                let epoch = owners.acquire(number(&event, "epoch")?, rows.len() as u64);
                steps.push(Step::Acquire {
                    owner: format!("{session}/owner-{epoch}"),
                    epoch,
                    head: rows.len() as u64,
                });
            }
            "flush" => {
                let epoch = owners.writer(number(&event, "epoch")?);
                let decoded = journal::decode_lines(text(&event, "bytes")?.as_bytes(), false)?;
                let head = *owners.heads.get(&epoch).ok_or_else(|| {
                    format!("seed {seed}: a flush from epoch {epoch}, which never acquired")
                })?;
                let fenced = match text(&event, "answer")? {
                    "ok" => false,
                    "fenced" => true,
                    other => return Err(format!("seed {seed}: a row flush answered {other}")),
                };
                if !fenced {
                    assert_eq!(
                        head,
                        rows.len() as u64,
                        "a committing owner has read every row"
                    );
                    for record in &decoded.records {
                        rows.push(Row {
                            seq: rows.len() as u64 + sql::FIRST_ROW_SEQ,
                            epoch,
                            record: record.clone(),
                        });
                    }
                    owners.heads.insert(epoch, rows.len() as u64);
                }
                steps.push(Step::Commit {
                    epoch,
                    head,
                    records: decoded.records,
                    fenced,
                });
            }
            _ => {}
        }
    }
    let interpreted = journal::decode_lines(&run.journal, false)?.records;
    let planned: Vec<Record> = rows.iter().map(|row| row.record.clone()).collect();
    assert_eq!(
        planned, interpreted,
        "the committed flushes are the interpreter's journal"
    );
    let finished = rows
        .iter()
        .filter_map(|row| match &row.record {
            Record::Status { turn, .. } => Some(*turn),
            _ => None,
        })
        .max()
        .unwrap_or(sql::EMPTY_HEAD);
    let checkpoint = rows
        .iter()
        .filter(|row| matches!(row.record, Record::Checkpoint { .. }))
        .map(|row| row.seq)
        .max()
        .unwrap_or(sql::EMPTY_HEAD);
    Ok(Plan {
        session,
        turns,
        steps,
        rows,
        epoch: owners.epoch,
        finished,
        checkpoint,
    })
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub seeds: u64,
    pub turns: u64,
    pub acquires: u64,
    pub transactions: u64,
    pub fenced: u64,
    pub rows: u64,
    pub record_bytes: u64,
    pub batch_bytes: u64,
}

fn commit_text(
    catalogue: &Catalogue,
    session: &str,
    epoch: u64,
    head: u64,
    records: &[Record],
) -> Result<String, String> {
    let named = [
        ("session", Literal::Text(session.to_owned())),
        ("epoch", Literal::Integer(epoch)),
        ("head", Literal::Integer(head)),
        ("lease_ms", Literal::Integer(REPLAY_LEASE_MS)),
        ("batch", Literal::Text(sql::batch_json(records, head))),
    ];
    let body = sql::render_all(&catalogue.commit, &named)?;
    Ok(match catalogue.dialect {
        Dialect::DurableObject => format!("BEGIN;\n{body}COMMIT;\n"),
        Dialect::Sqlite | Dialect::Postgres => body,
    })
}

pub fn preamble(catalogue: &Catalogue) -> Result<String, String> {
    let mut out = match catalogue.dialect {
        Dialect::Postgres => "\\set VERBOSITY verbose\n".to_owned(),
        Dialect::Sqlite | Dialect::DurableObject => String::new(),
    };
    out.push_str(&sql::render_all(&catalogue.connect, &[])?);
    out.push_str(&sql::render_all(&catalogue.schema, &[])?);
    Ok(out)
}

pub fn replay(catalogue: &Catalogue, plan: &Plan, totals: &mut Totals) -> Result<String, String> {
    let mut out = String::new();
    totals.seeds += 1;
    totals.turns += plan.turns;
    for step in &plan.steps {
        match step {
            Step::Acquire { owner, .. } => {
                let named = [
                    ("session", Literal::Text(plan.session.clone())),
                    ("owner", Literal::Text(owner.clone())),
                    ("lease_ms", Literal::Integer(REPLAY_LEASE_MS)),
                ];
                out.push_str(&sql::render_all(
                    std::slice::from_ref(&catalogue.acquire),
                    &named,
                )?);
                out.push_str(&sql::render_all(
                    std::slice::from_ref(&catalogue.load),
                    &[("session", Literal::Text(plan.session.clone()))],
                )?);
                totals.acquires += 1;
            }
            Step::Commit {
                epoch,
                head,
                records,
                fenced,
            } => {
                let text = commit_text(catalogue, &plan.session, *epoch, *head, records)?;
                if *fenced {
                    totals.fenced += 1;
                } else {
                    totals.transactions += 1;
                    totals.rows += records.len() as u64;
                    totals.batch_bytes += sql::batch_json(records, *head).len() as u64;
                    for record in records {
                        totals.record_bytes += journal::to_json(record).encode().len() as u64;
                    }
                }
                out.push_str(&text);
            }
        }
    }
    Ok(out)
}

pub fn expected_dump(plan: &Plan) -> Vec<String> {
    let mut lines = Vec::with_capacity(plan.rows.len() + 1);
    for row in &plan.rows {
        lines.push(
            [
                "journal".to_owned(),
                plan.session.clone(),
                row.seq.to_string(),
                row.epoch.to_string(),
                sql::record_turn(&row.record).to_string(),
                sql::record_kind(&row.record).to_owned(),
                journal::to_json(&row.record).encode(),
            ]
            .join(&DUMP_SEPARATOR.to_string()),
        );
    }
    lines.push(
        [
            "session".to_owned(),
            plan.session.clone(),
            plan.epoch.to_string(),
            plan.rows.len().to_string(),
            plan.finished.to_string(),
            plan.checkpoint.to_string(),
        ]
        .join(&DUMP_SEPARATOR.to_string()),
    );
    lines
}

pub fn check_dump_line(line: &str) -> Result<(), String> {
    let fields: Vec<&str> = line.split(DUMP_SEPARATOR).collect();
    match fields.first() {
        Some(&"journal") if fields.len() == JOURNAL_DUMP_FIELDS => {
            let record = journal::decode_line(fields[JOURNAL_DUMP_FIELDS - 1], false)?;
            if sql::record_kind(&record) != fields[5] {
                return Err(format!(
                    "row kind {} holds a {} record",
                    fields[5],
                    sql::record_kind(&record)
                ));
            }
            if sql::record_turn(&record).to_string() != fields[4] {
                return Err(format!(
                    "row turn {} holds a record of turn {}",
                    fields[4],
                    sql::record_turn(&record)
                ));
            }
            Ok(())
        }
        Some(&"session") if fields.len() == SESSION_DUMP_FIELDS => Ok(()),
        _ => Err(format!(
            "dump line has an unexpected shape: {}",
            line.chars().take(160).collect::<String>()
        )),
    }
}
