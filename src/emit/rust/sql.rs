use std::fmt::Write;

use crate::emit::sql::{self, ParamType, Statement};
use crate::target::Target;

const RUNTIME_SQL: &str = include_str!("runtime/sql.rs");

fn statement(out: &mut String, statement: &Statement) {
    let params: Vec<String> = statement
        .params
        .iter()
        .map(|param| {
            let ty = match param.ty {
                ParamType::Text => "Text",
                ParamType::Integer => "Integer",
                ParamType::Json => "Json",
            };
            format!("Param {{ name: {:?}, ty: ParamType::{ty} }}", param.name)
        })
        .collect();
    write!(
        out,
        "Statement {{ name: {:?}, sql: {:?}, params: &[{}], returns: &{:?} }}",
        statement.name,
        statement.sql(),
        params.join(", "),
        statement.returns
    )
    .expect("writing to a string succeeds");
}

fn list(out: &mut String, name: &str, statements: &[Statement]) {
    write!(out, "pub const {name}: &[Statement] = &[").expect("writing to a string succeeds");
    for (index, item) in statements.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        statement(out, item);
    }
    out.push_str("];\n");
}

pub fn store(target: Target) -> Result<String, String> {
    let catalogue = sql::catalogue(target, sql::Mutation::None)?;
    let mut out = String::from(RUNTIME_SQL);
    writeln!(
        out,
        "\npub const DIALECT: &str = {:?};",
        catalogue.dialect.name()
    )
    .expect("writing to a string succeeds");
    writeln!(
        out,
        "pub const FENCED_ERROR: &str = {:?};",
        catalogue.dialect.fenced_error()
    )
    .expect("writing to a string succeeds");
    list(&mut out, "CONNECT", &catalogue.connect);
    list(&mut out, "SCHEMA", &catalogue.schema);
    out.push_str("pub const ACQUIRE: Statement = ");
    statement(&mut out, &catalogue.acquire);
    out.push_str(";\npub const LOAD: Statement = ");
    statement(&mut out, &catalogue.load);
    out.push_str(";\n");
    list(&mut out, "COMMIT", &catalogue.commit);
    Ok(out)
}
