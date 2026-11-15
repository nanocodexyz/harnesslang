use super::compile::{Binary, Branch, Op, Unary};

#[derive(Clone, Copy, Debug)]
pub enum Condition {
    NonZero(u32),
    Zero(u32),
    Compare(Binary, u32, u32),
    CompareImmediate(Binary, u32, u32),
}

pub fn condition(last: Op, slot: u32) -> Option<Condition> {
    match last {
        Op::I32Binary {
            binary,
            result,
            left,
            right,
        } if result == slot && binary.is_compare() => Some(Condition::Compare(binary, left, right)),
        Op::I32BinaryImmediate {
            binary,
            result,
            left,
            right,
        } if result == slot && binary.is_compare() => {
            Some(Condition::CompareImmediate(binary, left, right))
        }
        Op::I32Unary {
            unary: Unary::Eqz,
            result,
            source,
        } if result == slot => Some(Condition::Zero(source)),
        _ => None,
    }
}

pub fn jump(condition: Condition, when: bool, target: u32) -> Op {
    match (condition, when) {
        (Condition::NonZero(condition), true) | (Condition::Zero(condition), false) => {
            Op::JumpIf { condition, target }
        }
        (Condition::NonZero(condition), false) | (Condition::Zero(condition), true) => {
            Op::JumpUnless { condition, target }
        }
        (Condition::Compare(compare, left, right), _) => Op::JumpCompare {
            compare: if when { compare } else { compare.negated() },
            left,
            right,
            target,
        },
        (Condition::CompareImmediate(compare, left, right), _) => Op::JumpCompareImmediate {
            compare: if when { compare } else { compare.negated() },
            left,
            right,
            target,
        },
    }
}

pub fn retarget(op: &mut Op, to: u32) {
    match op {
        Op::Jump(target)
        | Op::JumpIf { target, .. }
        | Op::JumpUnless { target, .. }
        | Op::JumpCompare { target, .. }
        | Op::JumpCompareImmediate { target, .. } => *target = to,
        other => unreachable!("a patch names a jump, not {other:?}"),
    }
}

fn result_slot(op: &mut Op) -> Option<&mut u32> {
    match op {
        Op::Move { result, .. }
        | Op::Const { result, .. }
        | Op::Select { result, .. }
        | Op::GlobalGet { result, .. }
        | Op::Load { result, .. }
        | Op::MemorySize { result }
        | Op::MemoryGrow { result, .. }
        | Op::I32Unary { result, .. }
        | Op::I64Unary { result, .. }
        | Op::I32Binary { result, .. }
        | Op::I32BinaryImmediate { result, .. }
        | Op::I64Binary { result, .. }
        | Op::I64BinaryConstant { result, .. }
        | Op::LoadIndexed { result, .. }
        | Op::LoadPlus { result, .. }
        | Op::Wrap { result, .. }
        | Op::ExtendS { result, .. } => Some(result),
        _ => None,
    }
}

pub fn result(mut op: Op) -> Option<u32> {
    result_slot(&mut op).copied()
}

pub fn set_result(op: &mut Op, slot: u32) {
    *result_slot(op).expect("only an op with a result is retargeted") = slot;
}

pub fn set_unary(op: &mut Op, slot: u32, from: u32) {
    match op {
        Op::Move { result, source }
        | Op::I32Unary { result, source, .. }
        | Op::I64Unary { result, source, .. }
        | Op::Wrap { result, source }
        | Op::ExtendS { result, source } => {
            *result = slot;
            *source = from;
        }
        other => unreachable!("a unary op has one source, not {other:?}"),
    }
}

fn slots(op: Op) -> [Option<u32>; 4] {
    match op {
        Op::Unreachable | Op::Jump(_) | Op::ReturnVoid => [None; 4],
        Op::Call { .. } | Op::CallImport { .. } => [None; 4],
        Op::JumpIf { condition, .. } | Op::JumpUnless { condition, .. } => {
            [Some(condition), None, None, None]
        }
        Op::JumpCompare { left, right, .. } => [Some(left), Some(right), None, None],
        Op::JumpCompareImmediate { left, .. } => [Some(left), None, None, None],
        Op::JumpTable { index, .. } => [Some(index), None, None, None],
        Op::Return { source } | Op::GlobalSet { source, .. } => [Some(source), None, None, None],
        Op::Move { result, source }
        | Op::I32Unary { result, source, .. }
        | Op::I64Unary { result, source, .. }
        | Op::Wrap { result, source }
        | Op::ExtendS { result, source } => [Some(result), Some(source), None, None],
        Op::Const { result, .. } | Op::GlobalGet { result, .. } | Op::MemorySize { result } => {
            [Some(result), None, None, None]
        }
        Op::MemoryGrow { result, delta } => [Some(result), Some(delta), None, None],
        Op::Select {
            result,
            first,
            second,
            condition,
        } => [Some(result), Some(first), Some(second), Some(condition)],
        Op::Load {
            result, address, ..
        } => [Some(result), Some(address), None, None],
        Op::Store { address, value, .. } => [Some(address), Some(value), None, None],
        Op::MemoryCopy {
            destination,
            source,
            length,
        } => [Some(destination), Some(source), Some(length), None],
        Op::MemoryFill {
            destination,
            value,
            length,
        } => [Some(destination), Some(value), Some(length), None],
        Op::I32Binary {
            result,
            left,
            right,
            ..
        }
        | Op::I64Binary {
            result,
            left,
            right,
            ..
        } => [Some(result), Some(left), Some(right), None],
        Op::I32BinaryImmediate { result, left, .. }
        | Op::I64BinaryConstant { result, left, .. } => [Some(result), Some(left), None, None],
        Op::LoadIndexed {
            result,
            base,
            index,
            ..
        } => [Some(result), Some(base), Some(index), None],
        Op::LoadPlus {
            result, address, ..
        } => [Some(result), Some(address), None, None],
        Op::StoreIndexed {
            base, index, value, ..
        } => [Some(base), Some(index), Some(value), None],
    }
}

pub fn assert_constants(ops: &[Op], constants: usize) {
    for op in ops {
        if let Op::I64BinaryConstant { constant, .. } = op {
            assert!(
                (*constant as usize) < constants,
                "{op:?} names a constant past its pool"
            );
        }
    }
}

pub fn assert_slots(ops: &[Op], table: &[Branch], slots_max: u32) {
    for op in ops {
        for slot in slots(*op).into_iter().flatten() {
            assert!(slot < slots_max, "{op:?} reads or writes past its frame");
        }
        if let Op::Call { base, .. } | Op::CallImport { base, .. } = op {
            assert!(*base <= slots_max, "{op:?} calls past its frame");
        }
    }
    for branch in table {
        if branch.arity == 1 {
            assert!(
                branch.source < slots_max,
                "a table branch reads past its frame"
            );
            assert!(
                branch.result < slots_max,
                "a table branch writes past its frame"
            );
        }
    }
}
