pub mod compile;
pub mod decode;
pub mod exec;
pub mod fuse;
pub mod inline;
pub mod js;
pub mod native;
pub mod sim;
pub mod specialize;

pub use decode::Module;
pub use exec::{Instance, Memory};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValType {
    I32,
    I64,
}

impl ValType {
    pub const fn name(self) -> &'static str {
        match self {
            Self::I32 => "i32",
            Self::I64 => "i64",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value {
    I32(i32),
    I64(i64),
}

impl Value {
    pub const fn ty(self) -> ValType {
        match self {
            Self::I32(_) => ValType::I32,
            Self::I64(_) => ValType::I64,
        }
    }

    pub(crate) const fn slot(self) -> u64 {
        match self {
            Self::I32(value) => value as u32 as u64,
            Self::I64(value) => value as u64,
        }
    }

    pub(crate) const fn from_slot(ty: ValType, slot: u64) -> Self {
        match ty {
            ValType::I32 => Self::I32(slot as u32 as i32),
            ValType::I64 => Self::I64(slot as i64),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trap {
    Unreachable,
    MemoryOutOfBounds,
    DivideByZero,
    IntegerOverflow,
    CallStackExhausted,
    ValueStackExhausted,
}

impl std::fmt::Display for Trap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Unreachable => "unreachable executed",
            Self::MemoryOutOfBounds => "memory access out of bounds",
            Self::DivideByZero => "integer divide by zero",
            Self::IntegerOverflow => "integer overflow",
            Self::CallStackExhausted => {
                "call stack exhausted: calls nested deeper than call_depth_max"
            }
            Self::ValueStackExhausted => {
                "value stack exhausted: frames need more than value_slots_max slots"
            }
        })
    }
}

#[derive(Debug)]
pub enum Error<S> {
    Trap(Trap),
    Stop(S),
}

pub trait Host: Sized {
    type Stop;

    fn call(
        &mut self,
        import: usize,
        args: &[Value],
        instance: &mut Instance<'_>,
    ) -> Result<Option<Value>, Error<Self::Stop>>;
}
