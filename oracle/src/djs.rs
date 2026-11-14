use std::cell::Cell;
use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nanocode_code_mode::ToolOutputContent;
use nanocode_code_mode_engine::{
    CellJournaling, CellTools, EmbeddedHost, Interrupt, JournalEntry, RuntimeEvent, SharedJournal,
};
use nanocode_platform::{Host, HostFuture};
use serde_json::{Value, json};

const WAKES_PER_EVENT_MAX: usize = 1 << 20;
const ROUNDS_PER_CELL_MAX: usize = 1 << 20;
const PERMITS_PER_TIMER_MAX: usize = 2;
const RANDOM_MULTIPLIER: u64 = 6_364_136_223_846_793_005;
const RANDOM_INCREMENT: u64 = 1_442_695_040_888_963_407;

thread_local! {
    static CLOCK: Cell<Duration> = const { Cell::new(Duration::ZERO) };
    static PERMIT: Cell<bool> = const { Cell::new(false) };
    static RANDOM: Cell<u64> = const { Cell::new(0) };
}

struct VirtualHost;

static VIRTUAL_HOST: VirtualHost = VirtualHost;

fn base() -> Instant {
    static BASE: OnceLock<Instant> = OnceLock::new();
    *BASE.get_or_init(Instant::now)
}

struct Sleep(Duration);

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<()> {
        if PERMIT.replace(false) {
            CLOCK.set(CLOCK.get() + self.0);
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl Host for VirtualHost {
    fn now(&self) -> Instant {
        base() + CLOCK.get()
    }

    fn system_time(&self) -> SystemTime {
        UNIX_EPOCH + CLOCK.get()
    }

    fn sleep(&self, duration: Duration) -> HostFuture {
        Box::pin(Sleep(duration))
    }

    fn fill_random(&self, bytes: &mut [u8]) -> std::io::Result<()> {
        for byte in bytes {
            let next = RANDOM
                .get()
                .wrapping_mul(RANDOM_MULTIPLIER)
                .wrapping_add(RANDOM_INCREMENT);
            RANDOM.set(next);
            *byte = next.to_be_bytes()[0];
        }
        Ok(())
    }

    fn spawn(&self, _future: HostFuture) {
        panic!("the oracle's cells spawn no tasks on the platform host");
    }
}

pub fn install() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        nanocode_platform::install(&VIRTUAL_HOST)
            .expect("the oracle installs the platform host once, before any cell runs");
    });
}

fn reset(seed: u64, wall_ms: u64) {
    CLOCK.set(Duration::from_millis(wall_ms));
    PERMIT.set(false);
    RANDOM.set(seed);
}

struct Flag(AtomicBool);

impl Wake for Flag {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn poll_event(host: &mut EmbeddedHost) -> Result<Option<RuntimeEvent>, String> {
    let flag = Arc::new(Flag(AtomicBool::new(false)));
    let waker = Waker::from(Arc::clone(&flag));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(host.read_event());
    for _ in 0..WAKES_PER_EVENT_MAX {
        flag.0.store(false, Ordering::SeqCst);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(event) => return event.map(Some),
            Poll::Pending if !flag.0.load(Ordering::SeqCst) => return Ok(None),
            Poll::Pending => {}
        }
    }
    Err(format!(
        "djs woke {WAKES_PER_EVENT_MAX} times without an event"
    ))
}

#[derive(Clone, Debug, PartialEq)]
pub enum Terminal {
    Done,
    Threw(String),
    Diverged(String),
    Waiting(usize),
}

#[derive(Debug)]
pub struct Run {
    pub entries: Vec<JournalEntry>,
    pub text: String,
    pub terminal: Terminal,
    pub replayed: bool,
}

pub enum Next {
    Deliver {
        id: u64,
        encoded: String,
        success: bool,
    },
    Fire,
    Stop,
}

pub struct View<'a> {
    pub entries: &'a [JournalEntry],
    pub open: &'a BTreeSet<u64>,
}

pub trait Inputs {
    fn next(&mut self, view: &View<'_>) -> Next;
}

pub struct Script {
    pub id: u64,
    pub script: String,
    pub tools: Vec<String>,
    pub seed: u64,
    pub wall_ms: u64,
}

fn cell_tools(names: &[String]) -> Result<Arc<CellTools>, String> {
    let definitions: Vec<Value> = names
        .iter()
        .map(|name| json!({"name": name, "tool_name": name, "kind": "function"}))
        .collect();
    CellTools::encode(&definitions).map(Arc::new)
}

struct Driver {
    host: EmbeddedHost,
    journal: SharedJournal,
    issued: BTreeSet<u64>,
    text: String,
    replayed: bool,
}

fn open_calls(entries: &[JournalEntry]) -> BTreeSet<u64> {
    let mut open = BTreeSet::new();
    for entry in entries {
        match entry {
            JournalEntry::Call { id, .. } => {
                assert!(open.insert(*id), "djs journals each nested call id once");
            }
            JournalEntry::Result { id, .. } => {
                assert!(
                    open.remove(id),
                    "djs journals a result only for an open call"
                );
            }
            _ => {}
        }
    }
    open
}

impl Driver {
    fn event(&mut self, event: RuntimeEvent) -> Option<Terminal> {
        match event {
            RuntimeEvent::ToolCall { id, .. } => {
                assert!(
                    self.issued.insert(id),
                    "djs issues each nested call id once"
                );
            }
            RuntimeEvent::Content {
                content: ToolOutputContent::InputText { text },
                ..
            } => {
                if !self.text.is_empty() {
                    self.text.push('\n');
                }
                self.text.push_str(&text);
            }
            RuntimeEvent::Replayed { .. } => self.replayed = true,
            RuntimeEvent::Done { .. } => return Some(Terminal::Done),
            RuntimeEvent::Error { message, .. } => return Some(Terminal::Threw(message)),
            RuntimeEvent::Diverged { reason, .. } => return Some(Terminal::Diverged(reason)),
            _ => {}
        }
        None
    }

    fn entries(&self) -> Vec<JournalEntry> {
        self.journal
            .entries()
            .expect("an oracle cell stays under JOURNAL_LIMIT_BYTES")
    }
}

pub fn run(
    cell: &Script,
    replay: Option<Vec<JournalEntry>>,
    inputs: &mut dyn Inputs,
) -> Result<Run, String> {
    install();
    reset(cell.seed, cell.wall_ms);
    let journal = SharedJournal::new();
    let mut driver = Driver {
        host: EmbeddedHost::spawn()?,
        journal: journal.clone(),
        issued: BTreeSet::new(),
        text: String::new(),
        replayed: false,
    };
    driver.host.start_cell(
        cell.id,
        &cell.script,
        HashMap::new(),
        cell_tools(&cell.tools)?,
        None,
        Some(CellJournaling { journal, replay }),
        Interrupt::new(),
    )?;
    let terminal = drive(&mut driver, cell.id, inputs)?;
    Ok(Run {
        entries: driver.entries(),
        text: driver.text,
        terminal,
        replayed: driver.replayed,
    })
}

fn drive(driver: &mut Driver, cell_id: u64, inputs: &mut dyn Inputs) -> Result<Terminal, String> {
    let mut permits = 0;
    for _ in 0..ROUNDS_PER_CELL_MAX {
        if let Some(event) = poll_event(&mut driver.host)? {
            permits = 0;
            if let Some(terminal) = driver.event(event) {
                return Ok(terminal);
            }
            continue;
        }
        PERMIT.set(false);
        let entries = driver.entries();
        let open = open_calls(&entries);
        let view = View {
            entries: &entries,
            open: &open,
        };
        match inputs.next(&view) {
            Next::Deliver {
                id,
                encoded,
                success,
            } => {
                assert!(open.contains(&id), "inputs deliver only open calls");
                permits = 0;
                driver
                    .host
                    .send_tool_result(cell_id, id, encoded, success)?;
            }
            Next::Fire if permits < PERMITS_PER_TIMER_MAX => {
                permits += 1;
                PERMIT.set(true);
            }
            Next::Fire | Next::Stop => return Ok(Terminal::Waiting(entries.len())),
        }
    }
    Err(format!("a cell ran {ROUNDS_PER_CELL_MAX} rounds"))
}
