use crate::ir::{Code, Program};
use crate::prove::{Finding, Report};
use crate::run::cell_journal::{ENTRY_BYTES, JOURNAL_LIMIT_BYTES};

pub const ENTRIES_PER_NESTED_CALL: u64 = 2;
pub const ENTRIES_PER_TIMER: u64 = 2;

pub fn prove(program: &Program, report: &mut Report) {
    for code in &program.codes {
        let findings = obligations(code);
        if findings.is_empty() {
            report.proved.push(proved(code));
        }
        for (message, counterexample) in findings {
            report.add(Finding {
                handler: format!("(code {})", code.name),
                target: None,
                property: "cell_bounds",
                message,
                path: format!("code {}", code.name),
                counterexample,
            });
        }
    }
}

fn entries_floor_bytes(code: &Code) -> Option<u64> {
    let calls = code.calls.value.checked_mul(ENTRIES_PER_NESTED_CALL)?;
    let timers = code.timers.value.checked_mul(ENTRIES_PER_TIMER)?;
    calls.checked_add(timers)?.checked_mul(ENTRY_BYTES)
}

fn obligations(code: &Code) -> Vec<(String, Option<String>)> {
    assert!(
        code.parallel.value <= code.calls.value,
        "the checker refuses a code tool whose parallel exceeds its calls"
    );
    let mut findings = Vec::new();
    if code.journal_bytes.value > JOURNAL_LIMIT_BYTES {
        findings.push((
            format!(
                "journal_bytes ({} = {}) exceeds nanocode's JOURNAL_LIMIT_BYTES ({JOURNAL_LIMIT_BYTES})",
                code.journal_bytes.name, code.journal_bytes.value
            ),
            Some("a journal nanocode's engine refuses to replay is accepted as replayable".to_owned()),
        ));
    }
    match entries_floor_bytes(code) {
        Some(floor) if floor <= code.journal_bytes.value => {}
        floor => findings.push((
            format!(
                "journal_bytes ({} = {}) cannot hold the entries of {} nested calls and {} timers ({} bytes at {ENTRY_BYTES} bytes per entry at least)",
                code.journal_bytes.name,
                code.journal_bytes.value,
                code.calls.value,
                code.timers.value,
                floor.map_or_else(|| "more than u64".to_owned(), |bytes| bytes.to_string())
            ),
            Some(format!(
                "every cell that uses its {} calls stops unknown at the journal limit first",
                code.calls.name
            )),
        )),
    }
    findings
}

fn proved(code: &Code) -> String {
    let floor = entries_floor_bytes(code).expect("an accepted code tool's entry floor fits u64");
    format!(
        "code {}: cell_bounds: parallel {} <= calls {}; journal_bytes {} <= JOURNAL_LIMIT_BYTES {JOURNAL_LIMIT_BYTES} and >= {floor} bytes of entries for {} calls and {} timers; timer_ms {} is not checked against a sleep limit, since no target model gives one",
        code.name,
        code.parallel.value,
        code.calls.value,
        code.journal_bytes.value,
        code.calls.value,
        code.timers.value,
        code.timer_ms.value
    )
}
