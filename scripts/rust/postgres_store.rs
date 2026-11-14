use std::collections::BTreeMap;
use std::io::{BufReader, Read, Write};
use std::net::TcpStream;

use crate::harness::{ParamType, Sql, SqlError, SqlValue, Statement};

const PROTOCOL_VERSION_3: i32 = 196_608;
const AUTHENTICATION_OK: i32 = 0;
const OID_TEXT: i32 = 25;
const OID_INT8: i32 = 20;
const OID_JSON: i32 = 114;
const FORMAT_TEXT: i16 = 0;
const NULL_LENGTH: i32 = -1;
const LENGTH_BYTES: usize = 4;
const MESSAGE_BYTES_MAX: usize = 0x3fff_ffff;
const EXECUTE_ROWS_ALL: i32 = 0;
const SQLSTATE_NOT_NULL_VIOLATION: &str = "23502";
const FENCED_TABLE: &str = "hl_journal";
const FENCED_COLUMN: &str = "epoch";
const TRANSACTION_IDLE: u8 = b'I';
const FIELD_CODE: u8 = b'C';
const FIELD_MESSAGE: u8 = b'M';
const FIELD_TABLE: u8 = b't';
const FIELD_COLUMN: u8 = b'c';

pub struct PostgresStore {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    pub backend_pid: i32,
    pub server_version: String,
    transaction: u8,
    broken: Option<String>,
}

struct Message {
    tag: u8,
    body: Vec<u8>,
}

struct Fields(BTreeMap<u8, String>);

impl Fields {
    fn parse(body: &[u8]) -> Self {
        let mut fields = BTreeMap::new();
        let mut rest = body;
        while let Some((&code, tail)) = rest.split_first() {
            if code == 0 {
                break;
            }
            let end = tail
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(tail.len());
            fields.insert(code, String::from_utf8_lossy(&tail[..end]).into_owned());
            rest = tail.get(end + 1..).unwrap_or(&[]);
        }
        Self(fields)
    }

    fn get(&self, code: u8) -> &str {
        self.0.get(&code).map_or("", String::as_str)
    }

    fn fenced(&self) -> bool {
        self.get(FIELD_CODE) == SQLSTATE_NOT_NULL_VIOLATION
            && self.get(FIELD_TABLE) == FENCED_TABLE
            && self.get(FIELD_COLUMN) == FENCED_COLUMN
    }

    fn describe(&self) -> String {
        format!(
            "SQLSTATE {} {}",
            self.get(FIELD_CODE),
            self.get(FIELD_MESSAGE)
        )
    }
}

fn put_cstr(out: &mut Vec<u8>, text: &str) {
    assert!(
        !text.as_bytes().contains(&0),
        "a protocol string holds no NUL byte"
    );
    out.extend_from_slice(text.as_bytes());
    out.push(0);
}

fn put_i32(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_i16(out: &mut Vec<u8>, value: i16) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn frame(out: &mut Vec<u8>, tag: u8, body: &[u8]) {
    out.push(tag);
    let length =
        i32::try_from(body.len() + LENGTH_BYTES).expect("a frontend message fits its length field");
    put_i32(out, length);
    out.extend_from_slice(body);
}

fn count_i16(count: usize) -> i16 {
    i16::try_from(count).expect("a statement's parameter count fits the protocol's int16")
}

fn oid(ty: ParamType) -> i32 {
    match ty {
        ParamType::Text => OID_TEXT,
        ParamType::Integer => OID_INT8,
        ParamType::Json => OID_JSON,
    }
}

fn value_text(value: &SqlValue) -> String {
    match value {
        SqlValue::Text(text) | SqlValue::Json(text) => text.clone(),
        SqlValue::Integer(number) => number.to_string(),
    }
}

fn parse_message(statement: &Statement) -> Vec<u8> {
    let mut body = Vec::new();
    put_cstr(&mut body, "");
    put_cstr(&mut body, statement.sql);
    put_i16(&mut body, count_i16(statement.params.len()));
    for param in statement.params {
        put_i32(&mut body, oid(param.ty));
    }
    body
}

fn bind_message(statement: &Statement, values: &[SqlValue]) -> Vec<u8> {
    let mut body = Vec::new();
    put_cstr(&mut body, "");
    put_cstr(&mut body, "");
    put_i16(&mut body, 1);
    put_i16(&mut body, FORMAT_TEXT);
    put_i16(&mut body, count_i16(values.len()));
    for (param, value) in statement.params.iter().zip(values) {
        let fits = matches!(
            (param.ty, value),
            (ParamType::Text, SqlValue::Text(_))
                | (ParamType::Integer, SqlValue::Integer(_))
                | (ParamType::Json, SqlValue::Json(_))
        );
        assert!(
            fits,
            "parameter {} of {} has its declared type",
            param.name, statement.name
        );
        let text = value_text(value);
        put_i32(
            &mut body,
            i32::try_from(text.len()).expect("a parameter fits the protocol's length"),
        );
        body.extend_from_slice(text.as_bytes());
    }
    put_i16(&mut body, 1);
    put_i16(&mut body, FORMAT_TEXT);
    body
}

fn execute_message() -> Vec<u8> {
    let mut body = Vec::new();
    put_cstr(&mut body, "");
    put_i32(&mut body, EXECUTE_ROWS_ALL);
    body
}

fn read_i16(body: &[u8], at: usize) -> Result<i16, String> {
    let bytes = body
        .get(at..at + 2)
        .ok_or("a message ends inside an int16")?;
    Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_i32(body: &[u8], at: usize) -> Result<i32, String> {
    let bytes = body
        .get(at..at + 4)
        .ok_or("a message ends inside an int32")?;
    Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn data_row(body: &[u8]) -> Result<Vec<Option<String>>, String> {
    let columns = read_i16(body, 0)?;
    let mut at = 2;
    let mut row = Vec::with_capacity(columns.max(0) as usize);
    for _ in 0..columns {
        let length = read_i32(body, at)?;
        at += LENGTH_BYTES;
        if length == NULL_LENGTH {
            row.push(None);
            continue;
        }
        let length =
            usize::try_from(length).map_err(|_| format!("a column has length {length}"))?;
        let bytes = body
            .get(at..at + length)
            .ok_or("a data row ends inside a column")?;
        row.push(Some(String::from_utf8_lossy(bytes).into_owned()));
        at += length;
    }
    Ok(row)
}

impl PostgresStore {
    pub fn connect(
        address: &str,
        user: &str,
        database: &str,
        application: &str,
    ) -> Result<Self, String> {
        let stream =
            TcpStream::connect(address).map_err(|error| format!("connect {address}: {error}"))?;
        stream
            .set_nodelay(true)
            .map_err(|error| format!("nodelay: {error}"))?;
        let writer = stream
            .try_clone()
            .map_err(|error| format!("clone: {error}"))?;
        let mut store = Self {
            reader: BufReader::new(stream),
            writer,
            backend_pid: 0,
            server_version: String::new(),
            transaction: TRANSACTION_IDLE,
            broken: None,
        };
        store.startup(user, database, application)?;
        assert!(store.backend_pid > 0, "the server names its backend");
        Ok(store)
    }

    fn startup(&mut self, user: &str, database: &str, application: &str) -> Result<(), String> {
        let mut body = Vec::new();
        put_i32(&mut body, PROTOCOL_VERSION_3);
        for (key, value) in [
            ("user", user),
            ("database", database),
            ("application_name", application),
        ] {
            put_cstr(&mut body, key);
            put_cstr(&mut body, value);
        }
        body.push(0);
        let mut out = Vec::new();
        put_i32(
            &mut out,
            i32::try_from(body.len() + LENGTH_BYTES).expect("a startup message is short"),
        );
        out.extend_from_slice(&body);
        self.writer
            .write_all(&out)
            .map_err(|error| format!("startup: {error}"))?;
        loop {
            let message = self.read_message()?;
            match message.tag {
                b'R' => {
                    let kind = read_i32(&message.body, 0)?;
                    if kind != AUTHENTICATION_OK {
                        return Err(format!("authentication method {kind} is not trust"));
                    }
                }
                b'K' => self.backend_pid = read_i32(&message.body, 0)?,
                b'S' => self.parameter(&message.body),
                b'Z' => return Ok(()),
                b'E' => {
                    return Err(format!(
                        "startup: {}",
                        Fields::parse(&message.body).describe()
                    ));
                }
                b'N' => {}
                other => return Err(format!("startup: unexpected message {:?}", other as char)),
            }
        }
    }

    fn parameter(&mut self, body: &[u8]) {
        let mut parts = body.split(|byte| *byte == 0);
        if parts.next() == Some(b"server_version") {
            self.server_version =
                String::from_utf8_lossy(parts.next().unwrap_or_default()).into_owned();
        }
    }

    fn read_message(&mut self) -> Result<Message, String> {
        let mut header = [0u8; 1 + LENGTH_BYTES];
        self.reader
            .read_exact(&mut header)
            .map_err(|error| format!("read: {error}"))?;
        let length = i32::from_be_bytes([header[1], header[2], header[3], header[4]]);
        let length =
            usize::try_from(length).map_err(|_| format!("a message has length {length}"))?;
        if length < LENGTH_BYTES || length > MESSAGE_BYTES_MAX {
            return Err(format!(
                "a message has length {length}, outside 4..={MESSAGE_BYTES_MAX}"
            ));
        }
        let mut body = vec![0u8; length - LENGTH_BYTES];
        self.reader
            .read_exact(&mut body)
            .map_err(|error| format!("read: {error}"))?;
        Ok(Message {
            tag: header[0],
            body,
        })
    }

    fn send(&mut self, statement: &Statement, values: &[SqlValue]) -> Result<(), String> {
        let mut out = Vec::new();
        frame(&mut out, b'P', &parse_message(statement));
        frame(&mut out, b'B', &bind_message(statement, values));
        frame(&mut out, b'E', &execute_message());
        frame(&mut out, b'S', &[]);
        self.writer
            .write_all(&out)
            .map_err(|error| format!("write: {error}"))
    }

    fn receive(&mut self) -> Result<Result<Vec<Vec<Option<String>>>, Fields>, String> {
        let mut rows = Vec::new();
        let mut failure = None;
        loop {
            let message = self.read_message()?;
            match message.tag {
                b'D' => rows.push(data_row(&message.body)?),
                b'E' => failure = failure.or(Some(Fields::parse(&message.body))),
                b'Z' => {
                    self.transaction =
                        *message.body.first().ok_or("ReadyForQuery has no status")?;
                    return Ok(failure.map_or(Ok(rows), Err));
                }
                b'1' | b'2' | b'C' | b'n' | b'I' | b'N' | b'S' | b's' => {}
                other => return Err(format!("unexpected message {:?}", other as char)),
            }
        }
    }

    fn lost(&mut self, statement: &Statement, error: String) -> SqlError {
        let text = format!("{}: connection lost: {error}", statement.name);
        self.broken = Some(text.clone());
        SqlError::Other(text)
    }
}

impl Sql for PostgresStore {
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
        if let Some(broken) = &self.broken {
            return Err(SqlError::Other(format!("{}: {broken}", statement.name)));
        }
        if let Err(error) = self.send(statement, values) {
            return Err(self.lost(statement, error));
        }
        match self.receive() {
            Ok(Ok(rows)) => Ok(rows),
            Ok(Err(fields)) if fields.fenced() => Err(SqlError::Fenced),
            Ok(Err(fields)) => Err(SqlError::Other(format!(
                "{}: {}",
                statement.name,
                fields.describe()
            ))),
            Err(error) => Err(self.lost(statement, error)),
        }
    }

    fn rollback(&mut self) {
        if self.broken.is_some() || self.transaction == TRANSACTION_IDLE {
            return;
        }
        let mut out = Vec::new();
        let mut body = Vec::new();
        put_cstr(&mut body, "ROLLBACK");
        frame(&mut out, b'Q', &body);
        let sent = self
            .writer
            .write_all(&out)
            .map_err(|error| format!("write: {error}"));
        let received = sent.and_then(|()| self.receive());
        if let Err(error) = received {
            self.broken = Some(format!("rollback: connection lost: {error}"));
        }
    }
}
