use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use url::Url;

/// Platform-neutral commands produced by desktop or future mobile activation adapters.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ActivationCommand {
    Open {
        note_id: i64,
        reminder_id: i64,
        delivery_at: i64,
    },
    Snooze {
        note_id: i64,
        reminder_id: i64,
        delivery_at: i64,
    },
    Archive {
        note_id: i64,
        reminder_id: i64,
        delivery_at: i64,
    },
}

impl ActivationCommand {
    /// Parses the platform adapter's URI into one shared domain command.
    pub fn parse(value: &str) -> Result<Self, String> {
        let url = Url::parse(value).map_err(|error| format!("Invalid activation URI: {error}"))?;
        if url.scheme() != "quicknote-spike" {
            return Err("Unexpected activation URI scheme.".to_owned());
        }
        let action = url.host_str().ok_or("Activation action is missing.")?;
        let values: HashMap<_, _> = url.query_pairs().into_owned().collect();
        let note_id = parse_id(&values, "note")?;
        let reminder_id = parse_id(&values, "reminder")?;
        let delivery_at = parse_id(&values, "delivery")?;
        match action {
            "open" => Ok(Self::Open {
                note_id,
                reminder_id,
                delivery_at,
            }),
            "snooze" => Ok(Self::Snooze {
                note_id,
                reminder_id,
                delivery_at,
            }),
            "archive" => Ok(Self::Archive {
                note_id,
                reminder_id,
                delivery_at,
            }),
            _ => Err(format!("Unsupported activation action: {action}")),
        }
    }

    /// Serializes a command as a protocol URI understood by every platform shell.
    pub fn as_uri(&self) -> String {
        let (action, note_id, reminder_id, delivery_at) = match self {
            Self::Open {
                note_id,
                reminder_id,
                delivery_at,
            } => ("open", note_id, reminder_id, delivery_at),
            Self::Snooze {
                note_id,
                reminder_id,
                delivery_at,
            } => ("snooze", note_id, reminder_id, delivery_at),
            Self::Archive {
                note_id,
                reminder_id,
                delivery_at,
            } => ("archive", note_id, reminder_id, delivery_at),
        };
        format!(
            "quicknote-spike://{action}?note={note_id}&reminder={reminder_id}&delivery={delivery_at}"
        )
    }
}

fn parse_id(values: &HashMap<String, String>, key: &str) -> Result<i64, String> {
    values
        .get(key)
        .ok_or_else(|| format!("Activation parameter '{key}' is missing."))?
        .parse::<i64>()
        .map_err(|_| format!("Activation parameter '{key}' is invalid."))
}

#[cfg(test)]
mod tests {
    use super::ActivationCommand;

    #[test]
    fn activation_uri_round_trips() {
        let command = ActivationCommand::Snooze {
            note_id: 7,
            reminder_id: 11,
            delivery_at: 123,
        };
        assert_eq!(ActivationCommand::parse(&command.as_uri()), Ok(command));
    }

    #[test]
    fn activation_uri_rejects_other_schemes() {
        assert!(ActivationCommand::parse("https://example.com/open").is_err());
    }
}
