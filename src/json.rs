//! Minimaler JSON-Bauer/-Leser (keine externen Crates, damit der Build offline läuft).

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    pub fn obj(pairs: Vec<(&str, Value)>) -> Value {
        Value::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
    pub fn num(v: f64) -> Value {
        Value::Num(v)
    }
    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(s.into())
    }
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(p) => p.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Num(n) => Some(*n),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Value::Str(s) => s.parse().ok(),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::Num(n) => Some(*n != 0.0),
            _ => None,
        }
    }
    pub fn as_arr(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Arr(a) => Some(a),
            _ => None,
        }
    }

    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Num(n) => {
                if !n.is_finite() {
                    out.push('0');
                } else if n.fract() == 0.0 && n.abs() < 1e15 {
                    out.push_str(&format!("{}", *n as i64));
                } else {
                    out.push_str(&format!("{n:.3}"));
                }
            }
            Value::Str(s) => out.push_str(&quote(s)),
            Value::Arr(a) => {
                out.push('[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Value::Obj(p) => {
                out.push('{');
                for (i, (k, v)) in p.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&quote(k));
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

pub fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn parse(s: &str) -> Result<Value, String> {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let v = value(&b, &mut i)?;
    ws(&b, &mut i);
    if i != b.len() {
        return Err(format!("Rest ab {i}"));
    }
    Ok(v)
}

fn ws(b: &[char], i: &mut usize) {
    while *i < b.len() && b[*i].is_whitespace() {
        *i += 1;
    }
}

fn value(b: &[char], i: &mut usize) -> Result<Value, String> {
    ws(b, i);
    match b.get(*i) {
        None => Err("Ende".into()),
        Some('{') => {
            *i += 1;
            let mut pairs = Vec::new();
            ws(b, i);
            if b.get(*i) == Some(&'}') {
                *i += 1;
                return Ok(Value::Obj(pairs));
            }
            loop {
                ws(b, i);
                let k = string(b, i)?;
                ws(b, i);
                if b.get(*i) != Some(&':') {
                    return Err("':' erwartet".into());
                }
                *i += 1;
                pairs.push((k, value(b, i)?));
                ws(b, i);
                match b.get(*i) {
                    Some(',') => *i += 1,
                    Some('}') => {
                        *i += 1;
                        return Ok(Value::Obj(pairs));
                    }
                    _ => return Err("',' oder '}' erwartet".into()),
                }
            }
        }
        Some('[') => {
            *i += 1;
            let mut items = Vec::new();
            ws(b, i);
            if b.get(*i) == Some(&']') {
                *i += 1;
                return Ok(Value::Arr(items));
            }
            loop {
                items.push(value(b, i)?);
                ws(b, i);
                match b.get(*i) {
                    Some(',') => *i += 1,
                    Some(']') => {
                        *i += 1;
                        return Ok(Value::Arr(items));
                    }
                    _ => return Err("',' oder ']' erwartet".into()),
                }
            }
        }
        Some('"') => Ok(Value::Str(string(b, i)?)),
        Some('t') => lit(b, i, "true", Value::Bool(true)),
        Some('f') => lit(b, i, "false", Value::Bool(false)),
        Some('n') => lit(b, i, "null", Value::Null),
        Some(_) => {
            let start = *i;
            while *i < b.len() && (b[*i].is_ascii_digit() || "+-.eE".contains(b[*i])) {
                *i += 1;
            }
            let s: String = b[start..*i].iter().collect();
            s.parse::<f64>().map(Value::Num).map_err(|_| format!("Zahl: {s}"))
        }
    }
}

fn lit(b: &[char], i: &mut usize, word: &str, v: Value) -> Result<Value, String> {
    let w: Vec<char> = word.chars().collect();
    if b.len() >= *i + w.len() && b[*i..*i + w.len()] == w[..] {
        *i += w.len();
        Ok(v)
    } else {
        Err(format!("{word} erwartet"))
    }
}

fn string(b: &[char], i: &mut usize) -> Result<String, String> {
    if b.get(*i) != Some(&'"') {
        return Err("'\"' erwartet".into());
    }
    *i += 1;
    let mut out = String::new();
    while *i < b.len() {
        match b[*i] {
            '"' => {
                *i += 1;
                return Ok(out);
            }
            '\\' => {
                *i += 1;
                match b.get(*i) {
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('u') => {
                        let hex: String = b.get(*i + 1..*i + 5).unwrap_or(&[]).iter().collect();
                        if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                            out.push(c);
                        }
                        *i += 4;
                    }
                    Some(c) => out.push(*c),
                    None => return Err("Escape".into()),
                }
                *i += 1;
            }
            c => {
                out.push(c);
                *i += 1;
            }
        }
    }
    Err("String offen".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rundlauf() {
        let v = Value::obj(vec![
            ("a", Value::num(1.5)),
            ("b", Value::Arr(vec![Value::Bool(true), Value::str("x")])),
        ]);
        let text = v.to_json();
        let back = parse(&text).unwrap();
        assert_eq!(back.get("a").unwrap().as_f64(), Some(1.5));
        assert_eq!(back.get("b").unwrap().as_arr().unwrap().len(), 2);
    }

    #[test]
    fn zahlen_und_bools() {
        let v = parse(r#"{"gain":-12.5,"mute":true}"#).unwrap();
        assert_eq!(v.get("gain").unwrap().as_f64(), Some(-12.5));
        assert_eq!(v.get("mute").unwrap().as_bool(), Some(true));
    }
}
