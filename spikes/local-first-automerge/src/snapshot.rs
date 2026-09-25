//! Canonical, human-readable rendering of a document's CURRENT state.
//!
//! Two replicas that render identically hold the same visible data, which is
//! what the convergence assertions compare (alongside heads). Map keys are
//! emitted in sorted order and, where a key has concurrent conflicting values,
//! only Automerge's deterministic winner is shown; use [`conflicts`] to see
//! the losers.

use automerge::{AutoCommit, ObjId, ObjType, Prop, ReadDoc, Value, ROOT};

pub fn render(doc: &AutoCommit) -> String {
    let mut out = String::new();
    render_obj(doc, &ROOT, ObjType::Map, &mut out);
    out
}

fn render_obj(doc: &AutoCommit, obj: &ObjId, kind: ObjType, out: &mut String) {
    match kind {
        ObjType::Map | ObjType::Table => {
            out.push('{');
            for (i, key) in doc.keys(obj).enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&format!("{key:?}:"));
                render_value(doc, obj, key.into(), out);
            }
            out.push('}');
        }
        ObjType::List => {
            out.push('[');
            for i in 0..doc.length(obj) {
                if i > 0 {
                    out.push(',');
                }
                render_value(doc, obj, i.into(), out);
            }
            out.push(']');
        }
        ObjType::Text => {
            out.push_str(&format!("{:?}", doc.text(obj).unwrap()));
        }
    }
}

fn render_value(doc: &AutoCommit, obj: &ObjId, prop: Prop, out: &mut String) {
    match doc.get(obj, prop).unwrap() {
        Some((Value::Object(kind), child)) => render_obj(doc, &child, kind, out),
        Some((Value::Scalar(s), _)) => out.push_str(&s.to_string()),
        None => out.push_str("null"),
    }
}

/// Every concurrent value currently held for `key` in map `obj`. More than one
/// means the key was written concurrently and [`render`] shows only the winner.
pub fn conflicts(doc: &AutoCommit, obj: &ObjId, key: &str) -> usize {
    doc.get_all(obj, key).unwrap().len()
}
