//! The wire is append-only: what `registry/wire.txt` records never changes.

use std::collections::HashSet;

use serde::Deserialize;
use serde_reflection::{
    ContainerFormat, Format, Named, Registry, Tracer, TracerConfig, VariantFormat,
};

use crate::*;

/// The wire's layout as committed: one line a struct field, tuple element or
/// enum variant, with its position.
const WIRE: &str = include_str!("../../registry/wire.txt");

#[test]
fn the_wire_layout_only_grows() {
    let live = layout_lines(&trace_wire());
    let live_set: HashSet<&str> = live.iter().map(String::as_str).collect();
    let committed: Vec<&str> = WIRE
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    let committed_set: HashSet<&str> = committed.iter().copied().collect();
    assert_eq!(
        committed_set.len(),
        committed.len(),
        "registry/wire.txt repeats a line"
    );
    let changed: Vec<&str> = committed
        .iter()
        .copied()
        .filter(|line| !live_set.contains(line))
        .collect();
    assert!(
        changed.is_empty(),
        "these wire positions changed or disappeared; keep them and retire them instead \
         (README.md, Protocol):\n{}",
        changed.join("\n")
    );
    let unregistered: Vec<&String> = live
        .iter()
        .filter(|line| !committed_set.contains(line.as_str()))
        .collect();
    assert!(
        unregistered.is_empty(),
        "append these to crates/radovid_proto/registry/wire.txt:\n{}",
        unregistered
            .iter()
            .map(|line| line.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Every type reachable from the two things that cross the wire, each enum
/// traced on its own so that no variant goes unseen.
fn trace_wire() -> Registry {
    let mut tracer = Tracer::new(TracerConfig::default());
    let mut enums = HashSet::new();
    trace::<ClientFrame>(&mut tracer, &mut enums);
    trace::<ClientMsg>(&mut tracer, &mut enums);
    trace::<ServerMsg>(&mut tracer, &mut enums);
    trace::<Reply>(&mut tracer, &mut enums);
    trace::<Event>(&mut tracer, &mut enums);
    trace::<Error>(&mut tracer, &mut enums);
    trace::<Presence>(&mut tracer, &mut enums);
    trace::<AccountKind>(&mut tracer, &mut enums);
    trace::<Audience>(&mut tracer, &mut enums);
    trace::<Outcome>(&mut tracer, &mut enums);
    trace::<EndReason>(&mut tracer, &mut enums);
    trace::<Team>(&mut tracer, &mut enums);
    let registry = tracer.registry().expect("every wire type traces whole");
    for (name, container) in &registry {
        if matches!(container, ContainerFormat::Enum(_)) {
            assert!(
                enums.contains(name),
                "trace enum {name} on its own in trace_wire"
            );
        }
    }
    registry
}

fn trace<'de, T: Deserialize<'de>>(tracer: &mut Tracer, enums: &mut HashSet<String>) {
    let (format, _) = tracer.trace_simple_type::<T>().expect("trace");
    if let Format::TypeName(name) = format {
        enums.insert(name);
    }
}

/// The registry as lines, each naming one position on the wire.
fn layout_lines(registry: &Registry) -> Vec<String> {
    let mut lines = Vec::new();
    for (name, container) in registry {
        match container {
            ContainerFormat::UnitStruct => {
                lines.push(format!("unit {name}"));
            }
            ContainerFormat::NewTypeStruct(inner) => {
                lines.push(format!("newtype {name} {}", render(inner)));
            }
            ContainerFormat::TupleStruct(items) => tuple_lines(&mut lines, name, items),
            ContainerFormat::Struct(fields) => field_lines(&mut lines, name, fields),
            ContainerFormat::Enum(variants) => {
                for (index, variant) in variants {
                    variant_lines(&mut lines, name, *index, variant);
                }
            }
        }
    }
    lines
}

fn variant_lines(lines: &mut Vec<String>, owner: &str, index: u32, variant: &Named<VariantFormat>) {
    let head = format!("variant {owner} {index} {}", variant.name);
    match &variant.value {
        VariantFormat::Unit => lines.push(head),
        VariantFormat::NewType(inner) => lines.push(format!("{head}({})", render(inner))),
        VariantFormat::Tuple(items) => {
            lines.push(format!("{head}()"));
            tuple_lines(lines, &format!("{owner}::{}", variant.name), items);
        }
        VariantFormat::Struct(fields) => {
            lines.push(format!("{head} {{}}"));
            field_lines(lines, &format!("{owner}::{}", variant.name), fields);
        }
        VariantFormat::Variable(_) => panic!("{owner}::{} traced incompletely", variant.name),
    }
}

fn tuple_lines(lines: &mut Vec<String>, owner: &str, items: &[Format]) {
    for (index, item) in items.iter().enumerate() {
        lines.push(format!("element {owner} {index} {}", render(item)));
    }
}

fn field_lines(lines: &mut Vec<String>, owner: &str, fields: &[Named<Format>]) {
    for (index, field) in fields.iter().enumerate() {
        lines.push(format!(
            "field {owner} {index} {}: {}",
            field.name,
            render(&field.value)
        ));
    }
}

/// A field's type as Rust spells it, primitives in lower case.
fn render(format: &Format) -> String {
    match format {
        Format::TypeName(name) => name.clone(),
        Format::Unit => "()".to_string(),
        Format::Bool => "bool".to_string(),
        Format::I8 => "i8".to_string(),
        Format::I16 => "i16".to_string(),
        Format::I32 => "i32".to_string(),
        Format::I64 => "i64".to_string(),
        Format::I128 => "i128".to_string(),
        Format::U8 => "u8".to_string(),
        Format::U16 => "u16".to_string(),
        Format::U32 => "u32".to_string(),
        Format::U64 => "u64".to_string(),
        Format::U128 => "u128".to_string(),
        Format::F32 => "f32".to_string(),
        Format::F64 => "f64".to_string(),
        Format::Char => "char".to_string(),
        Format::Str => "String".to_string(),
        Format::Bytes => "bytes".to_string(),
        Format::Option(inner) => format!("Option<{}>", render(inner)),
        Format::Seq(inner) => format!("Vec<{}>", render(inner)),
        Format::Map { key, value } => format!("Map<{}, {}>", render(key), render(value)),
        Format::Tuple(items) => {
            let items: Vec<String> = items.iter().map(render).collect();
            format!("({})", items.join(", "))
        }
        Format::TupleArray { content, size } => format!("[{}; {size}]", render(content)),
        Format::Variable(_) => panic!("a type traced incompletely"),
    }
}
