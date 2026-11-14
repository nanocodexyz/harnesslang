use crate::harness::{self, Disk, Record, Stop};
use crate::{Driver, fail, text};

#[derive(Default)]
pub struct State {
    contents: Vec<u8>,
    answer: Option<String>,
    opening: bool,
}

pub fn open(driver: &mut Driver, epoch: u64, bytes: &[u8]) -> (Vec<Record>, u64) {
    driver.store.contents = bytes.to_vec();
    driver.store.opening = true;
    let opened = harness::jsonl_open(driver).unwrap_or_else(|error| fail(&error.to_string()));
    driver.store.opening = false;
    (opened.records, epoch.min(0))
}

pub fn flush(driver: &mut Driver, _epoch: u64, records: &[Record]) -> Result<(), Stop> {
    harness::jsonl_flush(driver, records)
}

impl Disk for Driver {
    fn contents(&mut self) -> std::io::Result<Vec<u8>> {
        assert!(self.store.opening, "the store reads the file only at open");
        Ok(self.store.contents.clone())
    }

    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        assert!(!self.store.opening, "the store appends only when it flushes");
        if self.store.answer.is_some() {
            fail("the generated store wrote twice in one flush");
        }
        let event = self.events.next("flush");
        if text(&event, "bytes").as_bytes() != bytes {
            fail(&format!(
                "event {}: the generated store wrote {:?}, the interpreter {:?}",
                self.events.index,
                String::from_utf8_lossy(bytes),
                text(&event, "bytes")
            ));
        }
        self.stats.writes += 1;
        self.store.answer = Some(text(&event, "answer").to_owned());
        Ok(())
    }

    fn sync(&mut self) -> std::io::Result<()> {
        if self.store.opening {
            return Ok(());
        }
        self.stats.syncs += 1;
        match self.store.answer.take().as_deref() {
            Some("ok") => Ok(()),
            Some("crash") => Err(std::io::Error::other("the simulated disk crashed")),
            Some(other) => fail(&format!("a jsonl flush answered {other}")),
            None => fail("the generated store synced without writing"),
        }
    }

    fn truncate(&mut self, length: u64) -> std::io::Result<()> {
        assert!(self.store.opening, "the store truncates only at open");
        self.store.contents.truncate(length as usize);
        Ok(())
    }
}
