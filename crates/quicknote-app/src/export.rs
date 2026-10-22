//! 外部导出时间使用 ISO 8601 UTC，内部领域时间仍保存为 Unix 毫秒。

use chrono::{DateTime, SecondsFormat};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 保留毫秒精度；超出可表示范围时中止导出，避免写出不完整元数据。
pub(crate) fn utc_timestamp(value: i64) -> Result<String, String> {
    DateTime::from_timestamp_millis(value)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
        .ok_or_else(|| "导出时间超出 ISO 8601 支持范围".to_owned())
}

fn parse_timestamp(value: &str) -> Result<i64, chrono::ParseError> {
    DateTime::parse_from_rfc3339(value).map(|time| time.timestamp_millis())
}

/// 必填时间字段的序列化合同，仅用于导出 DTO。
pub(crate) mod timestamp {
    use super::*;

    pub fn serialize<S: Serializer>(value: &i64, serializer: S) -> Result<S::Ok, S::Error> {
        utc_timestamp(*value)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
        let value = String::deserialize(deserializer)?;
        parse_timestamp(&value).map_err(serde::de::Error::custom)
    }
}

/// 可空时间保留 JSON null，不把缺失时间转换为 epoch。
pub(crate) mod optional_timestamp {
    use super::*;

    pub fn serialize<S: Serializer>(value: &Option<i64>, serializer: S) -> Result<S::Ok, S::Error> {
        value
            .map(utc_timestamp)
            .transpose()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<i64>, D::Error> {
        let value = Option::<String>::deserialize(deserializer)?;
        value
            .as_deref()
            .map(parse_timestamp)
            .transpose()
            .map_err(serde::de::Error::custom)
    }
}
