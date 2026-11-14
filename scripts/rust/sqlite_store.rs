use std::ffi::{CStr, CString, c_char, c_int, c_void};

use crate::harness::{FENCED_ERROR, ParamType, Sql, SqlError, SqlValue, Statement};

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_NULL: c_int = 5;
const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
const SQLITE_OPEN_CREATE: c_int = 0x0000_0004;
const SQLITE_CONSTRAINT_NOTNULL: c_int = 1299;
const SQLITE_TRANSIENT: isize = -1;
const NUL_TERMINATED: c_int = -1;
const JSON_EACH_AND_RETURNING_VERSION_MIN: c_int = 3_038_000;
const FENCED_CODE_PREFIX: &str = "SQLITE_CONSTRAINT_NOTNULL: ";

#[repr(C)]
struct Connection {
    _opaque: [u8; 0],
}

#[repr(C)]
struct Prepared {
    _opaque: [u8; 0],
}

#[link(name = "libsqlite3.so.0", kind = "dylib", modifiers = "+verbatim")]
unsafe extern "C" {
    fn sqlite3_libversion_number() -> c_int;
    fn sqlite3_open_v2(
        path: *const c_char,
        db: *mut *mut Connection,
        flags: c_int,
        vfs: *const c_char,
    ) -> c_int;
    fn sqlite3_close(db: *mut Connection) -> c_int;
    fn sqlite3_extended_result_codes(db: *mut Connection, on: c_int) -> c_int;
    fn sqlite3_busy_timeout(db: *mut Connection, ms: c_int) -> c_int;
    fn sqlite3_prepare_v2(
        db: *mut Connection,
        sql: *const c_char,
        bytes: c_int,
        statement: *mut *mut Prepared,
        tail: *mut *const c_char,
    ) -> c_int;
    fn sqlite3_bind_parameter_count(statement: *mut Prepared) -> c_int;
    fn sqlite3_bind_text(
        statement: *mut Prepared,
        index: c_int,
        text: *const c_char,
        bytes: c_int,
        destructor: isize,
    ) -> c_int;
    fn sqlite3_bind_int64(statement: *mut Prepared, index: c_int, value: i64) -> c_int;
    fn sqlite3_step(statement: *mut Prepared) -> c_int;
    fn sqlite3_column_count(statement: *mut Prepared) -> c_int;
    fn sqlite3_column_type(statement: *mut Prepared, column: c_int) -> c_int;
    fn sqlite3_column_text(statement: *mut Prepared, column: c_int) -> *const u8;
    fn sqlite3_column_bytes(statement: *mut Prepared, column: c_int) -> c_int;
    fn sqlite3_finalize(statement: *mut Prepared) -> c_int;
    fn sqlite3_errmsg(db: *mut Connection) -> *const c_char;
    fn sqlite3_extended_errcode(db: *mut Connection) -> c_int;
    fn sqlite3_get_autocommit(db: *mut Connection) -> c_int;
    fn sqlite3_exec(
        db: *mut Connection,
        sql: *const c_char,
        callback: *const c_void,
        argument: *mut c_void,
        error: *mut *mut c_char,
    ) -> c_int;
}

pub struct SqliteStore {
    db: *mut Connection,
}

pub fn version() -> c_int {
    unsafe { sqlite3_libversion_number() }
}

impl SqliteStore {
    pub fn open(path: &str, busy_timeout_ms: c_int) -> Result<Self, String> {
        let version = version();
        if version < JSON_EACH_AND_RETURNING_VERSION_MIN {
            return Err(format!(
                "libsqlite3 {version} is older than {JSON_EACH_AND_RETURNING_VERSION_MIN}, which json_each and RETURNING need"
            ));
        }
        let path_text = CString::new(path).map_err(|_| format!("{path:?} holds a NUL byte"))?;
        let mut db = std::ptr::null_mut();
        let flags = SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE;
        let opened =
            unsafe { sqlite3_open_v2(path_text.as_ptr(), &mut db, flags, std::ptr::null()) };
        let store = Self { db };
        if opened != SQLITE_OK {
            return Err(format!("open {path}: {}", store.error_text()));
        }
        assert!(
            !store.db.is_null(),
            "a successful open returns a connection"
        );
        unsafe {
            sqlite3_extended_result_codes(store.db, 1);
            sqlite3_busy_timeout(store.db, busy_timeout_ms);
        }
        Ok(store)
    }

    fn error_text(&self) -> String {
        if self.db.is_null() {
            return "no connection".to_owned();
        }
        let message = unsafe { CStr::from_ptr(sqlite3_errmsg(self.db)) };
        message.to_string_lossy().into_owned()
    }

    fn error(&self, statement: &Statement) -> SqlError {
        let code = unsafe { sqlite3_extended_errcode(self.db) };
        let message = self.error_text();
        let fenced_message = FENCED_ERROR
            .strip_prefix(FENCED_CODE_PREFIX)
            .expect("the catalogue names the SQLite fence by its extended code");
        if code == SQLITE_CONSTRAINT_NOTNULL && message == fenced_message {
            return SqlError::Fenced;
        }
        SqlError::Other(format!(
            "{}: sqlite error {code}: {message}",
            statement.name
        ))
    }

    fn prepare(&self, statement: &Statement) -> Result<*mut Prepared, SqlError> {
        let sql = CString::new(statement.sql).expect("catalogue SQL holds no NUL byte");
        let mut prepared = std::ptr::null_mut();
        let result = unsafe {
            sqlite3_prepare_v2(
                self.db,
                sql.as_ptr(),
                NUL_TERMINATED,
                &mut prepared,
                std::ptr::null_mut(),
            )
        };
        if result != SQLITE_OK {
            return Err(self.error(statement));
        }
        assert!(!prepared.is_null(), "a catalogue statement is not empty");
        Ok(prepared)
    }

    fn bind(
        &self,
        prepared: *mut Prepared,
        statement: &Statement,
        values: &[SqlValue],
    ) -> Result<(), SqlError> {
        let count = unsafe { sqlite3_bind_parameter_count(prepared) };
        assert_eq!(
            count as usize,
            statement.params.len(),
            "{} binds every parameter it names",
            statement.name
        );
        assert_eq!(
            values.len(),
            statement.params.len(),
            "one value per parameter"
        );
        for (index, (param, value)) in statement.params.iter().zip(values).enumerate() {
            let position = index as c_int + 1;
            let result = match (param.ty, value) {
                (ParamType::Integer, SqlValue::Integer(number)) => {
                    let number =
                        i64::try_from(*number).expect("journal integers fit SQLite's INTEGER");
                    unsafe { sqlite3_bind_int64(prepared, position, number) }
                }
                (ParamType::Text, SqlValue::Text(text))
                | (ParamType::Json, SqlValue::Json(text)) => {
                    let bytes =
                        c_int::try_from(text.len()).expect("a bound text fits SQLite's length");
                    unsafe {
                        sqlite3_bind_text(
                            prepared,
                            position,
                            text.as_ptr().cast(),
                            bytes,
                            SQLITE_TRANSIENT,
                        )
                    }
                }
                _ => panic!(
                    "parameter {} of {} has its declared type",
                    param.name, statement.name
                ),
            };
            if result != SQLITE_OK {
                return Err(self.error(statement));
            }
        }
        Ok(())
    }

    fn row(prepared: *mut Prepared) -> Vec<Option<String>> {
        let columns = unsafe { sqlite3_column_count(prepared) };
        (0..columns)
            .map(|column| unsafe {
                if sqlite3_column_type(prepared, column) == SQLITE_NULL {
                    return None;
                }
                let text = sqlite3_column_text(prepared, column);
                let bytes = sqlite3_column_bytes(prepared, column) as usize;
                let slice = std::slice::from_raw_parts(text, bytes);
                Some(String::from_utf8_lossy(slice).into_owned())
            })
            .collect()
    }

    fn step_all(
        &self,
        prepared: *mut Prepared,
        statement: &Statement,
    ) -> Result<Vec<Vec<Option<String>>>, SqlError> {
        let mut rows = Vec::new();
        loop {
            match unsafe { sqlite3_step(prepared) } {
                SQLITE_ROW => rows.push(Self::row(prepared)),
                SQLITE_DONE => return Ok(rows),
                _ => return Err(self.error(statement)),
            }
        }
    }

    pub fn in_transaction(&self) -> bool {
        unsafe { sqlite3_get_autocommit(self.db) == 0 }
    }
}

impl Sql for SqliteStore {
    fn query(
        &mut self,
        statement: &Statement,
        values: &[SqlValue],
    ) -> Result<Vec<Vec<Option<String>>>, SqlError> {
        let prepared = self.prepare(statement)?;
        let result = self
            .bind(prepared, statement, values)
            .and_then(|()| self.step_all(prepared, statement));
        unsafe { sqlite3_finalize(prepared) };
        result
    }

    fn rollback(&mut self) {
        if !self.in_transaction() {
            return;
        }
        let sql = CString::new("ROLLBACK").expect("no NUL byte");
        let result = unsafe {
            sqlite3_exec(
                self.db,
                sql.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if result != SQLITE_OK {
            eprintln!("rollback: {}", self.error_text());
        }
    }
}

impl Drop for SqliteStore {
    fn drop(&mut self) {
        unsafe { sqlite3_close(self.db) };
    }
}
