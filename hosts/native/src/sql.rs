use harnesslang::emit::sql as catalogue;
use harnesslang::target::Target;
use harnesslang::wasm::js::Json;
use harnesslang::wasm::native::Store;

use crate::store_postgres::PostgresStore;
use crate::store_sqlite::SqliteStore;

pub const FENCED_ERROR: &str = catalogue::Dialect::Sqlite.fenced_error();
pub const POSTGRES_FENCED_ERROR: &str = "SQLSTATE 23502 not_null_violation on hl_journal.epoch";
pub const POSTGRES_USER: &str = "postgres";
pub const BUSY_TIMEOUT_MS: i32 = 10_000;
const LINE_END: char = '\n';

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
    fn query(
        &mut self,
        statement: &Statement,
        values: &[SqlValue],
    ) -> Result<Vec<Vec<Option<String>>>, SqlError>;
    fn rollback(&mut self);
}

pub enum Database {
    Sqlite(SqliteStore),
    Postgres(PostgresStore),
}

impl Database {
    pub fn sql(&mut self) -> &mut dyn Sql {
        match self {
            Self::Sqlite(store) => store,
            Self::Postgres(store) => store,
        }
    }
}

pub fn connect(target: Target, location: &str, application: &str) -> Result<Database, String> {
    match target {
        Target::NativeSqlite => SqliteStore::open(location, BUSY_TIMEOUT_MS).map(Database::Sqlite),
        Target::ServerPostgres => {
            assert_eq!(
                catalogue::Dialect::Postgres.fenced_error(),
                POSTGRES_FENCED_ERROR,
                "fence mismatch"
            );
            let (address, database) = location
                .split_once('/')
                .ok_or_else(|| format!("{location} is not host:port/database"))?;
            PostgresStore::connect(address, POSTGRES_USER, database, application)
                .map(Database::Postgres)
        }
        other => Err(format!("{}: not SQL", other.name())),
    }
}

fn leak(statement: &catalogue::Statement) -> Statement {
    let params: Vec<Param> = statement
        .params
        .iter()
        .map(|param| Param {
            name: param.name,
            ty: match param.ty {
                catalogue::ParamType::Text => ParamType::Text,
                catalogue::ParamType::Integer => ParamType::Integer,
                catalogue::ParamType::Json => ParamType::Json,
            },
        })
        .collect();
    Statement {
        name: statement.name,
        sql: Box::leak(statement.sql().into_boxed_str()),
        params: Box::leak(params.into_boxed_slice()),
        returns: Box::leak(statement.returns.clone().into_boxed_slice()),
    }
}

pub struct Statements {
    pub setup: Vec<Statement>,
    pub acquire: Statement,
    pub load: Statement,
    pub commit: Vec<Statement>,
}

pub fn statements(target: Target, mutation: catalogue::Mutation) -> Result<Statements, String> {
    let catalogue = catalogue::catalogue(target, mutation)?;
    Ok(Statements {
        setup: catalogue
            .connect
            .iter()
            .chain(&catalogue.schema)
            .map(leak)
            .collect(),
        acquire: leak(&catalogue.acquire),
        load: leak(&catalogue.load),
        commit: catalogue.commit.iter().map(leak).collect(),
    })
}

pub struct Lease {
    pub session: String,
    pub owner: String,
    pub lease_ms: u64,
}

impl Lease {
    fn value(&self, param: &Param, epoch: u64, head: u64, batch: &str) -> SqlValue {
        match param.name {
            "session" => SqlValue::Text(self.session.clone()),
            "owner" => SqlValue::Text(self.owner.clone()),
            "lease_ms" => SqlValue::Integer(self.lease_ms),
            "epoch" => SqlValue::Integer(epoch),
            "head" => SqlValue::Integer(head),
            "batch" => SqlValue::Json(batch.to_owned()),
            other => panic!("unknown parameter: {other}"),
        }
    }

    fn values(&self, statement: &Statement, epoch: u64, head: u64, batch: &str) -> Vec<SqlValue> {
        statement
            .params
            .iter()
            .map(|param| self.value(param, epoch, head, batch))
            .collect()
    }
}

fn column(row: &[Option<String>], statement: &Statement, name: &str) -> Result<String, String> {
    let index = statement
        .returns
        .iter()
        .position(|returned| *returned == name)
        .expect("column missing");
    row.get(index)
        .and_then(|value| value.clone())
        .ok_or_else(|| format!("{} returned no {name}", statement.name))
}

pub fn setup(sql: &mut dyn Sql, statements: &Statements) -> Result<(), String> {
    for statement in &statements.setup {
        sql.query(statement, &[])
            .map_err(|error| format!("{}: {error:?}", statement.name))?;
    }
    Ok(())
}

pub fn acquire(
    sql: &mut dyn Sql,
    statements: &Statements,
    lease: &Lease,
) -> Result<Option<(u64, u64)>, String> {
    let acquire = &statements.acquire;
    let rows = sql
        .query(acquire, &lease.values(acquire, 0, 0, ""))
        .map_err(|error| format!("acquire: {error:?}"))?;
    let [row] = rows.as_slice() else {
        assert!(rows.is_empty(), "acquire: many rows");
        return Ok(None);
    };
    let read = |name: &str| -> Result<u64, String> {
        column(row, acquire, name)?
            .parse()
            .map_err(|_| format!("acquire: {name} not numeric"))
    };
    Ok(Some((read("epoch")?, read("head")?)))
}

pub fn load_lines(sql: &mut dyn Sql, load: &Statement, lease: &Lease) -> Result<String, SqlError> {
    let rows = sql.query(load, &lease.values(load, 0, 0, ""))?;
    let mut text = String::new();
    for (index, row) in rows.iter().enumerate() {
        let seq = column(row, load, "seq").map_err(SqlError::Other)?;
        assert_eq!(seq, (index + 1).to_string(), "seq gap");
        let record = column(row, load, "record").map_err(SqlError::Other)?;
        assert!(!record.contains(LINE_END), "multiline row");
        text.push_str(&record);
        text.push(LINE_END);
    }
    Ok(text)
}

pub enum Opened<'h> {
    Held,
    Owned(SqlJournal<'h>),
}

pub fn open_journal<'h>(
    target: Target,
    location: &str,
    application: &str,
    lease: Lease,
    mutation: catalogue::Mutation,
) -> Result<Opened<'h>, String> {
    let mut db = connect(target, location, application)?;
    let statements = statements(target, mutation)?;
    setup(db.sql(), &statements)?;
    let Some((epoch, head)) = acquire(db.sql(), &statements, &lease)? else {
        return Ok(Opened::Held);
    };
    Ok(Opened::Owned(SqlJournal::new(
        db, statements, lease, epoch, head,
    )))
}

pub fn read_lines(target: Target, location: &str, session: &str) -> Result<String, String> {
    let mut db = connect(target, location, "read")?;
    let statements = statements(target, catalogue::Mutation::None)?;
    setup(db.sql(), &statements)?;
    let lease = Lease {
        session: session.to_owned(),
        owner: String::new(),
        lease_ms: 0,
    };
    load_lines(db.sql(), &statements.load, &lease).map_err(|error| format!("load: {error:?}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Fenced,
    Lost,
}

fn object(fields: Vec<(&str, Json)>) -> Json {
    Json::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

pub struct SqlJournal<'h> {
    pub db: Database,
    pub statements: Statements,
    pub lease: Lease,
    pub epoch: u64,
    pub head: u64,
    pub failure: Option<Failure>,
    pub before_append: Option<&'h dyn Fn()>,
    read: u64,
}

impl<'h> SqlJournal<'h> {
    pub fn new(db: Database, statements: Statements, lease: Lease, epoch: u64, head: u64) -> Self {
        Self {
            db,
            statements,
            lease,
            epoch,
            head,
            failure: None,
            before_append: None,
            read: 0,
        }
    }

    fn batch(&self, bytes: &[u8]) -> Result<(String, u64), String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "flush not UTF-8".to_owned())?;
        let lines: Vec<&str> = text.split_terminator(LINE_END).collect();
        assert!(
            !lines.is_empty() && text.ends_with(LINE_END),
            "partial flush line"
        );
        let mut rows = Vec::with_capacity(lines.len());
        for (index, line) in lines.iter().enumerate() {
            let record = harnesslang::run::journal::decode_line(line, false)
                .map_err(|error| format!("flush undecodable (non-SQL module?): {error}"))?;
            rows.push(object(vec![
                ("kind", Json::text(catalogue::record_kind(&record))),
                ("record", Json::text(line)),
                ("seq", Json::int(self.head + 1 + index as u64)),
                ("turn", Json::int(catalogue::record_turn(&record))),
            ]));
        }
        Ok((Json::Array(rows).encode(), lines.len() as u64))
    }
}

impl Store for SqlJournal<'_> {
    fn read(&mut self) -> Result<Vec<u8>, String> {
        let text =
            load_lines(self.db.sql(), &self.statements.load, &self.lease).map_err(|error| {
                self.failure = Some(Failure::Lost);
                format!("load: {error:?}")
            })?;
        let rows = text.matches(LINE_END).count() as u64;
        if rows != self.head {
            return Err(format!(
                "the session's head is {} but {rows} rows load",
                self.head
            ));
        }
        self.read = text.len() as u64;
        Ok(text.into_bytes())
    }

    fn append(&mut self, epoch: i64, bytes: &[u8]) -> Result<(), String> {
        if let Some(hook) = self.before_append {
            hook();
        }
        let epoch = u64::try_from(epoch).map_err(|_| "negative epoch".to_owned())?;
        let (batch, rows) = self.batch(bytes)?;
        for statement in &self.statements.commit {
            let values = self.lease.values(statement, epoch, self.head, &batch);
            match self.db.sql().query(statement, &values) {
                Ok(_) => {}
                Err(SqlError::Fenced) => {
                    self.db.sql().rollback();
                    self.failure = Some(Failure::Fenced);
                    return Err(format!("fenced: epoch {epoch} superseded"));
                }
                Err(SqlError::Other(error)) => {
                    self.db.sql().rollback();
                    self.failure = Some(Failure::Lost);
                    return Err(format!("store lost mid-flush: {error}"));
                }
            }
        }
        self.head += rows;
        self.read += bytes.len() as u64;
        Ok(())
    }

    fn keep(&mut self, length: u64) -> Result<(), String> {
        if length == self.read {
            return Ok(());
        }
        Err(format!(
            "SQL journal untorn ({length} of {} bytes kept)",
            self.read
        ))
    }

    fn epoch(&self) -> i64 {
        self.epoch as i64
    }
}
