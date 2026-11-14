use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::time::{Duration, Instant};

use crate::Bench;
use crate::harness::{self, Ran, Sql, SqlError, SqlJournal, SqlValue, Statement, TurnRequest};

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_NULL: c_int = 5;
const SQLITE_CONSTRAINT_NOTNULL: c_int = 1299;
const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
const SQLITE_OPEN_CREATE: c_int = 0x0000_0004;
const SQLITE_STATIC: *const c_void = std::ptr::null();
const SQLITE_TABLE_ROWS_MAX: u64 = u64::MAX;
const SQL_NUL_TERMINATED: c_int = -1;
const SESSION: &str = "bench";
const OWNER: &str = "bench";
const LEASE_MS: u64 = 60_000;

type Sqlite3 = c_void;
type Sqlite3Statement = c_void;

unsafe extern "C" {
    fn sqlite3_open_v2(
        filename: *const c_char,
        database: *mut *mut Sqlite3,
        flags: c_int,
        vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_close_v2(database: *mut Sqlite3) -> c_int;
    fn sqlite3_prepare_v2(
        database: *mut Sqlite3,
        sql: *const c_char,
        bytes: c_int,
        statement: *mut *mut Sqlite3Statement,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_bind_text(
        statement: *mut Sqlite3Statement,
        index: c_int,
        text: *const c_char,
        bytes: c_int,
        destructor: *const c_void,
    ) -> c_int;
    fn sqlite3_bind_int64(statement: *mut Sqlite3Statement, index: c_int, value: i64) -> c_int;
    fn sqlite3_step(statement: *mut Sqlite3Statement) -> c_int;
    fn sqlite3_reset(statement: *mut Sqlite3Statement) -> c_int;
    fn sqlite3_clear_bindings(statement: *mut Sqlite3Statement) -> c_int;
    fn sqlite3_finalize(statement: *mut Sqlite3Statement) -> c_int;
    fn sqlite3_column_count(statement: *mut Sqlite3Statement) -> c_int;
    fn sqlite3_column_type(statement: *mut Sqlite3Statement, column: c_int) -> c_int;
    fn sqlite3_column_text(statement: *mut Sqlite3Statement, column: c_int) -> *const u8;
    fn sqlite3_column_bytes(statement: *mut Sqlite3Statement, column: c_int) -> c_int;
    fn sqlite3_errmsg(database: *mut Sqlite3) -> *const c_char;
    fn sqlite3_extended_errcode(database: *mut Sqlite3) -> c_int;
    fn sqlite3_get_autocommit(database: *mut Sqlite3) -> c_int;
    fn sqlite3_exec(
        database: *mut Sqlite3,
        sql: *const c_char,
        callback: *const c_void,
        argument: *mut c_void,
        error: *mut *mut c_char,
    ) -> c_int;
}

struct Database {
    handle: *mut Sqlite3,
    prepared: BTreeMap<&'static str, *mut Sqlite3Statement>,
    commit_started: Option<Instant>,
    commit_time: Duration,
    commits: u64,
    statements: u64,
}

impl Database {
    fn open(path: &std::path::Path) -> Self {
        let name = CString::new(path.as_os_str().as_encoded_bytes())
            .expect("a bench path has no NUL byte");
        let mut handle = std::ptr::null_mut();
        let flags = SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE;
        let opened =
            unsafe { sqlite3_open_v2(name.as_ptr(), &mut handle, flags, std::ptr::null()) };
        assert!(!handle.is_null(), "sqlite3_open_v2 returns a handle");
        let database = Self {
            handle,
            prepared: BTreeMap::new(),
            commit_started: None,
            commit_time: Duration::ZERO,
            commits: 0,
            statements: 0,
        };
        if opened != SQLITE_OK {
            panic!("{path:?}: {}", database.message());
        }
        database
    }

    fn message(&self) -> String {
        let text = unsafe { CStr::from_ptr(sqlite3_errmsg(self.handle)) };
        text.to_string_lossy().into_owned()
    }

    fn error(&self) -> SqlError {
        if unsafe { sqlite3_extended_errcode(self.handle) } == SQLITE_CONSTRAINT_NOTNULL {
            return SqlError::Fenced;
        }
        SqlError::Other(self.message())
    }

    fn statement(&mut self, statement: &Statement) -> Result<*mut Sqlite3Statement, SqlError> {
        if let Some(prepared) = self.prepared.get(statement.name) {
            return Ok(*prepared);
        }
        let text = CString::new(statement.sql).expect("catalogued SQL has no NUL byte");
        let mut prepared = std::ptr::null_mut();
        let status = unsafe {
            sqlite3_prepare_v2(
                self.handle,
                text.as_ptr(),
                SQL_NUL_TERMINATED,
                &mut prepared,
                std::ptr::null_mut(),
            )
        };
        if status != SQLITE_OK {
            return Err(self.error());
        }
        assert!(!prepared.is_null(), "a catalogued statement is not empty");
        self.prepared.insert(statement.name, prepared);
        Ok(prepared)
    }

    fn bind(&self, prepared: *mut Sqlite3Statement, values: &[SqlValue]) -> Result<(), SqlError> {
        for (offset, value) in values.iter().enumerate() {
            let index = c_int::try_from(offset + 1).expect("a parameter index fits in an int");
            let status = match value {
                SqlValue::Integer(number) => {
                    let number = i64::try_from(*number).expect("a journal integer fits in i64");
                    unsafe { sqlite3_bind_int64(prepared, index, number) }
                }
                SqlValue::Text(text) | SqlValue::Json(text) => {
                    let bytes = c_int::try_from(text.len()).expect("a bound text fits in an int");
                    unsafe {
                        sqlite3_bind_text(
                            prepared,
                            index,
                            text.as_ptr().cast(),
                            bytes,
                            SQLITE_STATIC,
                        )
                    }
                }
            };
            if status != SQLITE_OK {
                return Err(self.error());
            }
        }
        Ok(())
    }

    fn row(prepared: *mut Sqlite3Statement) -> Vec<Option<String>> {
        let columns = unsafe { sqlite3_column_count(prepared) };
        (0..columns)
            .map(|column| unsafe {
                if sqlite3_column_type(prepared, column) == SQLITE_NULL {
                    return None;
                }
                let text = sqlite3_column_text(prepared, column);
                let bytes = usize::try_from(sqlite3_column_bytes(prepared, column))
                    .expect("sqlite reports a non-negative length");
                let slice = std::slice::from_raw_parts(text, bytes);
                Some(String::from_utf8_lossy(slice).into_owned())
            })
            .collect()
    }

    fn step(&self, prepared: *mut Sqlite3Statement) -> Result<Vec<Vec<Option<String>>>, SqlError> {
        let mut rows = Vec::new();
        for _ in 0..SQLITE_TABLE_ROWS_MAX {
            match unsafe { sqlite3_step(prepared) } {
                SQLITE_ROW => rows.push(Self::row(prepared)),
                SQLITE_DONE => return Ok(rows),
                _ => return Err(self.error()),
            }
        }
        panic!("a statement returned more rows than a SQLite table holds")
    }
}

impl Sql for Database {
    fn query(
        &mut self,
        statement: &Statement,
        values: &[SqlValue],
    ) -> Result<Vec<Vec<Option<String>>>, SqlError> {
        assert_eq!(
            values.len(),
            statement.params.len(),
            "one value per parameter"
        );
        self.statements += 1;
        let begins = harness::COMMIT
            .first()
            .is_some_and(|commit| commit.name == statement.name);
        let ends = harness::COMMIT
            .last()
            .is_some_and(|commit| commit.name == statement.name);
        if begins {
            assert!(self.commit_started.is_none(), "commits do not nest");
            self.commit_started = Some(Instant::now());
        }
        let prepared = self.statement(statement)?;
        let result = self
            .bind(prepared, values)
            .and_then(|()| self.step(prepared));
        unsafe {
            sqlite3_reset(prepared);
            sqlite3_clear_bindings(prepared);
        }
        if ends && result.is_ok() {
            let started = self
                .commit_started
                .take()
                .expect("a commit ends after it begins");
            self.commit_time += started.elapsed();
            self.commits += 1;
        }
        result
    }

    fn rollback(&mut self) {
        self.commit_started = None;
        if unsafe { sqlite3_get_autocommit(self.handle) } != 0 {
            return;
        }
        let text = CString::new("ROLLBACK").expect("no NUL byte");
        let status = unsafe {
            sqlite3_exec(
                self.handle,
                text.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(
            status, SQLITE_OK,
            "ROLLBACK of an open transaction succeeds"
        );
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        for prepared in self.prepared.values() {
            unsafe { sqlite3_finalize(*prepared) };
        }
        let closed = unsafe { sqlite3_close_v2(self.handle) };
        assert_eq!(closed, SQLITE_OK, "the database closes");
    }
}

fn journal() -> SqlJournal {
    SqlJournal {
        session: SESSION.to_owned(),
        owner: OWNER.to_owned(),
        lease_ms: LEASE_MS,
        head: 0,
    }
}

pub struct Session {
    database: Database,
    journal: SqlJournal,
}

impl Session {
    pub fn create(path: &std::path::Path) -> Self {
        assert!(!path.exists(), "the bench starts from no database");
        let mut database = Database::open(path);
        harness::sql_prepare(&mut database).unwrap_or_else(|error| panic!("{error}"));
        Self {
            database,
            journal: journal(),
        }
    }

    pub fn turn(&mut self, bench: &mut Bench, request: &TurnRequest) -> Ran {
        harness::run_sql(bench, &mut self.database, &mut self.journal, request)
            .unwrap_or_else(|error| panic!("turn {}: {error:?}", request.turn))
    }

    pub fn report(&self, _path: &std::path::Path) -> String {
        format!(
            "records={} flushes={} flush_ms={:.3} statements={}",
            self.journal.head,
            self.database.commits,
            crate::milliseconds(self.database.commit_time),
            self.database.statements
        )
    }
}

pub struct Opened {
    pub session: harness::Session,
    pub records: usize,
    pub report: String,
}

pub fn open(path: &std::path::Path) -> Opened {
    assert!(path.exists(), "the database to open exists");
    let mut database = Database::open(path);
    harness::sql_prepare(&mut database).unwrap_or_else(|error| panic!("{error}"));
    let mut journal = journal();
    let opened =
        harness::sql_open(&mut database, &mut journal).unwrap_or_else(|error| panic!("{error}"));
    let session =
        harness::fold(&opened.records).unwrap_or_else(|error| panic!("the journal folds: {error}"));
    Opened {
        session,
        records: opened.records.len(),
        report: format!("epoch={}", opened.epoch),
    }
}
