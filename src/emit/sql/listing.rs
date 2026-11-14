use crate::emit::sql::{self, Catalogue, Statement};
use crate::ir::Program;
use crate::run::json::Json;
use crate::target::Target;

fn statement_json(statement: &Statement) -> Json {
    let params = statement
        .params
        .iter()
        .map(|param| {
            Json::object([
                ("name", Json::text(param.name)),
                ("type", Json::text(param.ty.name())),
            ])
        })
        .collect();
    Json::object([
        ("name", Json::text(statement.name)),
        ("params", Json::Array(params)),
        (
            "returns",
            Json::Array(
                statement
                    .returns
                    .iter()
                    .map(|name| Json::text(name))
                    .collect(),
            ),
        ),
        ("sql", Json::String(statement.sql())),
    ])
}

fn list(statements: &[Statement]) -> Json {
    Json::Array(statements.iter().map(statement_json).collect())
}

pub fn listing(catalogue: &Catalogue, harness: &str) -> Json {
    let batch_fields = ["kind", "record", "seq", "turn"]
        .into_iter()
        .map(Json::text)
        .collect();
    Json::object([
        ("acquire", statement_json(&catalogue.acquire)),
        ("batch_fields", Json::Array(batch_fields)),
        ("commit", list(&catalogue.commit)),
        ("connect", list(&catalogue.connect)),
        ("dialect", Json::text(catalogue.dialect.name())),
        (
            "duplicate_error",
            Json::text(catalogue.dialect.duplicate_error()),
        ),
        ("fenced_error", Json::text(catalogue.dialect.fenced_error())),
        ("harness", Json::text(harness)),
        ("load", statement_json(&catalogue.load)),
        ("mutation", Json::text(catalogue.mutation.name())),
        ("schema", list(&catalogue.schema)),
        ("target", Json::text(catalogue.target.name())),
    ])
}

pub fn emit(program: &Program, target: Target, mutation: sql::Mutation) -> Result<String, String> {
    let catalogue = sql::catalogue(target, mutation)?;
    let mut text = listing(&catalogue, &program.name).encode();
    text.push('\n');
    Ok(text)
}
