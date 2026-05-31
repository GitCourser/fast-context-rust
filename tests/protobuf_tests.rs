use fast_context_rust::protobuf::{
    ProtobufEncoder, connect_frame_decode, connect_frame_encode, decode_varint, extract_strings,
};
use flate2::Compression;
use flate2::write::GzEncoder;
use std::io::Write;

const GOLDEN_SIMPLE_MESSAGE_HEX: &str = "089601120568656c6c6f1a050801120178";

#[test]
fn protobuf_encoder_matches_node_golden_hex() {
    let nested = ProtobufEncoder::new()
        .write_varint(1, 1)
        .write_string(2, "x");
    let encoded = ProtobufEncoder::new()
        .write_varint(1, 150)
        .write_string(2, "hello")
        .write_message(3, &nested)
        .to_vec();

    assert_eq!(to_hex(&encoded), GOLDEN_SIMPLE_MESSAGE_HEX);
}

#[test]
fn decode_varint_returns_value_and_new_offset() {
    let (value, offset) = decode_varint(&[0x96, 0x01, 0xff], 0).expect("varint");
    assert_eq!(value, 150);
    assert_eq!(offset, 2);
}

#[test]
fn connect_frame_round_trips_uncompressed_and_gzip() {
    let payload = from_hex(GOLDEN_SIMPLE_MESSAGE_HEX);
    let uncompressed = connect_frame_encode(&payload, false).expect("uncompressed frame");
    assert_eq!(uncompressed[0], 0);
    assert_eq!(
        u32::from_be_bytes(uncompressed[1..5].try_into().unwrap()) as usize,
        payload.len()
    );
    assert_eq!(
        connect_frame_decode(&uncompressed).expect("decode"),
        vec![payload.clone()]
    );

    let compressed = connect_frame_encode(&payload, true).expect("compressed frame");
    assert_eq!(compressed[0], 1);
    assert_eq!(
        connect_frame_decode(&compressed).expect("decode"),
        vec![payload]
    );
}

#[test]
fn connect_frame_decodes_flag_three_gzip() {
    let payload = b"flag three compressed payload";
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(payload).expect("gzip write");
    let compressed = encoder.finish().expect("gzip finish");
    let mut frame = vec![3_u8];
    frame.extend_from_slice(&(compressed.len() as u32).to_be_bytes());
    frame.extend_from_slice(&compressed);

    assert_eq!(
        connect_frame_decode(&frame).expect("decode"),
        vec![payload.to_vec()]
    );
}

#[test]
fn extract_strings_filters_short_and_invalid_payloads() {
    let encoded = ProtobufEncoder::new()
        .write_string(1, "short")
        .write_string(2, "longer string")
        .write_bytes(3, &[0xff, 0xfe, 0xfd])
        .to_vec();

    assert_eq!(extract_strings(&encoded), vec!["longer string".to_string()]);
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn from_hex(hex: &str) -> Vec<u8> {
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(text, 16).unwrap()
        })
        .collect()
}
