//! Read the schema-21 payload map without buffering its entire inline JSON value twice.
use super::StoredPayload;
use serde::{
    Deserialize, Deserializer,
    de::{Error, MapAccess, SeqAccess, Visitor, value::SeqAccessDeserializer},
};
use std::{fmt, path::PathBuf};

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum Field {
    Storage,
    Value,
    Sha256,
    Bytes,
    RelativePath,
}

#[derive(Deserialize)]
#[serde(variant_identifier, rename_all = "snake_case")]
enum Storage {
    Inline,
    Blob,
}

pub(super) fn read<'de, D>(deserializer: D) -> Result<StoredPayload, D::Error>
where
    D: Deserializer<'de>,
{
    struct Payload;
    impl<'de> Visitor<'de> for Payload {
        type Value = StoredPayload;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a schema-21 journal payload")
        }

        // Leave non-map sequence representations to the original schema reader.
        fn visit_seq<S: SeqAccess<'de>>(self, seq: S) -> Result<Self::Value, S::Error> {
            StoredPayload::deserialize(SeqAccessDeserializer::new(seq))
        }

        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            fn field<'de, T: Deserialize<'de>, M: MapAccess<'de>>(
                map: &mut M,
                slot: &mut Option<T>,
                name: &'static str,
            ) -> Result<(), M::Error> {
                if slot.is_some() {
                    return Err(Error::duplicate_field(name));
                }
                *slot = Some(map.next_value()?);
                Ok(())
            }
            let (mut storage, mut value, mut sha256, mut bytes, mut relative_path) =
                (None, None, None, None, None::<PathBuf>);
            while let Some(key) = map.next_key::<Field>()? {
                match key {
                    Field::Storage => field(&mut map, &mut storage, "storage")?,
                    Field::Value => field(&mut map, &mut value, "value")?,
                    Field::Sha256 => field(&mut map, &mut sha256, "sha256")?,
                    Field::Bytes => field(&mut map, &mut bytes, "bytes")?,
                    Field::RelativePath => field(&mut map, &mut relative_path, "relative_path")?,
                }
            }
            // Check the exact field set even when the tag is last. A present null
            // is a real inline value, not an absent field.
            match (storage, value, sha256, bytes, relative_path) {
                (Some(Storage::Inline), Some(value), None, None, None) => {
                    Ok(StoredPayload::Inline { value })
                }
                (Some(Storage::Blob), None, Some(sha256), Some(bytes), Some(relative_path)) => {
                    Ok(StoredPayload::Blob {
                        sha256,
                        bytes,
                        relative_path,
                    })
                }
                _ => Err(Error::custom("fields do not match journal payload storage")),
            }
        }
    }
    deserializer.deserialize_any(Payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn streaming_reader_matches_schema_for_objects_and_sequences() {
        // The derived reader is retained for positional encodings and as the
        // compatibility oracle here. Compare canonical values, not error wording.
        fn check(s: &str) {
            let old = serde_json::from_str::<StoredPayload>(s)
                .ok()
                .map(|p| serde_json::to_value(p).unwrap());
            let mut decoder = serde_json::Deserializer::from_str(s);
            let new = read(&mut decoder)
                .ok()
                .map(|p| serde_json::to_value(p).unwrap());
            assert_eq!(new, old, "{s}");
        }
        for s in [
            r#"{"value":null,"storage":"inline"}"#,
            r#"{"storage":"inline","value":{"x":[null,-0.0,1e10,"тест"],"x":42}}"#,
            r#"{"storage":"inline","value":1,"value":2}"#,
            r#"{"storage":"blob","storage":"inline","value":2}"#,
            r#"{"storage":"inline","value":0,"unknown":null}"#,
            r#"{"storage":"inline","value":0,"sha256":null}"#,
            r#"{"bytes":10,"relative_path":"blobs/a.json","sha256":"a","storage":"blob"}"#,
            r#"{"storage":"blob","sha256":"a","bytes":1,"relative_path":"b","value":null}"#,
            r#"{"storage":"blob","sha256":"a","bytes":1,"bytes":2,"relative_path":"b"}"#,
            r#"["inline",{"k":"v"}]"#,
            r#"["blob","a",3,"b"]"#,
            r#"["inline",{},null]"#,
            "null",
            "42",
            "{}",
        ] {
            check(s)
        }
        for tag in [
            Value::Null,
            json!(0),
            json!(1),
            json!(255),
            json!("inline"),
            json!("blob"),
            json!("other"),
            json!({"inline":null}),
        ] {
            for bits in 0..16 {
                let mut m = serde_json::Map::new();
                m.insert("storage".to_owned(), tag.clone());
                for (i, (k, v)) in [
                    ("value", json!({"é":-57.12})),
                    ("sha256", json!("hash")),
                    ("bytes", json!(1)),
                    ("relative_path", json!("path")),
                ]
                .into_iter()
                .enumerate()
                {
                    if bits & (1 << i) != 0 {
                        m.insert(k.to_owned(), v);
                    }
                }
                check(&Value::Object(m).to_string());
            }
        }
    }
}
