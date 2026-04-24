//! Execution trace support.
//!
//! Trace output is intentionally JSON but does not depend on serde. Keeping the
//! format small and explicit makes it suitable for later replay and diff tools.

use crate::vm::TrapCode;
use std::cmp::Ordering;
use std::fmt;
use std::fmt::Write;

/// One VM instruction execution event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceEvent {
    pub cycle: u64,
    pub pc: usize,
    pub opcode: String,
    pub arg1: i32,
    pub arg2: i32,
    pub stack_before: Vec<i64>,
    pub stack_after: Vec<i64>,
    pub frame_depth_before: usize,
    pub frame_depth_after: usize,
    pub next_pc: usize,
    pub outcome: TraceOutcome,
}

/// Result of executing one traced instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraceOutcome {
    Continue,
    Jump,
    Exit,
    Trap { code: TrapCode, message: String },
}

/// First observable mismatch between two traces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceDivergence {
    pub event_index: usize,
    pub field: String,
    pub left: String,
    pub right: String,
}

impl TraceDivergence {
    fn new(event_index: usize, field: &str, left: String, right: String) -> Self {
        Self {
            event_index,
            field: field.to_string(),
            left,
            right,
        }
    }
}

/// Return the first differing field between two instruction traces.
pub fn first_trace_divergence(
    left: &[TraceEvent],
    right: &[TraceEvent],
) -> Option<TraceDivergence> {
    for (index, (left_event, right_event)) in left.iter().zip(right).enumerate() {
        if let Some(divergence) = compare_event(index, left_event, right_event) {
            return Some(divergence);
        }
    }

    match left.len().cmp(&right.len()) {
        Ordering::Equal => None,
        Ordering::Less => Some(TraceDivergence::new(
            left.len(),
            "length",
            "<end>".to_string(),
            summarize_event(&right[left.len()]),
        )),
        Ordering::Greater => Some(TraceDivergence::new(
            right.len(),
            "length",
            summarize_event(&left[right.len()]),
            "<end>".to_string(),
        )),
    }
}

/// Return the first semantic trace mismatch, normalizing runtime-internal values.
///
/// VM and GC VM intentionally use different internal representations for heap
/// references. This comparator keeps raw PC/opcode/control-flow checks, but
/// normalizes stack values that are known to be array references.
pub fn first_semantic_trace_divergence(
    left: &[TraceEvent],
    right: &[TraceEvent],
) -> Option<TraceDivergence> {
    let mut left_normalizer = TraceNormalizer::new();
    let mut right_normalizer = TraceNormalizer::new();

    for (index, (left_event, right_event)) in left.iter().zip(right).enumerate() {
        let left_stack = left_normalizer.normalize_event(left_event);
        let right_stack = right_normalizer.normalize_event(right_event);

        if let Some(divergence) =
            compare_event_semantic(index, left_event, right_event, &left_stack, &right_stack)
        {
            return Some(divergence);
        }
    }

    match left.len().cmp(&right.len()) {
        Ordering::Equal => None,
        Ordering::Less => Some(TraceDivergence::new(
            left.len(),
            "length",
            "<end>".to_string(),
            summarize_event(&right[left.len()]),
        )),
        Ordering::Greater => Some(TraceDivergence::new(
            right.len(),
            "length",
            summarize_event(&left[right.len()]),
            "<end>".to_string(),
        )),
    }
}

/// Serialize trace events as a stable JSON object.
pub fn events_to_json(backend: &str, events: &[TraceEvent]) -> String {
    let mut out = String::new();
    out.push_str("{\n  \"backend\": ");
    push_json_string(&mut out, backend);
    out.push_str(",\n  \"events\": [");

    for (index, event) in events.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("\n    ");
        push_event_json(&mut out, event);
    }

    out.push_str("\n  ]\n}");
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedStackEvent {
    before: Vec<NormalizedValue>,
    after: Vec<NormalizedValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NormalizedValue {
    Int(i64),
    ArrayRef(usize),
}

struct TraceNormalizer {
    stack: Vec<NormalizedValue>,
    next_array_ref: usize,
}

impl TraceNormalizer {
    fn new() -> Self {
        Self {
            stack: Vec::new(),
            next_array_ref: 0,
        }
    }

    fn normalize_event(&mut self, event: &TraceEvent) -> NormalizedStackEvent {
        let before = self.normalize_before(event);
        let after = self.normalize_after(event, &before);
        self.stack = after.clone();
        NormalizedStackEvent { before, after }
    }

    fn normalize_before(&self, event: &TraceEvent) -> Vec<NormalizedValue> {
        if self.stack.len() == event.stack_before.len() {
            self.stack.clone()
        } else {
            event
                .stack_before
                .iter()
                .copied()
                .map(NormalizedValue::Int)
                .collect()
        }
    }

    fn normalize_after(
        &mut self,
        event: &TraceEvent,
        before: &[NormalizedValue],
    ) -> Vec<NormalizedValue> {
        let mut after = preserve_stack_prefix(&event.stack_before, before, &event.stack_after);

        if matches!(event.opcode.as_str(), "AllocArray" | "ArrayNew")
            && event.stack_after.len() == event.stack_before.len() + 1
        {
            if let Some(last) = after.last_mut() {
                *last = NormalizedValue::ArrayRef(self.next_array_ref);
                self.next_array_ref += 1;
            }
        }

        after
    }
}

fn preserve_stack_prefix(
    raw_before: &[i64],
    normalized_before: &[NormalizedValue],
    raw_after: &[i64],
) -> Vec<NormalizedValue> {
    raw_after
        .iter()
        .enumerate()
        .map(|(index, value)| {
            if raw_before.get(index) == Some(value) {
                normalized_before
                    .get(index)
                    .cloned()
                    .unwrap_or(NormalizedValue::Int(*value))
            } else {
                NormalizedValue::Int(*value)
            }
        })
        .collect()
}

fn compare_event(index: usize, left: &TraceEvent, right: &TraceEvent) -> Option<TraceDivergence> {
    field_divergence(index, "cycle", &left.cycle, &right.cycle)
        .or_else(|| field_divergence(index, "pc", &left.pc, &right.pc))
        .or_else(|| field_divergence(index, "opcode", &left.opcode, &right.opcode))
        .or_else(|| field_divergence(index, "arg1", &left.arg1, &right.arg1))
        .or_else(|| field_divergence(index, "arg2", &left.arg2, &right.arg2))
        .or_else(|| {
            field_divergence(
                index,
                "stack_before",
                &left.stack_before,
                &right.stack_before,
            )
        })
        .or_else(|| field_divergence(index, "stack_after", &left.stack_after, &right.stack_after))
        .or_else(|| {
            field_divergence(
                index,
                "frame_depth_before",
                &left.frame_depth_before,
                &right.frame_depth_before,
            )
        })
        .or_else(|| {
            field_divergence(
                index,
                "frame_depth_after",
                &left.frame_depth_after,
                &right.frame_depth_after,
            )
        })
        .or_else(|| field_divergence(index, "next_pc", &left.next_pc, &right.next_pc))
        .or_else(|| field_divergence(index, "outcome", &left.outcome, &right.outcome))
}

fn compare_event_semantic(
    index: usize,
    left: &TraceEvent,
    right: &TraceEvent,
    left_stack: &NormalizedStackEvent,
    right_stack: &NormalizedStackEvent,
) -> Option<TraceDivergence> {
    field_divergence(index, "cycle", &left.cycle, &right.cycle)
        .or_else(|| field_divergence(index, "pc", &left.pc, &right.pc))
        .or_else(|| field_divergence(index, "opcode", &left.opcode, &right.opcode))
        .or_else(|| field_divergence(index, "arg1", &left.arg1, &right.arg1))
        .or_else(|| field_divergence(index, "arg2", &left.arg2, &right.arg2))
        .or_else(|| {
            field_divergence(
                index,
                "stack_before",
                &left_stack.before,
                &right_stack.before,
            )
        })
        .or_else(|| field_divergence(index, "stack_after", &left_stack.after, &right_stack.after))
        .or_else(|| {
            field_divergence(
                index,
                "frame_depth_before",
                &left.frame_depth_before,
                &right.frame_depth_before,
            )
        })
        .or_else(|| {
            field_divergence(
                index,
                "frame_depth_after",
                &left.frame_depth_after,
                &right.frame_depth_after,
            )
        })
        .or_else(|| field_divergence(index, "next_pc", &left.next_pc, &right.next_pc))
        .or_else(|| field_divergence(index, "outcome", &left.outcome, &right.outcome))
}

fn field_divergence<T: fmt::Debug + PartialEq>(
    index: usize,
    field: &str,
    left: &T,
    right: &T,
) -> Option<TraceDivergence> {
    if left == right {
        None
    } else {
        Some(TraceDivergence::new(
            index,
            field,
            format!("{:?}", left),
            format!("{:?}", right),
        ))
    }
}

fn summarize_event(event: &TraceEvent) -> String {
    format!(
        "pc={} opcode={} stack_after={:?} outcome={:?}",
        event.pc, event.opcode, event.stack_after, event.outcome
    )
}

fn push_event_json(out: &mut String, event: &TraceEvent) {
    out.push('{');
    write!(out, "\"cycle\":{}", event.cycle).expect("write to string cannot fail");
    write!(out, ",\"pc\":{}", event.pc).expect("write to string cannot fail");
    out.push_str(",\"opcode\":");
    push_json_string(out, &event.opcode);
    write!(out, ",\"arg1\":{}", event.arg1).expect("write to string cannot fail");
    write!(out, ",\"arg2\":{}", event.arg2).expect("write to string cannot fail");
    out.push_str(",\"stack_before\":");
    push_i64_array(out, &event.stack_before);
    out.push_str(",\"stack_after\":");
    push_i64_array(out, &event.stack_after);
    write!(out, ",\"frame_depth_before\":{}", event.frame_depth_before)
        .expect("write to string cannot fail");
    write!(out, ",\"frame_depth_after\":{}", event.frame_depth_after)
        .expect("write to string cannot fail");
    write!(out, ",\"next_pc\":{}", event.next_pc).expect("write to string cannot fail");
    out.push_str(",\"outcome\":");
    push_outcome_json(out, &event.outcome);
    out.push('}');
}

fn push_outcome_json(out: &mut String, outcome: &TraceOutcome) {
    match outcome {
        TraceOutcome::Continue => out.push_str("{\"kind\":\"continue\"}"),
        TraceOutcome::Jump => out.push_str("{\"kind\":\"jump\"}"),
        TraceOutcome::Exit => out.push_str("{\"kind\":\"exit\"}"),
        TraceOutcome::Trap { code, message } => {
            out.push_str("{\"kind\":\"trap\",\"code\":");
            write!(out, "{}", *code as u8).expect("write to string cannot fail");
            out.push_str(",\"trap\":");
            push_json_string(out, &format!("{:?}", code));
            out.push_str(",\"message\":");
            push_json_string(out, message);
            out.push('}');
        }
    }
}

fn push_i64_array(out: &mut String, values: &[i64]) {
    out.push('[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write!(out, "{}", value).expect("write to string cannot fail");
    }
    out.push(']');
}

fn push_json_string(out: &mut String, value: &str) {
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch <= '\u{1f}' => {
                write!(out, "\\u{:04x}", ch as u32).expect("write to string cannot fail");
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

impl fmt::Display for TraceDivergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "event #{} field {} differs: {} vs {}",
            self.event_index, self.field, self.left, self.right
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event() -> TraceEvent {
        TraceEvent {
            cycle: 1,
            pc: 0,
            opcode: "LoadConst".to_string(),
            arg1: 42,
            arg2: 0,
            stack_before: vec![],
            stack_after: vec![42],
            frame_depth_before: 1,
            frame_depth_after: 1,
            next_pc: 1,
            outcome: TraceOutcome::Continue,
        }
    }

    #[test]
    fn escapes_json_strings() {
        let mut event = sample_event();
        event.opcode = "LoadConst\"x".to_string();

        let json = events_to_json("VM", &[event]);
        assert!(json.contains("LoadConst\\\"x"));
    }

    #[test]
    fn detects_first_field_divergence() {
        let left = sample_event();
        let mut right = sample_event();
        right.stack_after = vec![43];

        let divergence = first_trace_divergence(&[left], &[right]).unwrap();
        assert_eq!(divergence.event_index, 0);
        assert_eq!(divergence.field, "stack_after");
    }

    #[test]
    fn detects_trace_length_divergence() {
        let event = sample_event();
        let divergence = first_trace_divergence(&[], &[event]).unwrap();

        assert_eq!(divergence.event_index, 0);
        assert_eq!(divergence.field, "length");
    }

    #[test]
    fn semantic_diff_normalizes_array_references() {
        let left = TraceEvent {
            cycle: 1,
            pc: 0,
            opcode: "AllocArray".to_string(),
            arg1: 3,
            arg2: 0,
            stack_before: vec![],
            stack_after: vec![1024],
            frame_depth_before: 1,
            frame_depth_after: 1,
            next_pc: 1,
            outcome: TraceOutcome::Continue,
        };
        let right = TraceEvent {
            stack_after: vec![0],
            ..left.clone()
        };

        assert!(
            first_trace_divergence(std::slice::from_ref(&left), std::slice::from_ref(&right))
                .is_some()
        );
        assert!(first_semantic_trace_divergence(&[left], &[right]).is_none());
    }
}
