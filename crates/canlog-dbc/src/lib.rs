//! DBC files: parsing, and decoding/encoding of signals.
//!
//! The parser is tolerant: statements it does not know are skipped, and
//! problems in one statement become warnings instead of failing the file.
//! Real-world DBCs from different tools disagree on details, and losing one
//! odd attribute is better than refusing the whole bus.

mod codec;
mod parse;

pub use codec::{decode_message, decode_signal, encode_message, raw_bits, raw_to_phys, signal_active, DecodedSignal};
pub use parse::{parse, text_from_bytes};

use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Dbc {
    /// `VERSION "..."`, if set to something readable.
    pub version: Option<String>,
    pub messages: Vec<Message>,
    pub nodes: Vec<String>,
    /// `BA_ "Baudrate"` (network attribute), if present.
    pub bitrate: Option<u32>,
    /// `BA_ "BaudRateCANFD"` / `"DataBitrate"`, if present.
    pub data_bitrate: Option<u32>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Message {
    /// CAN id without the extended flag.
    pub id: u32,
    pub extended: bool,
    pub name: String,
    /// Payload length in bytes (DLC field of `BO_`).
    pub size: u8,
    pub sender: String,
    pub signals: Vec<Signal>,
    /// `GenMsgCycleTime` in ms (0 or missing = not periodic).
    pub cycle_ms: Option<f64>,
    /// `GenMsgSendType` as text (`Cyclic`, `Event`, ...), if defined.
    pub send_type: Option<String>,
    /// CAN FD frame (`VFrameFormat` = *_FD).
    pub fd: bool,
    /// CAN FD bit rate switch (`CANFD_BRS`).
    pub brs: bool,
    pub comment: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ValueType {
    Int,
    Float32,
    Float64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Mux {
    /// Always present.
    None,
    /// `M`: selects which multiplexed signals are present.
    Multiplexor,
    /// `m<n>`: present when the multiplexor has value n. With `m<n>M` the
    /// signal is also a multiplexor itself (extended multiplexing).
    Multiplexed { value: u64, also_multiplexor: bool },
}

#[derive(Debug, Clone, Serialize)]
pub struct Signal {
    pub name: String,
    /// As written in the DBC: LSB position for Intel, MSB position
    /// (sawtooth numbering) for Motorola.
    pub start_bit: u16,
    pub size: u16,
    /// Intel (`@1`) when true, Motorola (`@0`) when false.
    pub little_endian: bool,
    pub signed: bool,
    pub value_type: ValueType,
    pub factor: f64,
    pub offset: f64,
    pub min: f64,
    pub max: f64,
    pub unit: String,
    pub receivers: Vec<String>,
    pub mux: Mux,
    /// Extended multiplexing (`SG_MUL_VAL_`): present when signal `switch`
    /// has a value in one of the ranges. Overrides the simple `m<n>` rule.
    pub mux_ext: Vec<MuxRange>,
    /// Value descriptions (`VAL_`), sorted by raw value.
    pub choices: Vec<(i64, String)>,
    pub comment: String,
    /// `GenSigStartValue` (raw), used as the default when encoding.
    pub start_raw: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MuxRange {
    pub switch: String,
    pub ranges: Vec<(u64, u64)>,
}

impl Dbc {
    pub fn message(&self, id: u32, extended: bool) -> Option<&Message> {
        self.messages.iter().find(|m| m.id == id && m.extended == extended)
    }

    pub fn message_by_name(&self, name: &str) -> Option<&Message> {
        self.messages.iter().find(|m| m.name == name)
    }
}

impl Message {
    pub fn signal(&self, name: &str) -> Option<&Signal> {
        self.signals.iter().find(|s| s.name == name)
    }

    /// The simple (`M`) multiplexor, if any.
    pub fn multiplexor(&self) -> Option<&Signal> {
        self.signals.iter().find(|s| s.mux == Mux::Multiplexor)
    }

    pub fn is_periodic(&self) -> bool {
        self.cycle_ms.is_some_and(|c| c > 0.0)
    }
}

impl Signal {
    pub fn choice(&self, raw: i64) -> Option<&str> {
        self.choices.binary_search_by_key(&raw, |c| c.0).ok().map(|i| self.choices[i].1.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Dbc {
        let d = parse(include_str!("../testdata/sample.dbc"));
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
        d
    }

    #[test]
    fn structure() {
        let d = sample();
        assert_eq!(d.version.as_deref(), Some("1.2.3"));
        assert_eq!(parse("VERSION \"HNNBNNNYNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNN/4/%%%/4/'%**4NNN///\"\n").version, None);
        assert_eq!(parse("VERSION \"\"\n").version, None);
        assert_eq!(d.nodes, ["ECU1", "ECU2", "Gateway"]);
        assert_eq!(d.bitrate, Some(500_000));
        assert_eq!(d.messages.len(), 4, "independent-signal pseudo message is dropped");
        let e = d.message(256, false).unwrap();
        assert_eq!(e.cycle_ms, Some(10.0));
        assert_eq!(e.send_type.as_deref(), Some("Cyclic"));
        assert!(!e.fd);
        assert_eq!(e.comment, "Engine data");
        assert_eq!(e.signal("EngineSpeed").unwrap().comment, "Crankshaft\nspeed");
        assert_eq!(e.signal("EngineSpeed").unwrap().receivers, ["Gateway", "ECU2"]);
        assert_eq!(e.signal("Gear").unwrap().choice(15), Some("Invalid"));
        assert_eq!(e.signal("CoolantTemp").unwrap().start_raw, Some(40.0));
        let x = d.message(0x18FEF1FE, true).unwrap();
        assert_eq!(x.cycle_ms, Some(100.0));
        assert_eq!(x.signal("Ratio").unwrap().value_type, ValueType::Float32);
        assert_eq!(x.signal("Voltage").unwrap().mux, Mux::Multiplexed { value: 1, also_multiplexor: false });
        let fd = d.message(512, false).unwrap();
        assert!(fd.fd && fd.brs);
        assert_eq!(fd.size, 64);
        assert_eq!(d.message(768, false).unwrap().send_type.as_deref(), Some("Event"));
        assert_eq!(d.message(768, false).unwrap().signal("Deep").unwrap().mux_ext[0].ranges, [(2, 4), (7, 7)]);
    }

    #[test]
    fn decode() {
        let d = sample();
        let e = d.message(256, false).unwrap();
        // speed 0x1F40 * 0.25 = 2000 rpm, temp raw -20 -> -60, gear (Motorola nibble, MSB bit 31) = 1
        let v = decode_message(e, &[0x40, 0x1F, 0xEC, 0x10, 0, 0, 0, 0]);
        let get = |n: &str| v.iter().find(|s| s.name == n).unwrap().clone();
        assert_eq!(get("EngineSpeed").value, 2000.0);
        assert_eq!(get("CoolantTemp").value, -60.0);
        assert_eq!(get("Gear").value, 1.0);
        assert_eq!(get("Gear").text.as_deref(), Some("First"));

        let x = d.message(0x18FEF1FE, true).unwrap();
        let v = decode_message(x, &[2, 0x9C, 0xFF, 0, 0, 0x80, 0x3F, 0]);
        let names: Vec<_> = v.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Mode", "Current", "Ratio"]);
        assert_eq!(v[1].value, -1.0);
        assert_eq!(v[2].value, 1.0);

        let fd = d.message(512, false).unwrap();
        let mut data = vec![0u8; 64];
        data[0] = 7;
        data[63] = 99;
        let v = decode_message(fd, &data);
        assert_eq!(v[0].value, 7.0);
        assert_eq!(v[1].value, 99.0);
    }

    #[test]
    fn extended_mux_and_encode() {
        let d = sample();
        let m = d.message(768, false).unwrap();
        let names = |data: &[u8]| decode_message(m, data).into_iter().map(|s| s.name).collect::<Vec<_>>();
        assert_eq!(names(&[0x33, 9]), ["Sel", "Sub", "Deep"]);
        assert_eq!(names(&[0x53, 9]), ["Sel", "Sub"]);
        assert_eq!(names(&[0x31, 9]), ["Sel"]);
        let enc = encode_message(m, &[("Sel".into(), 3.0), ("Sub".into(), 7.0), ("Deep".into(), 200.0)]);
        assert_eq!(enc, [0x73, 200, 0, 0, 0, 0, 0, 0]);
        // Deep is not active for Sub=5: stays 0
        let enc = encode_message(m, &[("Sel".into(), 3.0), ("Sub".into(), 5.0), ("Deep".into(), 200.0)]);
        assert_eq!(enc[1], 0);

        let e = d.message(256, false).unwrap();
        let enc = encode_message(e, &[("EngineSpeed".into(), 2000.0)]);
        // CoolantTemp takes its start value (raw 40)
        assert_eq!(&enc[..3], &[0x40, 0x1F, 40]);
    }

    #[test]
    fn tolerant() {
        let d = parse("BO_ 1 A: 8 X\n SG_ bad : x|8@1+ (1,0) [0|0] \"\" X\n SG_ ok : 0|8@1+ (1,0) [0|0] \"\" X\nWHATEVER foo bar;\nBO_ 2 B: 8 X\n");
        assert_eq!(d.messages.len(), 2);
        assert_eq!(d.messages[0].signals.len(), 1);
        assert_eq!(d.warnings.len(), 1);
        let t = text_from_bytes(b"CM_ \"temp\xe9rature\";");
        assert!(t.contains('\u{e9}'));
    }
}
