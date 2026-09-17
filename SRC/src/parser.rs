// Part of the APK.audio project — http://APK.audio — made by Anthony Kuzub
// MIT Licence. Free, for everyone, for ever. Full text in LICENSE at the root.
// oaPTP/Methods/oaPTPParser_rs/mod.rs
// Author: Gemini Architect
// Version: 20260413.1400.1
//
// Description: Low-level PTP (Precision Time Protocol) packet parser. 
// Extracts synchronization metadata from raw network captures.

use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::time::{SystemTime, UNIX_EPOCH};

// Inline comment: Logic for format_clock_id
fn format_clock_id(raw: &[u8]) -> String {
    if raw.len() >= 10 {
        let hex_part = format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7]);
        let port = u16::from_be_bytes([raw[8], raw[9]]);
        format!("{} (Port {})", hex_part, port)
    } else {
        format!("{:?}", raw)
    }
}

// Inline comment: Logic for get_message_type
fn get_message_type(message_type_id: u8) -> &'static str {
    match message_type_id {
        0 => "Sync",
        1 => "Delay_Req",
        2 => "Pdelay_Req",
        3 => "Pdelay_Resp",
        8 => "Follow_Up",
        9 => "Delay_Resp",
        10 => "Pdelay_Resp_Follow_Up",
        11 => "Announce",
        12 => "Signaling",
        13 => "Management",
        _ => "Unknown",
    }
}

#[pyfunction]
// Inline comment: Logic for parse_packet
fn parse_packet(py: Python, payload: &[u8], src_ip: String, dst_ip: String, udp_port: u16) -> PyResult<Py<PyAny>> {
    let dict = PyDict::new_bound(py);
    
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64();
    dict.set_item("timestamp", now)?;
    dict.set_item("source_ip", src_ip)?;
    dict.set_item("dest_ip", dst_ip)?;
    dict.set_item("udp_port", udp_port)?;
    
    if payload.len() >= 34 {
        let message_type_id = payload[0] & 0x0F;
        let message_type = get_message_type(message_type_id);
        let domain = payload[4];
        let seq_id = u16::from_be_bytes([payload[30], payload[31]]);
        let port_id = &payload[20..30];
        
        let message_type_label = if message_type == "Unknown" {
            format!("Unknown ({})", message_type_id)
        } else {
            message_type.to_string()
        };

        dict.set_item("message_type", message_type_label)?;
        dict.set_item("domain", domain)?;
        dict.set_item("sequence_id", seq_id)?;
        dict.set_item("clock_identity", format_clock_id(port_id))?;
    } else {
        dict.set_item("message_type", "Unknown")?;
        dict.set_item("domain", 0)?;
        dict.set_item("sequence_id", 0)?;
        dict.set_item("clock_identity", "Unknown")?;
    }
    
    Ok(dict.into_any().unbind())
}

#[pymodule]
// Inline comment: Logic for oaptpparser_rs
pub fn oaptpparser_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(parse_packet, m)?)?;
    Ok(())
}