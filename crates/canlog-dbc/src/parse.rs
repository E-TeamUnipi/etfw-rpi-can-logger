//! Tolerant DBC parser: a small lexer plus one function per statement.

use crate::{Dbc, Message, Mux, MuxRange, Signal, ValueType};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Num(f64, String),
    Str(String),
    Punct(char),
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    line: u32,
    /// First token on its line (statements start at line starts).
    bol: bool,
}

fn lex(src: &str) -> Vec<Token> {
    let b: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line, mut bol) = (0usize, 1u32, true);
    while i < b.len() {
        let c = b[i];
        if c == '\n' {
            line += 1;
            bol = true;
            i += 1;
            continue;
        }
        if c.is_whitespace() || c == '\u{feff}' {
            i += 1;
            continue;
        }
        if c == '/' && b.get(i + 1) == Some(&'/') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        let start_line = line;
        let tok = if c == '"' {
            i += 1;
            let mut s = String::new();
            while i < b.len() && b[i] != '"' {
                if b[i] == '\\' && i + 1 < b.len() {
                    i += 1;
                }
                if b[i] == '\n' {
                    line += 1;
                }
                s.push(b[i]);
                i += 1;
            }
            i += 1;
            Tok::Str(s)
        } else if c.is_ascii_alphabetic() || c == '_' {
            let st = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            Tok::Ident(b[st..i].iter().collect())
        } else if c.is_ascii_digit()
            || ((c == '-' || c == '+' || c == '.')
                && b.get(i + 1).is_some_and(|n| n.is_ascii_digit() || (*n == '.' && c != '.')))
        {
            let st = i;
            i += 1;
            while i < b.len() {
                let d = b[i];
                if d.is_ascii_digit() || d == '.' {
                    i += 1;
                } else if (d == 'e' || d == 'E') && b.get(i + 1).is_some_and(|n| n.is_ascii_digit() || *n == '-' || *n == '+') {
                    i += 2;
                } else {
                    break;
                }
            }
            let text: String = b[st..i].iter().collect();
            match text.parse::<f64>() {
                Ok(v) => Tok::Num(v, text),
                Err(_) => Tok::Punct(c),
            }
        } else {
            i += 1;
            Tok::Punct(c)
        };
        out.push(Token { tok, line: start_line, bol });
        bol = false;
    }
    out
}

const KEYWORDS: &[&str] = &[
    "VERSION", "NS_", "BS_", "BU_", "BO_", "SG_", "CM_", "BA_DEF_", "BA_DEF_DEF_", "BA_", "VAL_", "VAL_TABLE_",
    "SIG_VALTYPE_", "SG_MUL_VAL_", "BO_TX_BU_", "EV_", "ENVVAR_DATA_", "SIG_GROUP_", "BA_DEF_REL_", "BA_REL_",
    "BA_DEF_DEF_REL_", "BU_SG_REL_", "BU_EV_REL_", "BU_BO_REL_", "SGTYPE_", "SGTYPE_VAL_", "BA_DEF_SGTYPE_",
    "BA_SGTYPE_", "SIG_TYPE_REF_", "SIGTYPE_VALTYPE_", "CAT_DEF_", "CAT_", "FILTER", "EV_DATA_", "ENVVAR_DATA_",
];

#[derive(Debug, Clone)]
enum AttrVal {
    Num(f64),
    Str(String),
}

#[derive(Default)]
struct Attrs {
    enums: HashMap<String, Vec<String>>,
    defaults: HashMap<String, AttrVal>,
    network: HashMap<String, AttrVal>,
    msg: HashMap<u32, HashMap<String, AttrVal>>,
    sig: HashMap<(u32, String), HashMap<String, AttrVal>>,
}

impl Attrs {
    /// Attribute value as text (enum indices resolved to their names).
    fn text(&self, name: &str, v: &AttrVal) -> String {
        match v {
            AttrVal::Str(s) => s.clone(),
            AttrVal::Num(n) => match self.enums.get(name) {
                Some(list) => list.get(*n as usize).cloned().unwrap_or_else(|| n.to_string()),
                None => n.to_string(),
            },
        }
    }
    fn num(&self, name: &str, v: &AttrVal) -> Option<f64> {
        match v {
            AttrVal::Num(n) => Some(*n),
            AttrVal::Str(_) => self.text(name, v).trim().parse().ok(),
        }
    }
    fn lookup<'a>(&'a self, specific: Option<&'a HashMap<String, AttrVal>>, name: &str) -> Option<&'a AttrVal> {
        specific.and_then(|m| m.get(name)).or_else(|| self.defaults.get(name))
    }
}

struct P {
    t: Vec<Token>,
    i: usize,
}

type R<T> = Result<T, String>;

impl P {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i).map(|t| &t.tok)
    }
    fn next(&mut self) -> R<Tok> {
        let t = self.t.get(self.i).map(|t| t.tok.clone()).ok_or("unexpected end of file")?;
        self.i += 1;
        Ok(t)
    }
    fn at_bol(&self) -> bool {
        self.t.get(self.i).is_none_or(|t| t.bol)
    }
    fn punct(&mut self, c: char) -> R<()> {
        match self.next()? {
            Tok::Punct(p) if p == c => Ok(()),
            t => Err(format!("expected '{c}', found {t:?}")),
        }
    }
    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(&Tok::Punct(c)) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn ident(&mut self) -> R<String> {
        match self.next()? {
            Tok::Ident(s) => Ok(s),
            t => Err(format!("expected a name, found {t:?}")),
        }
    }
    fn num(&mut self) -> R<f64> {
        match self.next()? {
            Tok::Num(v, _) => Ok(v),
            t => Err(format!("expected a number, found {t:?}")),
        }
    }
    fn uint(&mut self) -> R<u64> {
        match self.next()? {
            Tok::Num(_, s) => s.parse::<u64>().map_err(|_| format!("expected an integer, found {s}")),
            t => Err(format!("expected an integer, found {t:?}")),
        }
    }
    fn string(&mut self) -> R<String> {
        match self.next()? {
            Tok::Str(s) => Ok(s),
            t => Err(format!("expected a string, found {t:?}")),
        }
    }
    fn value(&mut self) -> R<AttrVal> {
        match self.next()? {
            Tok::Num(v, _) => Ok(AttrVal::Num(v)),
            Tok::Str(s) => Ok(AttrVal::Str(s)),
            Tok::Ident(s) => Ok(AttrVal::Str(s)),
            t => Err(format!("expected a value, found {t:?}")),
        }
    }
    fn is_kw(&self, i: usize) -> bool {
        matches!(&self.t[i].tok, Tok::Ident(s) if KEYWORDS.contains(&s.as_str()))
    }
    /// Skip to the next statement (a keyword at the start of a line).
    fn skip_statement(&mut self) {
        self.i += 1;
        while self.i < self.t.len() && !(self.t[self.i].bol && self.is_kw(self.i)) {
            self.i += 1;
        }
    }
    /// Skip to just after the next ';' (or to the next statement).
    fn skip_semicolon(&mut self) {
        while self.i < self.t.len() {
            if self.t[self.i].tok == Tok::Punct(';') {
                self.i += 1;
                return;
            }
            if self.t[self.i].bol && self.is_kw(self.i) {
                return;
            }
            self.i += 1;
        }
    }
}

/// `BO_` ids carry the extended flag in bit 31.
const EXT_FLAG: u32 = 0x8000_0000;
/// Pseudo message Vector tools use for signals not in any frame.
const INDEPENDENT_SIG_MSG: u32 = 0xC000_0000;

/// Parse DBC text. Never fails: unreadable statements become warnings.
/// CANdb++ fills an empty VERSION with an internal flag string
/// ("HNNBNNNYNN.../4/%%%/4/'%**4NNN///"): treat that as no version.
fn readable_version(v: &str) -> Option<String> {
    let v = v.trim();
    let junk = v.contains("%%%") || (v.len() > 20 && v.bytes().take(10).all(|b| matches!(b, b'H' | b'N' | b'Y' | b'B')));
    (!v.is_empty() && !junk).then(|| v.to_string())
}

pub fn parse(src: &str) -> Dbc {
    let mut p = P { t: lex(src), i: 0 };
    let mut dbc = Dbc::default();
    let mut attrs = Attrs::default();
    // raw id (with EXT_FLAG) -> index in dbc.messages
    let mut by_raw: HashMap<u32, usize> = HashMap::new();
    let mut current: Option<usize> = None;
    let mut sig_comments: Vec<(u32, String, String)> = Vec::new();
    let mut msg_comments: Vec<(u32, String)> = Vec::new();
    let mut choices: Vec<(u32, String, Vec<(i64, String)>)> = Vec::new();
    let mut valtypes: Vec<(u32, String, u64)> = Vec::new();
    let mut mux_ext: Vec<(u32, String, MuxRange)> = Vec::new();

    while p.i < p.t.len() {
        let start = p.i;
        let line = p.t[start].line;
        let kw = match &p.t[start].tok {
            Tok::Ident(s) => s.clone(),
            _ => {
                p.skip_statement();
                continue;
            }
        };
        p.i += 1;
        let res: R<()> = match kw.as_str() {
            "VERSION" => (|| {
                if let Tok::Str(v) = p.next()? {
                    dbc.version = readable_version(&v);
                }
                Ok(())
            })(),
            "NS_" => {
                // the NS_ block lists keywords on their own lines: skip to BS_ or BU_
                while p.i < p.t.len() && !matches!(&p.t[p.i].tok, Tok::Ident(s) if p.t[p.i].bol && (s == "BS_" || s == "BU_" || s == "BO_")) {
                    p.i += 1;
                }
                continue;
            }
            "BU_" => (|| {
                p.punct(':')?;
                while !p.at_bol() {
                    if let Tok::Ident(n) = p.next()? {
                        dbc.nodes.push(n);
                    }
                }
                Ok(())
            })(),
            "BO_" => (|| {
                let raw = p.uint()? as u32;
                let name = p.ident()?;
                p.punct(':')?;
                let size = p.uint()?;
                let sender = if p.at_bol() { String::new() } else { p.ident()? };
                if raw == INDEPENDENT_SIG_MSG {
                    current = None;
                    return Ok(());
                }
                let msg = Message {
                    id: raw & !EXT_FLAG,
                    extended: raw & EXT_FLAG != 0,
                    name,
                    size: size.min(64) as u8,
                    sender,
                    ..Default::default()
                };
                if by_raw.contains_key(&raw) {
                    dbc.warnings.push(format!("line {line}: message id {:#X} defined twice, keeping the first", raw & !EXT_FLAG));
                    current = None;
                    return Ok(());
                }
                by_raw.insert(raw, dbc.messages.len());
                current = Some(dbc.messages.len());
                dbc.messages.push(msg);
                Ok(())
            })(),
            "SG_" => (|| {
                let sig = parse_signal(&mut p)?;
                if let Some(m) = current {
                    dbc.messages[m].signals.push(sig);
                }
                Ok(())
            })(),
            "CM_" => (|| {
                match p.next()? {
                    Tok::Str(_) => {}
                    Tok::Ident(k) if k == "BO_" => {
                        let raw = p.uint()? as u32;
                        msg_comments.push((raw, p.string()?));
                    }
                    Tok::Ident(k) if k == "SG_" => {
                        let raw = p.uint()? as u32;
                        let s = p.ident()?;
                        sig_comments.push((raw, s, p.string()?));
                    }
                    Tok::Ident(_) => {
                        p.next()?;
                        p.string()?;
                    }
                    t => return Err(format!("unexpected {t:?}")),
                }
                p.skip_semicolon();
                Ok(())
            })(),
            "BA_DEF_" => (|| {
                if let Some(Tok::Ident(_)) = p.peek() {
                    p.next()?; // object type
                }
                let name = p.string()?;
                let typ = p.ident()?;
                if typ == "ENUM" {
                    let mut list = Vec::new();
                    loop {
                        match p.next()? {
                            Tok::Str(s) => list.push(s),
                            Tok::Punct(',') => {}
                            Tok::Punct(';') => break,
                            t => return Err(format!("unexpected {t:?} in enum")),
                        }
                    }
                    attrs.enums.insert(name, list);
                } else {
                    p.skip_semicolon();
                }
                Ok(())
            })(),
            "BA_DEF_DEF_" => (|| {
                let name = p.string()?;
                let v = p.value()?;
                attrs.defaults.insert(name, v);
                p.skip_semicolon();
                Ok(())
            })(),
            "BA_" => (|| {
                let name = p.string()?;
                match p.peek() {
                    Some(Tok::Ident(k)) if k == "BO_" => {
                        p.next()?;
                        let raw = p.uint()? as u32;
                        let v = p.value()?;
                        attrs.msg.entry(raw).or_default().insert(name, v);
                    }
                    Some(Tok::Ident(k)) if k == "SG_" => {
                        p.next()?;
                        let raw = p.uint()? as u32;
                        let s = p.ident()?;
                        let v = p.value()?;
                        attrs.sig.entry((raw, s)).or_default().insert(name, v);
                    }
                    Some(Tok::Ident(k)) if k == "BU_" || k == "EV_" => {}
                    _ => {
                        let v = p.value()?;
                        attrs.network.insert(name, v);
                    }
                }
                p.skip_semicolon();
                Ok(())
            })(),
            "VAL_" => (|| {
                let raw = match p.next()? {
                    Tok::Num(_, s) => s.parse::<u64>().map_err(|e| e.to_string())? as u32,
                    // value table of an environment variable
                    _ => {
                        p.skip_semicolon();
                        return Ok(());
                    }
                };
                let s = p.ident()?;
                let mut list = Vec::new();
                loop {
                    match p.next()? {
                        Tok::Num(v, _) => list.push((v as i64, p.string()?)),
                        Tok::Punct(';') => break,
                        t => return Err(format!("unexpected {t:?} in VAL_")),
                    }
                }
                choices.push((raw, s, list));
                Ok(())
            })(),
            "SIG_VALTYPE_" => (|| {
                let raw = p.uint()? as u32;
                let s = p.ident()?;
                p.eat(':');
                valtypes.push((raw, s, p.uint()?));
                p.skip_semicolon();
                Ok(())
            })(),
            "SG_MUL_VAL_" => (|| {
                let raw = p.uint()? as u32;
                let s = p.ident()?;
                let switch = p.ident()?;
                let mut ranges = Vec::new();
                loop {
                    match p.next()? {
                        Tok::Num(lo, _) => {
                            // "3-5" lexes as 3 and -5
                            let hi = match p.next()? {
                                Tok::Num(h, _) if h <= 0.0 => -h,
                                Tok::Punct('-') => p.num()?,
                                t => return Err(format!("bad range end {t:?}")),
                            };
                            ranges.push((lo as u64, hi as u64));
                        }
                        Tok::Punct(',') => {}
                        Tok::Punct(';') => break,
                        t => return Err(format!("unexpected {t:?} in SG_MUL_VAL_")),
                    }
                }
                mux_ext.push((raw, s, MuxRange { switch, ranges }));
                Ok(())
            })(),
            _ => {
                p.i = start;
                p.skip_statement();
                continue;
            }
        };
        if let Err(e) = res {
            dbc.warnings.push(format!("line {line}: {kw}: {e}"));
            p.i = start;
            p.skip_statement();
        }
    }

    // ---- apply the collected extras
    let find_sig = |dbc: &mut Dbc, raw: u32, s: &str| -> Option<(usize, usize)> {
        let m = *by_raw.get(&raw)?;
        let si = dbc.messages[m].signals.iter().position(|x| x.name == s)?;
        Some((m, si))
    };
    for (raw, c) in msg_comments {
        if let Some(&m) = by_raw.get(&raw) {
            dbc.messages[m].comment = c;
        }
    }
    for (raw, s, c) in sig_comments {
        if let Some((m, si)) = find_sig(&mut dbc, raw, &s) {
            dbc.messages[m].signals[si].comment = c;
        }
    }
    for (raw, s, mut list) in choices {
        if let Some((m, si)) = find_sig(&mut dbc, raw, &s) {
            list.sort_by_key(|c| c.0);
            list.dedup_by_key(|c| c.0);
            dbc.messages[m].signals[si].choices = list;
        }
    }
    for (raw, s, t) in valtypes {
        if let Some((m, si)) = find_sig(&mut dbc, raw, &s) {
            dbc.messages[m].signals[si].value_type = match t {
                1 => ValueType::Float32,
                2 => ValueType::Float64,
                _ => ValueType::Int,
            };
        }
    }
    for (raw, s, r) in mux_ext {
        if let Some((m, si)) = find_sig(&mut dbc, raw, &s) {
            dbc.messages[m].signals[si].mux_ext.push(r);
        }
    }

    for (raw, &m) in &by_raw {
        let spec = attrs.msg.get(raw);
        let msg = &mut dbc.messages[m];
        if let Some(v) = attrs.lookup(spec, "GenMsgCycleTime") {
            msg.cycle_ms = attrs.num("GenMsgCycleTime", v).filter(|c| *c > 0.0);
        }
        if let Some(v) = attrs.lookup(spec, "GenMsgSendType") {
            msg.send_type = Some(attrs.text("GenMsgSendType", v));
        }
        if let Some(v) = attrs.lookup(spec, "VFrameFormat") {
            msg.fd = attrs.text("VFrameFormat", v).to_ascii_uppercase().contains("FD");
        }
        if msg.size > 8 {
            msg.fd = true;
        }
        if msg.fd {
            msg.brs = match attrs.lookup(spec, "CANFD_BRS") {
                Some(v) => attrs.num("CANFD_BRS", v).unwrap_or(1.0) != 0.0,
                None => true,
            };
        }
        for s in &mut msg.signals {
            let sa = attrs.sig.get(&(*raw, s.name.clone()));
            if let Some(v) = attrs.lookup(sa, "GenSigStartValue") {
                s.start_raw = attrs.num("GenSigStartValue", v);
            }
        }
    }
    let net_num = |names: &[&str]| -> Option<u32> {
        for (k, v) in &attrs.network {
            if names.iter().any(|n| n.eq_ignore_ascii_case(k)) {
                if let Some(x) = attrs.num(k, v).filter(|x| *x > 0.0) {
                    // some tools store kbit/s
                    return Some(if x < 10_000.0 { (x * 1000.0) as u32 } else { x as u32 });
                }
            }
        }
        None
    };
    dbc.bitrate = net_num(&["Baudrate", "BusSpeed"]);
    dbc.data_bitrate = net_num(&["BaudRateCANFD", "BaudrateCANFD", "DataBaudrate", "DataBitrate"]);
    dbc.messages.sort_by_key(|m| (m.extended, m.id));
    dbc
}

fn parse_signal(p: &mut P) -> R<Signal> {
    let name = p.ident()?;
    let mut mux = Mux::None;
    if let Some(Tok::Ident(m)) = p.peek() {
        let m = m.clone();
        p.next()?;
        if m == "M" {
            mux = Mux::Multiplexor;
        } else if let Some(rest) = m.strip_prefix('m') {
            let also = rest.ends_with('M');
            let n = rest.trim_end_matches('M').parse::<u64>().map_err(|_| format!("bad multiplexer '{m}'"))?;
            mux = Mux::Multiplexed { value: n, also_multiplexor: also };
        } else {
            return Err(format!("bad multiplexer '{m}'"));
        }
    }
    p.punct(':')?;
    let start_bit = p.uint()? as u16;
    p.punct('|')?;
    let size = p.uint()? as u16;
    p.punct('@')?;
    // "1+" may lex as the number "1" followed by '+', or "1" "+"
    let order = p.uint()?;
    let signed = match p.next()? {
        Tok::Punct('-') => true,
        Tok::Punct('+') => false,
        t => return Err(format!("expected + or -, found {t:?}")),
    };
    p.punct('(')?;
    let factor = p.num()?;
    p.punct(',')?;
    let offset = p.num()?;
    p.punct(')')?;
    p.punct('[')?;
    let min = p.num()?;
    p.punct('|')?;
    let max = p.num()?;
    p.punct(']')?;
    let unit = p.string()?;
    let mut receivers = Vec::new();
    while !p.at_bol() {
        if let Tok::Ident(r) = p.next()? {
            receivers.push(r);
        }
    }
    if size == 0 || size > 64 {
        return Err(format!("signal {name}: size {size} out of range"));
    }
    Ok(Signal {
        name,
        start_bit,
        size,
        little_endian: order == 1,
        signed,
        value_type: ValueType::Int,
        factor,
        offset,
        min,
        max,
        unit,
        receivers,
        mux,
        mux_ext: Vec::new(),
        choices: Vec::new(),
        comment: String::new(),
        start_raw: None,
    })
}

/// Decode DBC file bytes: UTF-8, or Windows-1252/Latin-1 (common for DBCs).
pub fn text_from_bytes(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_string(),
        Err(_) => b.iter().map(|&c| c as char).collect(),
    }
}
