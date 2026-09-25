use serde::Deserialize;

pub const MIN_SIZE: u16 = 1;
pub const MAX_SIZE: u16 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resize {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlMessage {
    Resize(Resize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlError {
    Malformed,
    UnknownType,
    InvalidResize,
}

impl ControlError {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "malformed control message",
            Self::UnknownType => "unknown control message",
            Self::InvalidResize => "invalid resize",
        }
    }
}

#[derive(Debug, Deserialize)]
struct RawControl {
    #[serde(rename = "type")]
    kind: String,
    cols: Option<serde_json::Value>,
    rows: Option<serde_json::Value>,
}

fn decode_size(v: Option<&serde_json::Value>) -> Option<u16> {
    let n = match v? {
        serde_json::Value::Number(num) => num.as_u64()?,
        _ => return None,
    };
    if n < u64::from(MIN_SIZE) || n > u64::from(MAX_SIZE) {
        return None;
    }
    u16::try_from(n).ok()
}

/// Parse a text WebSocket payload as a control message.
///
/// Only `{"type":"resize","cols":N,"rows":M}` is accepted, with both dimensions
/// in `1..=1000`. Unknown types and malformed JSON are rejected.
pub fn parse_control(text: &str) -> Result<ControlMessage, ControlError> {
    let raw: RawControl = serde_json::from_str(text).map_err(|_| ControlError::Malformed)?;
    if raw.kind != "resize" {
        return Err(ControlError::UnknownType);
    }
    let cols = decode_size(raw.cols.as_ref()).ok_or(ControlError::InvalidResize)?;
    let rows = decode_size(raw.rows.as_ref()).ok_or(ControlError::InvalidResize)?;
    Ok(ControlMessage::Resize(Resize { cols, rows }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_ok() {
        let msg = parse_control(r#"{"type":"resize","cols":80,"rows":24}"#).unwrap();
        assert_eq!(msg, ControlMessage::Resize(Resize { cols: 80, rows: 24 }));
    }

    #[test]
    fn resize_bounds() {
        assert!(parse_control(r#"{"type":"resize","cols":1,"rows":1}"#).is_ok());
        assert!(parse_control(r#"{"type":"resize","cols":1000,"rows":1000}"#).is_ok());
        assert_eq!(
            parse_control(r#"{"type":"resize","cols":0,"rows":24}"#),
            Err(ControlError::InvalidResize)
        );
        assert_eq!(
            parse_control(r#"{"type":"resize","cols":80,"rows":1001}"#),
            Err(ControlError::InvalidResize)
        );
        assert_eq!(
            parse_control(r#"{"type":"resize","cols":-1,"rows":24}"#),
            Err(ControlError::InvalidResize)
        );
        assert_eq!(
            parse_control(r#"{"type":"resize","cols":80.5,"rows":24}"#),
            Err(ControlError::InvalidResize)
        );
    }

    #[test]
    fn unknown_type() {
        assert_eq!(
            parse_control(r#"{"type":"ioctl","req":1}"#),
            Err(ControlError::UnknownType)
        );
        assert_eq!(
            parse_control(r#"{"type":"ping"}"#),
            Err(ControlError::UnknownType)
        );
    }

    #[test]
    fn malformed() {
        assert_eq!(parse_control("not-json"), Err(ControlError::Malformed));
        assert_eq!(parse_control("[]"), Err(ControlError::Malformed));
        assert_eq!(parse_control("42"), Err(ControlError::Malformed));
        assert_eq!(
            parse_control(r#"{"cols":80,"rows":24}"#),
            Err(ControlError::Malformed)
        );
    }

    #[test]
    fn missing_dims() {
        assert_eq!(
            parse_control(r#"{"type":"resize"}"#),
            Err(ControlError::InvalidResize)
        );
        assert_eq!(
            parse_control(r#"{"type":"resize","cols":80}"#),
            Err(ControlError::InvalidResize)
        );
    }
}
