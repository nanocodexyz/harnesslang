#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamType {
    Text,
    Integer,
    Json,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: &'static str,
    pub ty: ParamType,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Statement {
    pub name: &'static str,
    pub sql: &'static str,
    pub params: &'static [Param],
    pub returns: &'static [&'static str],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SqlValue {
    Text(String),
    Integer(u64),
    Json(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SqlError {
    Fenced,
    Other(String),
}

pub trait Sql {
    fn query(&mut self, statement: &Statement, values: &[SqlValue]) -> Result<Vec<Vec<Option<String>>>, SqlError>;
    fn rollback(&mut self);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    Held,
    Sql(String),
    Corrupt(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Held => write!(formatter, "journal: another owner holds an unexpired lease"),
            Self::Sql(error) => write!(formatter, "journal: {error}"),
            Self::Corrupt(error) => write!(formatter, "journal: {error}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SqlJournal {
    pub session: String,
    pub owner: String,
    pub lease_ms: u64,
    pub head: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opened {
    pub records: Vec<Record>,
    pub epoch: u64,
}

impl SqlJournal {
    fn value(&self, param: &Param, epoch: u64, batch: &str) -> SqlValue {
        let value = match param.name {
            "session" => SqlValue::Text(self.session.clone()),
            "owner" => SqlValue::Text(self.owner.clone()),
            "lease_ms" => SqlValue::Integer(self.lease_ms),
            "epoch" => SqlValue::Integer(epoch),
            "head" => SqlValue::Integer(self.head),
            "batch" => SqlValue::Json(batch.to_owned()),
            other => panic!("the catalogue names only journal parameters, found {other}"),
        };
        let fits = matches!(
            (&value, param.ty),
            (SqlValue::Text(_), ParamType::Text)
                | (SqlValue::Integer(_), ParamType::Integer)
                | (SqlValue::Json(_), ParamType::Json)
        );
        assert!(fits, "parameter {} has its declared type", param.name);
        value
    }

    fn values(&self, statement: &Statement, epoch: u64, batch: &str) -> Vec<SqlValue> {
        statement
            .params
            .iter()
            .map(|param| self.value(param, epoch, batch))
            .collect()
    }
}

fn column(row: &[Option<String>], statement: &Statement, name: &str) -> Result<u64, OpenError> {
    let index = statement
        .returns
        .iter()
        .position(|returned| *returned == name)
        .expect("the statement returns the column");
    row.get(index)
        .and_then(|value| value.as_deref())
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| OpenError::Corrupt(format!("{} returned no number for {name}", statement.name)))
}

pub fn sql_prepare(sql: &mut dyn Sql) -> Result<(), OpenError> {
    for statement in CONNECT.iter().chain(SCHEMA) {
        assert!(statement.params.is_empty(), "setup statements take no parameters");
        sql.query(statement, &[]).map_err(|error| OpenError::Sql(format!("{}: {error:?}", statement.name)))?;
    }
    Ok(())
}

pub fn sql_open(sql: &mut dyn Sql, journal: &mut SqlJournal) -> Result<Opened, OpenError> {
    let values = journal.values(&ACQUIRE, 0, "");
    let rows = sql
        .query(&ACQUIRE, &values)
        .map_err(|error| OpenError::Sql(format!("acquire: {error:?}")))?;
    let Some(row) = rows.first() else {
        return Err(OpenError::Held);
    };
    if rows.len() != 1 {
        return Err(OpenError::Corrupt(format!("acquire returned {} rows", rows.len())));
    }
    let epoch = column(row, &ACQUIRE, "epoch")?;
    let head = column(row, &ACQUIRE, "head")?;
    let values = journal.values(&LOAD, epoch, "");
    let rows = sql
        .query(&LOAD, &values)
        .map_err(|error| OpenError::Sql(format!("load: {error:?}")))?;
    let mut records = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let seq = column(row, &LOAD, "seq")?;
        if seq != index as u64 + 1 {
            return Err(OpenError::Corrupt(format!("row {} has seq {seq}", index + 1)));
        }
        let text = row
            .get(LOAD.returns.iter().position(|name| *name == "record").expect("load returns record"))
            .and_then(|value| value.as_deref())
            .ok_or_else(|| OpenError::Corrupt(format!("row {} has no record", index + 1)))?;
        records.push(decode_line(text, false).map_err(|error| OpenError::Corrupt(format!("row {}: {error}", index + 1)))?);
    }
    if records.len() as u64 != head {
        return Err(OpenError::Corrupt(format!(
            "the session's head is {head} but {} rows load",
            records.len()
        )));
    }
    journal.head = head;
    Ok(Opened { records, epoch })
}

pub fn batch_json(records: &[Record], head: u64) -> String {
    assert!(!records.is_empty(), "a durability point commits at least one record");
    let mut rows = Vec::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        let text = record_json(record).encode();
        assert_eq!(decode_line(&text, false).as_ref(), Ok(record), "a row's record decodes back");
        rows.push(Json::object(vec![
            ("kind", Json::text(record.tag())),
            ("record", Json::String(text)),
            ("seq", Json::Number(head + 1 + index as u64)),
            ("turn", Json::Number(record.turn())),
        ]));
    }
    Json::Array(rows).encode()
}

pub fn sql_flush(sql: &mut dyn Sql, journal: &mut SqlJournal, epoch: u64, records: &[Record]) -> Result<(), Stop> {
    let batch = batch_json(records, journal.head);
    for statement in COMMIT {
        let values = journal.values(statement, epoch, &batch);
        match sql.query(statement, &values) {
            Ok(_) => {}
            Err(SqlError::Fenced) => {
                sql.rollback();
                return Err(Stop::Fenced);
            }
            Err(SqlError::Other(_)) => {
                sql.rollback();
                return Err(Stop::Crash);
            }
        }
    }
    journal.head += records.len() as u64;
    Ok(())
}

pub struct SqlWorld<'a, H: Host> {
    pub host: &'a mut H,
    pub sql: &'a mut dyn Sql,
    pub journal: &'a mut SqlJournal,
}

impl<H: Host> World for SqlWorld<'_, H> {
    fn hook(&mut self, point: Point) -> Result<(), Stop> {
        self.host.hook(point)
    }

    fn flush(&mut self, epoch: u64, records: &[Record]) -> Result<(), Stop> {
        sql_flush(self.sql, self.journal, epoch, records)
    }

    fn tool(&mut self, request: &ToolRequest<'_>) -> Output {
        self.host.tool(request)
    }

    fn model(&mut self, request: &ModelRequest<'_>) -> Json {
        self.host.model(request)
    }

    fn now(&mut self) -> u64 {
        self.host.now()
    }

    fn random(&mut self) -> u64 {
        self.host.random()
    }

    fn acknowledge(&mut self, finish: &Finish) {
        self.host.acknowledge(finish)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunError {
    Open(OpenError),
    Exit(Exit),
}

pub fn run_sql<H: Host>(host: &mut H, sql: &mut dyn Sql, journal: &mut SqlJournal, request: &TurnRequest) -> Result<Ran, RunError> {
    let opened = sql_open(sql, journal).map_err(RunError::Open)?;
    let mut world = SqlWorld { host, sql, journal };
    let ran = run_turn(&mut world, opened.epoch, &opened.records, request).map_err(RunError::Exit)?;
    if let Ran::AlreadyFinished(finish) = &ran {
        world.host.acknowledge(finish);
    }
    Ok(ran)
}
