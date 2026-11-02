//! The opaque position a long list hands out with each page: the order it
//! was taken in and the sort key of the last row sent. A client sends it
//! back as `after` and gets the rows that follow; it never builds one.
//!
//! A list in random order carries its seed in the cursor, so every page
//! of one walk reads the same shuffle: no row twice, none skipped.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rusqlite::types::Value;
use serde::{Deserialize, Serialize};

/// A position in one list order.
#[derive(Debug, Clone, PartialEq)]
pub struct Cursor {
    /// The list and order the key belongs to, e.g. `albums/name/asc`.
    pub order: String,
    /// The shuffle seed of a random order; zero for any other.
    pub seed: u32,
    /// The last row's sort key, one value per order column.
    pub key: Vec<Value>,
}

/// A cursor that is not one the list gave out, or one from another order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadCursor;

/// One sort key value as the cursor writes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum Part {
    Int(i64),
    Real(f64),
    Text(String),
}

#[derive(Serialize, Deserialize)]
struct Wire {
    o: String,
    #[serde(default)]
    s: u32,
    k: Vec<Part>,
}

impl Cursor {
    /// The cursor's text.
    pub fn encode(&self) -> String {
        let key = self
            .key
            .iter()
            .map(|value| match value {
                Value::Integer(value) => Part::Int(*value),
                Value::Real(value) => Part::Real(*value),
                Value::Text(value) => Part::Text(value.clone()),
                // Order keys are never NULL or blobs; the text form keeps
                // the cursor readable if one ever is.
                Value::Null | Value::Blob(_) => Part::Text(String::new()),
            })
            .collect();
        let wire = Wire {
            o: self.order.clone(),
            s: self.seed,
            k: key,
        };
        // Serializing plain strings and numbers cannot fail.
        let json = serde_json::to_vec(&wire).unwrap_or_default();
        URL_SAFE_NO_PAD.encode(json)
    }

    /// The cursor `text` names, when it was given out for `order` with a
    /// key of `columns` values.
    pub fn decode(text: &str, order: &str, columns: usize) -> Result<Self, BadCursor> {
        let json = URL_SAFE_NO_PAD.decode(text).map_err(|_| BadCursor)?;
        let wire: Wire = serde_json::from_slice(&json).map_err(|_| BadCursor)?;
        if wire.o != order || wire.k.len() != columns {
            return Err(BadCursor);
        }
        let key = wire
            .k
            .into_iter()
            .map(|part| match part {
                Part::Int(value) => Value::Integer(value),
                Part::Real(value) => Value::Real(value),
                Part::Text(value) => Value::Text(value),
            })
            .collect();
        Ok(Self {
            order: wire.o,
            seed: wire.s,
            key,
        })
    }
}

/// A fresh shuffle seed for the first page of a random list.
pub fn fresh_seed() -> u32 {
    uuid::Uuid::new_v4().as_u128() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cursor_reads_back_only_for_its_own_order() {
        let cursor = Cursor {
            order: "albums/name/asc".to_owned(),
            seed: 7,
            key: vec![
                Value::Text("perf album 09000".to_owned()),
                Value::Real(1000.0),
                Value::Real(1000.5),
                Value::Integer(3),
            ],
        };
        let text = cursor.encode();
        assert_eq!(Cursor::decode(&text, "albums/name/asc", 4), Ok(cursor));
        assert_eq!(Cursor::decode(&text, "albums/name/desc", 4), Err(BadCursor));
        assert_eq!(Cursor::decode(&text, "albums/name/asc", 3), Err(BadCursor));
        assert_eq!(
            Cursor::decode("not a cursor", "albums/name/asc", 3),
            Err(BadCursor)
        );
    }
}
