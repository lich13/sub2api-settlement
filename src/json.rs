use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
pub fn dumps<T: Serialize + ?Sized>(v: &T) -> String {
    serde_json::to_string(v).expect("JSON value is serializable")
}
struct DigestWriter(Sha256);
impl std::io::Write for DigestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub fn digest<T: Serialize + ?Sized>(v: &T) -> String {
    let mut writer = DigestWriter(Sha256::new());
    serde_json::to_writer(&mut writer, v).expect("JSON value is serializable");
    hex::encode(writer.0.finalize())
}
pub fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub fn string(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_owned()
}
pub fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
pub fn int(v: &Value, key: &str) -> i64 {
    v[key].as_i64().unwrap_or(0)
}
pub fn flag(v: &Value, key: &str) -> bool {
    v[key].as_bool().unwrap_or_else(|| int(v, key) != 0)
}
pub fn array(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
pub fn merge(mut a: Value, b: Value) -> Value {
    if let (Some(a), Value::Object(b)) = (a.as_object_mut(), b) {
        a.extend(b);
    }
    a
}
pub fn without(mut v: Value, keys: &[&str]) -> Value {
    if let Some(m) = v.as_object_mut() {
        for k in keys {
            m.remove(*k);
        }
    }
    v
}
pub fn obj() -> Value {
    json!({})
}

// Python sorts integer dictionary keys numerically before encoding them as strings.
// This only applies to the original in-memory billing weights, not parsed snapshots.
pub fn billing_digest(v: &Value) -> String {
    struct Weights<'a>(&'a Value);
    impl Serialize for Weights<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap;
            let mut pairs: Vec<_> = self
                .0
                .as_object()
                .into_iter()
                .flat_map(|m| m.iter())
                .collect();
            pairs.sort_by_key(|(k, _)| k.parse::<i64>().unwrap_or(0));
            let mut map = serializer.serialize_map(Some(pairs.len()))?;
            for (k, v) in pairs {
                map.serialize_entry(k, v)?;
            }
            map.end()
        }
    }
    struct Bill<'a>(&'a Value);
    impl Serialize for Bill<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap;
            let object = self.0.as_object();
            let mut map = serializer.serialize_map(object.map(|m| m.len()))?;
            if let Some(object) = object {
                for (k, v) in object {
                    if k == "weights" {
                        map.serialize_entry(k, &Weights(v))?;
                    } else {
                        map.serialize_entry(k, v)?;
                    }
                }
            }
            map.end()
        }
    }
    digest(&Bill(v))
}
