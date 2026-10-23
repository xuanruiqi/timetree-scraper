//! JSON output.

use serde_json::json;

use super::EventPayload;

/// Serialize the raw events as `{"public_events": [...]}`.
pub(super) fn to_json(events: &[EventPayload]) -> serde_json::Result<String> {
    serde_json::to_string_pretty(&json!({ "public_events": events }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::payload;
    use serde_json::Value;

    #[test]
    fn keeps_raw_events() {
        let events = [payload(json!({"z": 1, "a": "予定"}))];
        let out: Value = serde_json::from_str(&to_json(&events).unwrap()).unwrap();
        assert_eq!(out, json!({"public_events": [{"z": 1, "a": "予定"}]}));
    }
}
