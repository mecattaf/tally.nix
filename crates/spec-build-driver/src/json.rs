use std::collections::BTreeMap;

use crate::error::{DriverError, Result};

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

impl Json {
    pub(crate) fn as_object(&self) -> Option<&BTreeMap<String, Json>> {
        match self {
            Self::Object(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn as_array(&self) -> Option<&[Json]> {
        match self {
            Self::Array(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub(crate) fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(value)
                if !value.starts_with('-')
                    && !value.contains('.')
                    && !value.contains('e')
                    && !value.contains('E') =>
            {
                value.parse().ok()
            }
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        self.as_object()?.get(key)
    }

    pub(crate) fn object(entries: impl IntoIterator<Item = (impl Into<String>, Json)>) -> Self {
        Self::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key.into(), value))
                .collect(),
        )
    }

    pub(crate) fn stringify(&self) -> String {
        let mut output = String::new();
        self.write_to(&mut output);
        output
    }

    fn write_to(&self, output: &mut String) {
        match self {
            Self::Null => output.push_str("null"),
            Self::Bool(true) => output.push_str("true"),
            Self::Bool(false) => output.push_str("false"),
            Self::Number(value) => output.push_str(value),
            Self::String(value) => write_string(value, output),
            Self::Array(values) => {
                output.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index != 0 {
                        output.push(',');
                    }
                    value.write_to(output);
                }
                output.push(']');
            }
            Self::Object(values) => {
                output.push('{');
                for (index, (key, value)) in values.iter().enumerate() {
                    if index != 0 {
                        output.push(',');
                    }
                    write_string(key, output);
                    output.push(':');
                    value.write_to(output);
                }
                output.push('}');
            }
        }
    }
}

impl From<&str> for Json {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl From<String> for Json {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<bool> for Json {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<usize> for Json {
    fn from(value: usize) -> Self {
        Self::Number(value.to_string())
    }
}

pub(crate) fn parse(input: &str) -> Result<Json> {
    Parser::new(input).parse()
}

fn write_string(value: &str, output: &mut String) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character <= '\u{1f}' => {
                output.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => output.push(character),
        }
    }
    output.push('"');
}

struct Parser<'a> {
    input: &'a [u8],
    cursor: usize,
}

impl<'a> Parser<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input: input.as_bytes(),
            cursor: 0,
        }
    }

    fn parse(mut self) -> Result<Json> {
        self.whitespace();
        let value = self.value()?;
        self.whitespace();
        if self.cursor != self.input.len() {
            return self.error("trailing content");
        }
        Ok(value)
    }

    fn value(&mut self) -> Result<Json> {
        match self.peek() {
            Some(b'n') => {
                self.literal(b"null")?;
                Ok(Json::Null)
            }
            Some(b't') => {
                self.literal(b"true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.literal(b"false")?;
                Ok(Json::Bool(false))
            }
            Some(b'"') => self.string().map(Json::String),
            Some(b'[') => self.array(),
            Some(b'{') => self.object(),
            Some(b'-' | b'0'..=b'9') => self.number().map(Json::Number),
            Some(_) => self.error("expected a JSON value"),
            None => self.error("unexpected end of input"),
        }
    }

    fn array(&mut self) -> Result<Json> {
        self.cursor += 1;
        self.whitespace();
        let mut values = Vec::new();
        if self.take(b']') {
            return Ok(Json::Array(values));
        }
        loop {
            self.whitespace();
            values.push(self.value()?);
            self.whitespace();
            if self.take(b']') {
                break;
            }
            if !self.take(b',') {
                return self.error("expected ',' or ']' in array");
            }
        }
        Ok(Json::Array(values))
    }

    fn object(&mut self) -> Result<Json> {
        self.cursor += 1;
        self.whitespace();
        let mut values = BTreeMap::new();
        if self.take(b'}') {
            return Ok(Json::Object(values));
        }
        loop {
            self.whitespace();
            if self.peek() != Some(b'"') {
                return self.error("expected an object key");
            }
            let key = self.string()?;
            self.whitespace();
            if !self.take(b':') {
                return self.error("expected ':' after object key");
            }
            self.whitespace();
            values.insert(key, self.value()?);
            self.whitespace();
            if self.take(b'}') {
                break;
            }
            if !self.take(b',') {
                return self.error("expected ',' or '}' in object");
            }
        }
        Ok(Json::Object(values))
    }

    fn string(&mut self) -> Result<String> {
        debug_assert_eq!(self.peek(), Some(b'"'));
        self.cursor += 1;
        let mut bytes = Vec::new();
        loop {
            let byte = match self.peek() {
                Some(byte) => byte,
                None => return self.error("unterminated string"),
            };
            self.cursor += 1;
            match byte {
                b'"' => {
                    return String::from_utf8(bytes)
                        .map_err(|_| DriverError::new("invalid UTF-8 in JSON string"));
                }
                b'\\' => self.escape(&mut bytes)?,
                0..=0x1f => return self.error("control character in string"),
                byte => bytes.push(byte),
            }
        }
    }

    fn escape(&mut self, output: &mut Vec<u8>) -> Result<()> {
        let escaped = match self.peek() {
            Some(byte) => byte,
            None => return self.error("unterminated escape"),
        };
        self.cursor += 1;
        match escaped {
            b'"' | b'\\' | b'/' => output.push(escaped),
            b'b' => output.push(0x08),
            b'f' => output.push(0x0c),
            b'n' => output.push(b'\n'),
            b'r' => output.push(b'\r'),
            b't' => output.push(b'\t'),
            b'u' => {
                let first = self.hex_quad()?;
                let scalar = if (0xd800..=0xdbff).contains(&first) {
                    if !self.take(b'\\') || !self.take(b'u') {
                        return self.error("high surrogate without a low surrogate");
                    }
                    let second = self.hex_quad()?;
                    if !(0xdc00..=0xdfff).contains(&second) {
                        return self.error("invalid low surrogate");
                    }
                    0x10000 + (((first - 0xd800) as u32) << 10) + (second - 0xdc00) as u32
                } else if (0xdc00..=0xdfff).contains(&first) {
                    return self.error("low surrogate without a high surrogate");
                } else {
                    first as u32
                };
                let character = char::from_u32(scalar)
                    .ok_or_else(|| DriverError::new("invalid Unicode scalar in JSON string"))?;
                let mut encoded = [0_u8; 4];
                output.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            }
            _ => return self.error("invalid string escape"),
        }
        Ok(())
    }

    fn hex_quad(&mut self) -> Result<u16> {
        if self.cursor + 4 > self.input.len() {
            return self.error("incomplete Unicode escape");
        }
        let mut value = 0_u16;
        for _ in 0..4 {
            let digit = self.input[self.cursor];
            self.cursor += 1;
            value = value
                .checked_mul(16)
                .and_then(|value| {
                    let digit = match digit {
                        b'0'..=b'9' => (digit - b'0') as u16,
                        b'a'..=b'f' => (digit - b'a' + 10) as u16,
                        b'A'..=b'F' => (digit - b'A' + 10) as u16,
                        _ => return None,
                    };
                    value.checked_add(digit)
                })
                .ok_or_else(|| DriverError::new("invalid Unicode escape"))?;
        }
        Ok(value)
    }

    fn number(&mut self) -> Result<String> {
        let start = self.cursor;
        self.take(b'-');
        match self.peek() {
            Some(b'0') => {
                self.cursor += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return self.error("leading zero in number");
                }
            }
            Some(b'1'..=b'9') => {
                self.cursor += 1;
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.cursor += 1;
                }
            }
            _ => return self.error("invalid number"),
        }
        if self.take(b'.') {
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return self.error("fraction has no digits");
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.cursor += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.cursor += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.cursor += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return self.error("exponent has no digits");
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.cursor += 1;
            }
        }
        String::from_utf8(self.input[start..self.cursor].to_vec())
            .map_err(|_| DriverError::new("invalid number encoding"))
    }

    fn literal(&mut self, literal: &[u8]) -> Result<()> {
        if self.input.get(self.cursor..self.cursor + literal.len()) != Some(literal) {
            return self.error("invalid literal");
        }
        self.cursor += literal.len();
        Ok(())
    }

    fn whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.cursor += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.cursor).copied()
    }

    fn take(&mut self, expected: u8) -> bool {
        if self.peek() == Some(expected) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn error<T>(&self, message: &str) -> Result<T> {
        Err(DriverError::new(format!(
            "invalid JSON at byte {}: {message}",
            self.cursor
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, Json};

    #[test]
    fn round_trips_the_brief_shapes_the_driver_consumes() {
        let source =
            r#"{"array":[true,false,null,-12,3.5e2],"escaped":"line\n\ud83d\ude80","text":"é"}"#;
        let parsed = parse(source).unwrap();
        assert_eq!(parse(&parsed.stringify()).unwrap(), parsed);
        assert_eq!(
            parsed.get("escaped").and_then(Json::as_str),
            Some("line\n🚀")
        );
    }

    #[test]
    fn rejects_trailing_or_malformed_input() {
        assert!(parse("{} garbage").is_err());
        assert!(parse(r#""\ud800""#).is_err());
        assert!(parse("[1,]").is_err());
    }
}
