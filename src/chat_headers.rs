//! Sidebar metadata without materializing message bodies, tool results or model history.
use anyhow::Result;
use serde::de::{Deserializer, IgnoredAny, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::{fmt, fs::File, io::BufReader, path::Path};

#[derive(Deserialize, Serialize)]
pub struct Header {
    pub id: String,
    pub title: String,
    pub updated: String,
    pub mode: String,
    #[serde(rename(deserialize = "messages"), deserialize_with = "count_messages")]
    pub count: u64,
}
fn count_messages<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<u64, D::Error> {
    struct Count;
    impl<'de> Visitor<'de> for Count {
        type Value = u64;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("an array of messages")
        }
        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> std::result::Result<u64, A::Error> {
            let mut count = 0u64;
            while sequence.next_element::<IgnoredAny>()?.is_some() {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| serde::de::Error::custom("Message count overflow"))?;
            }
            Ok(count)
        }
    }
    deserializer.deserialize_seq(Count)
}
pub fn read(path: &Path) -> Result<Header> {
    let file = File::open(path)?;
    let header: Header = serde_json::from_reader(BufReader::with_capacity(64 * 1024, file))?;
    uuid::Uuid::parse_str(&header.id)?;
    anyhow::ensure!(
        path.file_stem().and_then(|s| s.to_str()) == Some(header.id.as_str()),
        "ID чата не соответствует имени файла"
    );
    Ok(header)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn streams_bodies_history_and_escaped_strings_without_constructing_messages() {
        let root = std::env::temp_dir().join(format!("chat-header-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let path = root.join(format!("{id}.json"));
        let mut file = File::create(&path).unwrap();
        write!(file,"{{\"id\":\"{id}\",\"title\":\"Назад \\\"в чат\\\"\",\"mode\":\"agent\",\"updated\":\"now\",\"messages\":[{{\"content\":\"").unwrap();
        for _ in 0..512 {
            file.write_all(&vec![b'x'; 65536]).unwrap();
        }
        file.write_all(br#"","tools":[{"result":"escaped \" quote"}]},{"content":"second"}],"history":[{"content":"not sidebar data"}]}"#).unwrap();
        drop(file);
        let header = read(&path).unwrap();
        assert_eq!(header.count, 2);
        assert_eq!(header.title, "Назад \"в чат\"");
        let value = serde_json::to_value(header).unwrap();
        assert_eq!(value["count"], 2);
        assert!(value.get("messages").is_none());
        assert!(value.get("history").is_none());
        std::fs::write(&path,format!("{{\"id\":\"{id}\",\"title\":\"t\",\"mode\":\"chat\",\"updated\":\"now\",\"messages\":[]}} trailing")).unwrap();
        assert!(read(&path).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
