use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::exec::{Instance, Limits};
use super::js::{Json, parse};
use super::sim::{Import, Loaded};
use super::{Error, Host, Value};

pub const EPOCH: i64 = 0;
const WORD_BITS: u32 = 32;
const LOW_WORD: u64 = u32::MAX as u64;
const EFFECTS: [&str; 3] = ["none", "idempotent", "external"];

pub trait World {
    fn tool(&mut self, request: &Json) -> Result<Json, String>;
    fn model(&mut self, request: &Json) -> Result<Json, String>;
    fn now(&mut self) -> Result<u64, String>;
    fn random(&mut self) -> Result<u64, String>;
    fn world(&mut self, request: &Json) -> Result<Json, String>;
    fn acknowledge(&mut self, finish: &Json) -> Result<(), String>;
}

pub struct Journal {
    path: PathBuf,
    file: File,
}

impl Journal {
    pub fn open(path: &Path) -> Result<Journal, String> {
        let existed = path.exists();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
        if !existed {
            file.sync_all()
                .map_err(|error| format!("cannot sync {}: {error}", path.display()))?;
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| format!("cannot sync {}: {error}", parent.display()))?;
        }
        Ok(Journal {
            path: path.to_path_buf(),
            file,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn read(&mut self) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        self.file
            .seek(SeekFrom::Start(0))
            .and_then(|_| self.file.read_to_end(&mut bytes))
            .map_err(|error| format!("cannot read {}: {error}", self.path.display()))?;
        Ok(bytes)
    }

    fn keep(&mut self, length: u64) -> Result<(), String> {
        let size = self
            .file
            .metadata()
            .map_err(|error| format!("cannot stat {}: {error}", self.path.display()))?
            .len();
        assert!(length <= size, "a journal keeps at most the bytes it read");
        if length == size {
            return Ok(());
        }
        self.file
            .set_len(length)
            .and_then(|()| self.file.sync_data())
            .map_err(|error| format!("cannot truncate {}: {error}", self.path.display()))
    }

    pub fn append(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.file
            .seek(SeekFrom::End(0))
            .and_then(|_| self.file.write_all(bytes))
            .and_then(|()| self.file.sync_data())
            .map_err(|error| format!("cannot append to {}: {error}", self.path.display()))
    }
}

struct Run<'w, 'j, 'm, W: World> {
    world: &'w mut W,
    journal: &'j mut Journal,
    loaded: &'m Loaded,
}

fn i32_arg(args: &[Value], index: usize) -> i32 {
    match args[index] {
        Value::I32(value) => value,
        Value::I64(_) => panic!("import argument {index} is an i32"),
    }
}

fn i64_arg(args: &[Value], index: usize) -> i64 {
    match args[index] {
        Value::I64(value) => value,
        Value::I32(_) => panic!("import argument {index} is an i64"),
    }
}

fn words(packed: u64) -> [Value; 2] {
    [
        Value::I32((packed >> WORD_BITS) as u32 as i32),
        Value::I32((packed & LOW_WORD) as u32 as i32),
    ]
}

impl<W: World> Run<'_, '_, '_, W> {
    fn text(&self, instance: &Instance, at: i32, length: i32) -> Result<String, String> {
        let bytes = instance
            .memory()
            .read(at as u32, length as u32)
            .map_err(|_| format!("the module named {length} bytes outside its memory"))?;
        String::from_utf8(bytes.to_vec())
            .map_err(|_| "the module wrote text that is not UTF-8".to_owned())
    }

    fn json(&self, instance: &Instance, args: &[Value]) -> Result<Json, String> {
        parse(&self.text(instance, i32_arg(args, 0), i32_arg(args, 1))?)
    }

    fn give(&mut self, instance: &mut Instance, bytes: &[u8]) -> Result<u64, Error<String>> {
        let length = u32::try_from(bytes.len())
            .map_err(|_| Error::Stop("a value for the module is over 4 GiB".to_owned()))?;
        let Some(Value::I32(at)) =
            instance.invoke(self, self.loaded.alloc, &[Value::I32(length as i32)])?
        else {
            panic!("hl_alloc returns an i32");
        };
        instance.memory_mut().write(at as u32, bytes).map_err(|_| {
            Error::Stop("hl_alloc gave memory outside the module's memory".to_owned())
        })?;
        Ok((u64::from(at as u32) << WORD_BITS) | u64::from(length))
    }

    fn give_json(
        &mut self,
        instance: &mut Instance,
        value: &Json,
    ) -> Result<Option<Value>, Error<String>> {
        let text = value.encode();
        self.give(instance, text.as_bytes())
            .map(|packed| Some(Value::I64(packed as i64)))
    }

    fn batch(&mut self, request: &Json) -> Result<Json, String> {
        let Json::Array(requests) = request.get("requests") else {
            return Err("a batch request lists its requests".to_owned());
        };
        let mut completed = Vec::with_capacity(requests.len());
        for (index, each) in requests.iter().enumerate() {
            let output = self.world.tool(each)?;
            completed.push(Json::Array(vec![Json::int(index), output]));
        }
        Ok(Json::Object(vec![(
            "completed".to_owned(),
            Json::Array(completed),
        )]))
    }

    fn world_request(
        &mut self,
        instance: &mut Instance,
        args: &[Value],
    ) -> Result<Option<Value>, Error<String>> {
        let request = self.json(instance, args).map_err(Error::Stop)?;
        let answer = if request.get("event").is_text("batch") {
            self.batch(&request)
        } else {
            self.world.world(&request)
        };
        let answer = answer.map_err(Error::Stop)?;
        self.give_json(instance, &answer)
    }
}

impl<W: World> Host for Run<'_, '_, '_, W> {
    type Stop = String;

    fn call(
        &mut self,
        import: usize,
        args: &[Value],
        instance: &mut Instance<'_>,
    ) -> Result<Option<Value>, Error<String>> {
        let stop = Error::Stop;
        match self.loaded.imports[import] {
            Import::Fail => {
                let text = self
                    .text(instance, i32_arg(args, 0), i32_arg(args, 1))
                    .map_err(stop)?;
                Err(Error::Stop(format!("the module failed: {text}")))
            }
            Import::Hook => {
                let effect = i32_arg(args, 2);
                assert!(
                    effect >= 0 && (effect as usize) < EFFECTS.len(),
                    "a hook names an effect class"
                );
                Ok(None)
            }
            Import::Flush => {
                if i64_arg(args, 0) != EPOCH {
                    return Err(Error::Stop(
                        "a native journal has one owner, epoch 0".to_owned(),
                    ));
                }
                let text = self
                    .text(instance, i32_arg(args, 1), i32_arg(args, 2))
                    .map_err(stop)?;
                self.journal.append(text.as_bytes()).map_err(stop)?;
                Ok(None)
            }
            Import::Keep => {
                let length = u64::try_from(i64_arg(args, 0))
                    .map_err(|_| Error::Stop("the module kept a negative length".to_owned()))?;
                self.journal.keep(length).map_err(stop)?;
                Ok(None)
            }
            Import::Tool => {
                let request = self.json(instance, args).map_err(stop)?;
                let output = self.world.tool(&request).map_err(stop)?;
                self.give_json(instance, &output)
            }
            Import::Model => {
                let request = self.json(instance, args).map_err(stop)?;
                let response = self.world.model(&request).map_err(stop)?;
                self.give_json(instance, &response)
            }
            Import::Now => Ok(Some(Value::I64(self.world.now().map_err(stop)? as i64))),
            Import::Random => Ok(Some(Value::I64(self.world.random().map_err(stop)? as i64))),
            Import::Attempt => Err(Error::Stop(
                "a native run recovers at open and counts no step attempts".to_owned(),
            )),
            Import::World => self.world_request(instance, args),
            Import::Acknowledge => {
                let finish = self.json(instance, args).map_err(stop)?;
                self.world.acknowledge(&finish).map_err(stop)?;
                Ok(None)
            }
        }
    }
}

pub fn run<W: World>(
    loaded: &Loaded,
    journal: &mut Journal,
    request: &Json,
    world: &mut W,
) -> Result<Json, String> {
    let opened = journal.read()?;
    let mut instance = Instance::new(&loaded.module, Limits::NODE);
    let mut host = Run {
        world,
        journal,
        loaded,
    };
    let request_text = request.encode();
    let exit = (|| {
        let request_at = host.give(&mut instance, request_text.as_bytes())?;
        let journal_at = host.give(&mut instance, &opened)?;
        let [request_pointer, request_length] = words(request_at);
        let [journal_pointer, journal_length] = words(journal_at);
        let args = [
            request_pointer,
            request_length,
            journal_pointer,
            journal_length,
            Value::I64(EPOCH),
        ];
        let Some(Value::I64(packed)) = instance.invoke(&mut host, loaded.run, &args)? else {
            panic!("hl_run returns an i64");
        };
        let [Value::I32(at), Value::I32(length)] = words(packed as u64) else {
            unreachable!("words are i32 values");
        };
        let text = host.text(&instance, at, length).map_err(Error::Stop)?;
        parse(&text).map_err(Error::Stop)
    })();
    match exit {
        Ok(exit) => Ok(exit),
        Err(Error::Stop(message)) => Err(message),
        Err(Error::Trap(trap)) => Err(format!("the module trapped: {trap}")),
    }
}

const EXPORT_IDLE: &str = "hl_idle";

pub fn idle<W: World>(
    loaded: &Loaded,
    journal: &mut Journal,
    world: &mut W,
) -> Result<Option<u64>, String> {
    let opened = journal.read()?;
    let function = loaded
        .module
        .export_function(EXPORT_IDLE)
        .ok_or_else(|| format!("the module exports no {EXPORT_IDLE}"))?;
    let mut instance = Instance::new(&loaded.module, Limits::NODE);
    let mut host = Run {
        world,
        journal,
        loaded,
    };
    let answer = (|| {
        let journal_at = host.give(&mut instance, &opened)?;
        let [pointer, length] = words(journal_at);
        let Some(Value::I64(packed)) =
            instance.invoke(&mut host, function, &[pointer, length, Value::I64(EPOCH)])?
        else {
            panic!("hl_idle returns an i64");
        };
        let [Value::I32(at), Value::I32(length)] = words(packed as u64) else {
            unreachable!("words are i32 values");
        };
        let text = host.text(&instance, at, length).map_err(Error::Stop)?;
        parse(&text).map_err(Error::Stop)
    })();
    let answer = match answer {
        Ok(answer) => answer,
        Err(Error::Stop(message)) => return Err(message),
        Err(Error::Trap(trap)) => return Err(format!("the module trapped: {trap}")),
    };
    match answer.get("due") {
        Json::Null => Ok(None),
        Json::Int(digits) => digits
            .parse::<u64>()
            .map(Some)
            .map_err(|_| format!("an idle due of {digits} is not a u64")),
        _ if answer.get("exit").is_text("corrupt") => Err(format!(
            "the journal is corrupt: {}",
            answer.get("error").encode()
        )),
        other => Err(format!("hl_idle answered {}", other.encode())),
    }
}
