use super::compile::{Binary, Load, Op, Store, Unary};

#[derive(Clone, Copy, Debug)]
pub enum Inst {
    Scan {
        scan: u32,
    },
    Crc {
        crc: u32,
    },
    Unreachable,
    Jump(u32),
    JumpIf {
        condition: u32,
        target: u32,
    },
    JumpUnless {
        condition: u32,
        target: u32,
    },
    JumpEq {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpEqImmediate {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpNe {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpNeImmediate {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpLtS {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpLtSImmediate {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpLtU {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpLtUImmediate {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpGtS {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpGtSImmediate {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpGtU {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpGtUImmediate {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpLeS {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpLeSImmediate {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpLeU {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpLeUImmediate {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpGeS {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpGeSImmediate {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpGeU {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpGeUImmediate {
        left: u32,
        right: u32,
        target: u32,
    },
    JumpTable {
        index: u32,
        start: u32,
        labels: u32,
    },
    Return {
        source: u32,
    },
    ReturnVoid,
    Call {
        function: u32,
        base: u32,
    },
    CallImport {
        import: u32,
        base: u32,
    },
    Move {
        result: u32,
        source: u32,
    },
    Const {
        result: u32,
        value: u64,
    },
    Select {
        result: u32,
        first: u32,
        second: u32,
        condition: u32,
    },
    GlobalGet {
        result: u32,
        global: u32,
    },
    GlobalSet {
        global: u32,
        source: u32,
    },
    LoadI32 {
        result: u32,
        address: u32,
        offset: u32,
    },
    LoadI64 {
        result: u32,
        address: u32,
        offset: u32,
    },
    LoadU8 {
        result: u32,
        address: u32,
        offset: u32,
    },
    Load {
        load: Load,
        result: u32,
        address: u32,
        offset: u32,
    },
    LoadPlusI32 {
        result: u32,
        address: u32,
        plus: u32,
    },
    LoadPlusU8 {
        result: u32,
        address: u32,
        plus: u32,
    },
    LoadPlus {
        load: Load,
        result: u32,
        address: u32,
        plus: u32,
    },
    LoadIndexedI32 {
        result: u32,
        base: u32,
        index: u32,
    },
    LoadIndexedU8 {
        result: u32,
        base: u32,
        index: u32,
    },
    LoadIndexed {
        load: Load,
        result: u32,
        base: u32,
        index: u32,
    },
    StoreI32 {
        address: u32,
        value: u32,
        offset: u32,
    },
    StoreI64 {
        address: u32,
        value: u32,
        offset: u32,
    },
    StoreU8 {
        address: u32,
        value: u32,
        offset: u32,
    },
    Store {
        store: Store,
        address: u32,
        value: u32,
        offset: u32,
    },
    StoreIndexed {
        store: Store,
        base: u32,
        index: u32,
        value: u32,
    },
    MemorySize {
        result: u32,
    },
    MemoryGrow {
        result: u32,
        delta: u32,
    },
    MemoryCopy {
        destination: u32,
        source: u32,
        length: u32,
    },
    MemoryFill {
        destination: u32,
        value: u32,
        length: u32,
    },
    I32Eqz {
        result: u32,
        source: u32,
    },
    I32Unary {
        unary: Unary,
        result: u32,
        source: u32,
    },
    I64Unary {
        unary: Unary,
        result: u32,
        source: u32,
    },
    I32Add {
        result: u32,
        left: u32,
        right: u32,
    },
    I32AddImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32Sub {
        result: u32,
        left: u32,
        right: u32,
    },
    I32SubImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32Mul {
        result: u32,
        left: u32,
        right: u32,
    },
    I32MulImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32And {
        result: u32,
        left: u32,
        right: u32,
    },
    I32AndImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32Or {
        result: u32,
        left: u32,
        right: u32,
    },
    I32OrImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32Xor {
        result: u32,
        left: u32,
        right: u32,
    },
    I32XorImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32Shl {
        result: u32,
        left: u32,
        right: u32,
    },
    I32ShlImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32ShrS {
        result: u32,
        left: u32,
        right: u32,
    },
    I32ShrSImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32ShrU {
        result: u32,
        left: u32,
        right: u32,
    },
    I32ShrUImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32Eq {
        result: u32,
        left: u32,
        right: u32,
    },
    I32EqImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32Ne {
        result: u32,
        left: u32,
        right: u32,
    },
    I32NeImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32LtS {
        result: u32,
        left: u32,
        right: u32,
    },
    I32LtSImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32LtU {
        result: u32,
        left: u32,
        right: u32,
    },
    I32LtUImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32GtS {
        result: u32,
        left: u32,
        right: u32,
    },
    I32GtSImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32GtU {
        result: u32,
        left: u32,
        right: u32,
    },
    I32GtUImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32LeS {
        result: u32,
        left: u32,
        right: u32,
    },
    I32LeSImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32LeU {
        result: u32,
        left: u32,
        right: u32,
    },
    I32LeUImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32GeS {
        result: u32,
        left: u32,
        right: u32,
    },
    I32GeSImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32GeU {
        result: u32,
        left: u32,
        right: u32,
    },
    I32GeUImmediate {
        result: u32,
        left: u32,
        right: u32,
    },
    I32Binary {
        binary: Binary,
        result: u32,
        left: u32,
        right: u32,
    },
    I32BinaryImmediate {
        binary: Binary,
        result: u32,
        left: u32,
        right: u32,
    },
    I64Add {
        result: u32,
        left: u32,
        right: u32,
    },
    I64AddConstant {
        result: u32,
        left: u32,
        right: u64,
    },
    I64Sub {
        result: u32,
        left: u32,
        right: u32,
    },
    I64SubConstant {
        result: u32,
        left: u32,
        right: u64,
    },
    I64Mul {
        result: u32,
        left: u32,
        right: u32,
    },
    I64MulConstant {
        result: u32,
        left: u32,
        right: u64,
    },
    I64And {
        result: u32,
        left: u32,
        right: u32,
    },
    I64AndConstant {
        result: u32,
        left: u32,
        right: u64,
    },
    I64Or {
        result: u32,
        left: u32,
        right: u32,
    },
    I64OrConstant {
        result: u32,
        left: u32,
        right: u64,
    },
    I64Xor {
        result: u32,
        left: u32,
        right: u32,
    },
    I64XorConstant {
        result: u32,
        left: u32,
        right: u64,
    },
    I64Shl {
        result: u32,
        left: u32,
        right: u32,
    },
    I64ShlConstant {
        result: u32,
        left: u32,
        right: u64,
    },
    I64ShrU {
        result: u32,
        left: u32,
        right: u32,
    },
    I64ShrUConstant {
        result: u32,
        left: u32,
        right: u64,
    },
    I64Eq {
        result: u32,
        left: u32,
        right: u32,
    },
    I64EqConstant {
        result: u32,
        left: u32,
        right: u64,
    },
    I64Ne {
        result: u32,
        left: u32,
        right: u32,
    },
    I64NeConstant {
        result: u32,
        left: u32,
        right: u64,
    },
    I64Binary {
        binary: Binary,
        result: u32,
        left: u32,
        right: u32,
    },
    I64BinaryConstant {
        binary: Binary,
        result: u32,
        left: u32,
        right: u64,
    },
    Wrap {
        result: u32,
        source: u32,
    },
    ExtendS {
        result: u32,
        source: u32,
    },
}

pub fn specialize(ops: &[Op], constants: &[u64]) -> Vec<Inst> {
    ops.iter().map(|op| inst(*op, constants)).collect()
}

fn inst(op: Op, constants: &[u64]) -> Inst {
    match op {
        Op::Unreachable => Inst::Unreachable,
        Op::Jump(target) => Inst::Jump(target),
        Op::JumpIf { condition, target } => Inst::JumpIf { condition, target },
        Op::JumpUnless { condition, target } => Inst::JumpUnless { condition, target },
        Op::JumpCompare {
            compare,
            left,
            right,
            target,
        } => match compare {
            Binary::Eq => Inst::JumpEq {
                left,
                right,
                target,
            },
            Binary::Ne => Inst::JumpNe {
                left,
                right,
                target,
            },
            Binary::LtS => Inst::JumpLtS {
                left,
                right,
                target,
            },
            Binary::LtU => Inst::JumpLtU {
                left,
                right,
                target,
            },
            Binary::GtS => Inst::JumpGtS {
                left,
                right,
                target,
            },
            Binary::GtU => Inst::JumpGtU {
                left,
                right,
                target,
            },
            Binary::LeS => Inst::JumpLeS {
                left,
                right,
                target,
            },
            Binary::LeU => Inst::JumpLeU {
                left,
                right,
                target,
            },
            Binary::GeS => Inst::JumpGeS {
                left,
                right,
                target,
            },
            Binary::GeU => Inst::JumpGeU {
                left,
                right,
                target,
            },
            other => unreachable!("a fused jump compares, not {other:?}"),
        },
        Op::JumpCompareImmediate {
            compare,
            left,
            right,
            target,
        } => match compare {
            Binary::Eq => Inst::JumpEqImmediate {
                left,
                right,
                target,
            },
            Binary::Ne => Inst::JumpNeImmediate {
                left,
                right,
                target,
            },
            Binary::LtS => Inst::JumpLtSImmediate {
                left,
                right,
                target,
            },
            Binary::LtU => Inst::JumpLtUImmediate {
                left,
                right,
                target,
            },
            Binary::GtS => Inst::JumpGtSImmediate {
                left,
                right,
                target,
            },
            Binary::GtU => Inst::JumpGtUImmediate {
                left,
                right,
                target,
            },
            Binary::LeS => Inst::JumpLeSImmediate {
                left,
                right,
                target,
            },
            Binary::LeU => Inst::JumpLeUImmediate {
                left,
                right,
                target,
            },
            Binary::GeS => Inst::JumpGeSImmediate {
                left,
                right,
                target,
            },
            Binary::GeU => Inst::JumpGeUImmediate {
                left,
                right,
                target,
            },
            other => unreachable!("a fused jump compares, not {other:?}"),
        },
        Op::JumpTable {
            index,
            start,
            labels,
        } => Inst::JumpTable {
            index,
            start,
            labels,
        },
        Op::Return { source } => Inst::Return { source },
        Op::ReturnVoid => Inst::ReturnVoid,
        Op::Call { function, base } => Inst::Call { function, base },
        Op::CallImport { import, base } => Inst::CallImport { import, base },
        Op::Move { result, source } => Inst::Move { result, source },
        Op::Const { result, value } => Inst::Const { result, value },
        Op::Select {
            result,
            first,
            second,
            condition,
        } => Inst::Select {
            result,
            first,
            second,
            condition,
        },
        Op::GlobalGet { result, global } => Inst::GlobalGet { result, global },
        Op::GlobalSet { global, source } => Inst::GlobalSet { global, source },
        Op::Load {
            load,
            result,
            address,
            offset,
        } => match load {
            Load::I32 => Inst::LoadI32 {
                result,
                address,
                offset,
            },
            Load::I64 => Inst::LoadI64 {
                result,
                address,
                offset,
            },
            Load::I32U8 => Inst::LoadU8 {
                result,
                address,
                offset,
            },
            _ => Inst::Load {
                load,
                result,
                address,
                offset,
            },
        },
        Op::LoadPlus {
            load,
            result,
            address,
            plus,
        } => match load {
            Load::I32 => Inst::LoadPlusI32 {
                result,
                address,
                plus,
            },
            Load::I32U8 => Inst::LoadPlusU8 {
                result,
                address,
                plus,
            },
            _ => Inst::LoadPlus {
                load,
                result,
                address,
                plus,
            },
        },
        Op::LoadIndexed {
            load,
            result,
            base,
            index,
        } => match load {
            Load::I32 => Inst::LoadIndexedI32 {
                result,
                base,
                index,
            },
            Load::I32U8 => Inst::LoadIndexedU8 {
                result,
                base,
                index,
            },
            _ => Inst::LoadIndexed {
                load,
                result,
                base,
                index,
            },
        },
        Op::Store {
            store,
            address,
            value,
            offset,
        } => match store {
            Store::I32 => Inst::StoreI32 {
                address,
                value,
                offset,
            },
            Store::I64 => Inst::StoreI64 {
                address,
                value,
                offset,
            },
            Store::I32As8 => Inst::StoreU8 {
                address,
                value,
                offset,
            },
            _ => Inst::Store {
                store,
                address,
                value,
                offset,
            },
        },
        Op::StoreIndexed {
            store,
            base,
            index,
            value,
        } => Inst::StoreIndexed {
            store,
            base,
            index,
            value,
        },
        Op::MemorySize { result } => Inst::MemorySize { result },
        Op::MemoryGrow { result, delta } => Inst::MemoryGrow { result, delta },
        Op::MemoryCopy {
            destination,
            source,
            length,
        } => Inst::MemoryCopy {
            destination,
            source,
            length,
        },
        Op::MemoryFill {
            destination,
            value,
            length,
        } => Inst::MemoryFill {
            destination,
            value,
            length,
        },
        Op::I32Unary {
            unary: Unary::Eqz,
            result,
            source,
        } => Inst::I32Eqz { result, source },
        Op::I32Unary {
            unary,
            result,
            source,
        } => Inst::I32Unary {
            unary,
            result,
            source,
        },
        Op::I64Unary {
            unary,
            result,
            source,
        } => Inst::I64Unary {
            unary,
            result,
            source,
        },
        Op::I32Binary {
            binary,
            result,
            left,
            right,
        } => match binary {
            Binary::Add => Inst::I32Add {
                result,
                left,
                right,
            },
            Binary::Sub => Inst::I32Sub {
                result,
                left,
                right,
            },
            Binary::Mul => Inst::I32Mul {
                result,
                left,
                right,
            },
            Binary::And => Inst::I32And {
                result,
                left,
                right,
            },
            Binary::Or => Inst::I32Or {
                result,
                left,
                right,
            },
            Binary::Xor => Inst::I32Xor {
                result,
                left,
                right,
            },
            Binary::Shl => Inst::I32Shl {
                result,
                left,
                right,
            },
            Binary::ShrS => Inst::I32ShrS {
                result,
                left,
                right,
            },
            Binary::ShrU => Inst::I32ShrU {
                result,
                left,
                right,
            },
            Binary::Eq => Inst::I32Eq {
                result,
                left,
                right,
            },
            Binary::Ne => Inst::I32Ne {
                result,
                left,
                right,
            },
            Binary::LtS => Inst::I32LtS {
                result,
                left,
                right,
            },
            Binary::LtU => Inst::I32LtU {
                result,
                left,
                right,
            },
            Binary::GtS => Inst::I32GtS {
                result,
                left,
                right,
            },
            Binary::GtU => Inst::I32GtU {
                result,
                left,
                right,
            },
            Binary::LeS => Inst::I32LeS {
                result,
                left,
                right,
            },
            Binary::LeU => Inst::I32LeU {
                result,
                left,
                right,
            },
            Binary::GeS => Inst::I32GeS {
                result,
                left,
                right,
            },
            Binary::GeU => Inst::I32GeU {
                result,
                left,
                right,
            },
            _ => Inst::I32Binary {
                binary,
                result,
                left,
                right,
            },
        },
        Op::I32BinaryImmediate {
            binary,
            result,
            left,
            right,
        } => match binary {
            Binary::Add => Inst::I32AddImmediate {
                result,
                left,
                right,
            },
            Binary::Sub => Inst::I32SubImmediate {
                result,
                left,
                right,
            },
            Binary::Mul => Inst::I32MulImmediate {
                result,
                left,
                right,
            },
            Binary::And => Inst::I32AndImmediate {
                result,
                left,
                right,
            },
            Binary::Or => Inst::I32OrImmediate {
                result,
                left,
                right,
            },
            Binary::Xor => Inst::I32XorImmediate {
                result,
                left,
                right,
            },
            Binary::Shl => Inst::I32ShlImmediate {
                result,
                left,
                right,
            },
            Binary::ShrS => Inst::I32ShrSImmediate {
                result,
                left,
                right,
            },
            Binary::ShrU => Inst::I32ShrUImmediate {
                result,
                left,
                right,
            },
            Binary::Eq => Inst::I32EqImmediate {
                result,
                left,
                right,
            },
            Binary::Ne => Inst::I32NeImmediate {
                result,
                left,
                right,
            },
            Binary::LtS => Inst::I32LtSImmediate {
                result,
                left,
                right,
            },
            Binary::LtU => Inst::I32LtUImmediate {
                result,
                left,
                right,
            },
            Binary::GtS => Inst::I32GtSImmediate {
                result,
                left,
                right,
            },
            Binary::GtU => Inst::I32GtUImmediate {
                result,
                left,
                right,
            },
            Binary::LeS => Inst::I32LeSImmediate {
                result,
                left,
                right,
            },
            Binary::LeU => Inst::I32LeUImmediate {
                result,
                left,
                right,
            },
            Binary::GeS => Inst::I32GeSImmediate {
                result,
                left,
                right,
            },
            Binary::GeU => Inst::I32GeUImmediate {
                result,
                left,
                right,
            },
            _ => Inst::I32BinaryImmediate {
                binary,
                result,
                left,
                right,
            },
        },
        Op::I64Binary {
            binary,
            result,
            left,
            right,
        } => match binary {
            Binary::Add => Inst::I64Add {
                result,
                left,
                right,
            },
            Binary::Sub => Inst::I64Sub {
                result,
                left,
                right,
            },
            Binary::Mul => Inst::I64Mul {
                result,
                left,
                right,
            },
            Binary::And => Inst::I64And {
                result,
                left,
                right,
            },
            Binary::Or => Inst::I64Or {
                result,
                left,
                right,
            },
            Binary::Xor => Inst::I64Xor {
                result,
                left,
                right,
            },
            Binary::Shl => Inst::I64Shl {
                result,
                left,
                right,
            },
            Binary::ShrU => Inst::I64ShrU {
                result,
                left,
                right,
            },
            Binary::Eq => Inst::I64Eq {
                result,
                left,
                right,
            },
            Binary::Ne => Inst::I64Ne {
                result,
                left,
                right,
            },
            _ => Inst::I64Binary {
                binary,
                result,
                left,
                right,
            },
        },
        Op::I64BinaryConstant {
            binary,
            result,
            left,
            constant,
        } => {
            let right = constants[constant as usize];
            match binary {
                Binary::Add => Inst::I64AddConstant {
                    result,
                    left,
                    right,
                },
                Binary::Sub => Inst::I64SubConstant {
                    result,
                    left,
                    right,
                },
                Binary::Mul => Inst::I64MulConstant {
                    result,
                    left,
                    right,
                },
                Binary::And => Inst::I64AndConstant {
                    result,
                    left,
                    right,
                },
                Binary::Or => Inst::I64OrConstant {
                    result,
                    left,
                    right,
                },
                Binary::Xor => Inst::I64XorConstant {
                    result,
                    left,
                    right,
                },
                Binary::Shl => Inst::I64ShlConstant {
                    result,
                    left,
                    right,
                },
                Binary::ShrU => Inst::I64ShrUConstant {
                    result,
                    left,
                    right,
                },
                Binary::Eq => Inst::I64EqConstant {
                    result,
                    left,
                    right,
                },
                Binary::Ne => Inst::I64NeConstant {
                    result,
                    left,
                    right,
                },
                _ => Inst::I64BinaryConstant {
                    binary,
                    result,
                    left,
                    right,
                },
            }
        }
        Op::Wrap { result, source } => Inst::Wrap { result, source },
        Op::ExtendS { result, source } => Inst::ExtendS { result, source },
    }
}

const SCAN_TABLE_OPS: u64 = 6;
const SCAN_EQUAL_OPS: u64 = 5;
const SCAN_TABLE_FOUND_OPS: u64 = 4;
const SCAN_EQUAL_FOUND_OPS: u64 = 3;
const CRC_OPS: u64 = 10;
const LOOP_EXIT_OPS: u64 = 1;
const CRC_BYTE_BITS: u32 = 8;
const CRC_BYTE_MASK: u32 = 0xff;
const CRC_WORD_SHIFT: u32 = 2;

#[derive(Clone, Copy, Debug)]
pub enum Test {
    Table { class: u32, table: u32 },
    Equal(u32),
}

#[derive(Clone, Copy, Debug)]
pub struct Scan {
    pub at: u32,
    pub end: u32,
    pub base: u32,
    pub byte: u32,
    pub test: Test,
    pub done: u32,
    pub found: u32,
}

impl Scan {
    pub const fn ops(self, iterations: u64, found: bool) -> u64 {
        let (full, partial) = match self.test {
            Test::Table { .. } => (SCAN_TABLE_OPS, SCAN_TABLE_FOUND_OPS),
            Test::Equal(_) => (SCAN_EQUAL_OPS, SCAN_EQUAL_FOUND_OPS),
        };
        iterations * full + if found { partial } else { LOOP_EXIT_OPS }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Crc {
    pub at: u32,
    pub end: u32,
    pub base: u32,
    pub crc: u32,
    pub shifted: u32,
    pub byte: u32,
    pub index: u32,
    pub word: u32,
    pub table: u32,
    pub done: u32,
}

impl Crc {
    pub const BYTE_BITS: u32 = CRC_BYTE_BITS;
    pub const BYTE_MASK: u32 = CRC_BYTE_MASK;
    pub const WORD_SHIFT: u32 = CRC_WORD_SHIFT;

    pub const fn ops(iterations: u64) -> u64 {
        iterations * CRC_OPS + LOOP_EXIT_OPS
    }
}

#[derive(Debug, Default)]
pub struct Loops {
    pub scans: Vec<Scan>,
    pub crcs: Vec<Crc>,
}

fn distinct(slots: &[u32]) -> bool {
    slots
        .iter()
        .enumerate()
        .all(|(index, slot)| !slots[index + 1..].contains(slot))
}

pub fn fuse_loops(insts: &mut [Inst]) -> Loops {
    let mut loops = Loops::default();
    for head in 0..insts.len() {
        if let Some(scan) = scan_at(insts, head) {
            insts[head] = Inst::Scan {
                scan: loops.scans.len() as u32,
            };
            loops.scans.push(scan);
        } else if let Some(crc) = crc_at(insts, head) {
            insts[head] = Inst::Crc {
                crc: loops.crcs.len() as u32,
            };
            loops.crcs.push(crc);
        }
    }
    loops
}

fn scan_at(insts: &[Inst], head: usize) -> Option<Scan> {
    let window = insts
        .get(head..head + SCAN_TABLE_OPS as usize)
        .unwrap_or(&[]);
    let (
        Inst::JumpGeU {
            left: at,
            right: end,
            target: done,
        },
        Inst::LoadIndexedU8 {
            result: byte,
            base,
            index,
        },
    ) = (*insts.get(head)?, *insts.get(head + 1)?)
    else {
        return None;
    };
    if index != at {
        return None;
    }
    let (test, found, step) = match (insts.get(head + 2)?, window) {
        (
            Inst::JumpEqImmediate {
                left,
                right,
                target,
            },
            _,
        ) if *left == byte => (Test::Equal(*right), *target, head + 3),
        (
            Inst::LoadPlusU8 {
                result: class,
                address,
                plus,
            },
            [_, _, _, Inst::JumpIf { condition, target }, ..],
        ) if *address == byte && condition == class => (
            Test::Table {
                class: *class,
                table: *plus,
            },
            *target,
            head + 4,
        ),
        _ => return None,
    };
    let (
        Inst::I32AddImmediate {
            result,
            left,
            right: 1,
        },
        Inst::Jump(back),
    ) = (*insts.get(step)?, *insts.get(step + 1)?)
    else {
        return None;
    };
    if result != at || left != at || back as usize != head {
        return None;
    }
    let mut slots = vec![at, end, base, byte];
    if let Test::Table { class, .. } = test {
        slots.push(class);
    }
    distinct(&slots).then_some(Scan {
        at,
        end,
        base,
        byte,
        test,
        done,
        found,
    })
}

fn crc_at(insts: &[Inst], head: usize) -> Option<Crc> {
    let window = insts.get(head..head + CRC_OPS as usize)?;
    let [
        Inst::JumpGeU {
            left: at,
            right: end,
            target: done,
        },
        Inst::I32ShrUImmediate {
            result: shifted,
            left: crc,
            right: CRC_BYTE_BITS,
        },
        Inst::LoadIndexedU8 {
            result: byte,
            base,
            index: byte_at,
        },
        Inst::I32Xor {
            result: index,
            left: mixed_crc,
            right: mixed_byte,
        },
        Inst::I32AndImmediate {
            result: masked,
            left: masked_from,
            right: CRC_BYTE_MASK,
        },
        Inst::I32ShlImmediate {
            result: shifted_index,
            left: shifted_from,
            right: CRC_WORD_SHIFT,
        },
        Inst::LoadPlusI32 {
            result: word,
            address: word_at,
            plus: table,
        },
        Inst::I32Xor {
            result: next,
            left: next_shifted,
            right: next_word,
        },
        Inst::I32AddImmediate {
            result: stepped,
            left: step_from,
            right: 1,
        },
        Inst::Jump(back),
    ] = *window
    else {
        return None;
    };
    let chained = byte_at == at
        && mixed_crc == crc
        && mixed_byte == byte
        && masked == index
        && masked_from == index
        && shifted_index == index
        && shifted_from == index
        && word_at == index
        && next == crc
        && next_shifted == shifted
        && next_word == word
        && stepped == at
        && step_from == at
        && back as usize == head;
    (chained && distinct(&[at, end, base, crc, shifted, byte, index, word])).then_some(Crc {
        at,
        end,
        base,
        crc,
        shifted,
        byte,
        index,
        word,
        table,
        done,
    })
}
