use crate::harness::{self, Json, Record, Sql, SqlError, SqlJournal, SqlValue, Statement, Stop};
use crate::{Driver, fail, text};

pub struct State {
    journal: SqlJournal,
    rows: Vec<String>,
    epoch: u64,
    answer: Option<String>,
    batched: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            journal: SqlJournal {
                session: "differential".to_owned(),
                owner: "rust".to_owned(),
                lease_ms: 0,
                head: 0,
            },
            rows: Vec::new(),
            epoch: 0,
            answer: None,
            batched: false,
        }
    }
}

pub fn open(driver: &mut Driver, epoch: u64, bytes: &[u8]) -> (Vec<Record>, u64) {
    let text = String::from_utf8(bytes.to_vec()).unwrap_or_else(|_| fail("rows are not utf-8"));
    driver.store.rows = text.lines().map(str::to_owned).collect();
    driver.store.epoch = epoch;
    let mut journal = driver.store.journal.clone();
    let opened = harness::sql_open(driver, &mut journal).unwrap_or_else(|error| fail(&error.to_string()));
    driver.store.journal = journal;
    (opened.records, opened.epoch)
}

pub fn flush(driver: &mut Driver, epoch: u64, records: &[Record]) -> Result<(), Stop> {
    let mut journal = driver.store.journal.clone();
    let writes = driver.stats.writes;
    driver.store.batched = false;
    let result = harness::sql_flush(driver, &mut journal, epoch, records);
    if driver.stats.writes != writes + 1 {
        fail("a commit did not carry exactly one batch");
    }
    driver.store.journal = journal;
    result
}

fn batch(driver: &mut Driver, values: &[SqlValue], statement: &Statement) {
    let index = statement.params.iter().position(|param| param.name == "batch").expect("a batch parameter");
    let SqlValue::Json(batch) = &values[index] else {
        fail("the batch is not bound as json");
    };
    let rows = Json::decode(batch).unwrap_or_else(|error| fail(&format!("the batch is not json: {error}")));
    let Json::Array(rows) = rows else {
        fail("the batch is not an array");
    };
    let mut lines = String::new();
    let head = driver.store.journal.head;
    for (offset, row) in rows.iter().enumerate() {
        let seq = row.get("seq").and_then(Json::as_u64).unwrap_or_else(|| fail("a batch row lacks seq"));
        if seq != head + 1 + offset as u64 {
            fail(&format!("a batch row has seq {seq} after head {head}"));
        }
        lines.push_str(row.get("record").and_then(Json::as_str).unwrap_or_else(|| fail("a batch row lacks record")));
        lines.push('\n');
    }
    let event = driver.events.next("flush");
    if text(&event, "bytes") != lines {
        fail(&format!(
            "event {}: the generated store committed {lines:?}, the interpreter {:?}",
            driver.events.index,
            text(&event, "bytes")
        ));
    }
    driver.stats.writes += 1;
    driver.store.answer = Some(text(&event, "answer").to_owned());
}

impl Sql for Driver {
    fn query(&mut self, statement: &Statement, values: &[SqlValue]) -> Result<Vec<Vec<Option<String>>>, SqlError> {
        assert_eq!(values.len(), statement.params.len(), "one value per parameter");
        self.stats.statements += 1;
        match statement.name {
            "acquire" => Ok(vec![vec![Some(self.store.epoch.to_string()), Some(self.store.rows.len().to_string())]]),
            "load" => Ok(self
                .store
                .rows
                .iter()
                .enumerate()
                .map(|(index, row)| vec![Some((index + 1).to_string()), Some(row.clone())])
                .collect()),
            _ if harness::COMMIT.iter().any(|commit| commit.name == statement.name) => {
                let last = harness::COMMIT.last().is_some_and(|commit| commit.name == statement.name);
                let carries = statement.params.iter().any(|param| param.name == "batch");
                let answer = if carries && !self.store.batched {
                    self.store.batched = true;
                    batch(self, values, statement);
                    self.store.answer.take()
                } else {
                    None
                };
                if last {
                    self.stats.syncs += 1;
                }
                match answer.as_deref() {
                    None | Some("ok") => Ok(Vec::new()),
                    Some("fenced") => Err(SqlError::Fenced),
                    Some(other) => fail(&format!("a rows flush answered {other}")),
                }
            }
            other => fail(&format!("the generated store ran an unknown statement {other}")),
        }
    }

    fn rollback(&mut self) {}
}
