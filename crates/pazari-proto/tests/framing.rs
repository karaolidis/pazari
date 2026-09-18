use std::time::Duration;

use pazari_proto::{
    Codec, DecodeError, Decoder, DenyReason, EncodeError, GrantReason, GuestMessage, HostMessage,
    JsonLinesCodec, Urgency,
};

const fn request() -> GuestMessage {
    GuestMessage::GrantRequest {
        id: 1,
        current: 2_147_483_648,
        target: 4_294_967_296,
        reason: GrantReason::Throttle,
        urgency: Urgency::High,
    }
}

fn drain<M, D: Decoder<M>>(decoder: &mut D) -> Vec<M> {
    let mut messages = Vec::new();
    while let Ok(Some(message)) = decoder.next_message() {
        messages.push(message);
    }
    messages
}

#[test]
fn a_message_split_into_single_byte_chunks_decodes() {
    let framed = JsonLinesCodec::encode_to_vec(&request()).unwrap();
    let mut decoder = JsonLinesCodec::decoder::<GuestMessage>();
    let mut decoded = Vec::new();

    for byte in &framed {
        decoder.push(&[*byte]).unwrap();
        decoded.extend(drain(&mut decoder));
    }

    assert_eq!(decoded, vec![request()]);
}

#[test]
fn many_messages_in_one_chunk_decode_in_order() {
    let messages = vec![
        GuestMessage::Bye {},
        request(),
        GuestMessage::OfferRelease {
            id: 2,
            current: 4_294_967_296,
            releasable: 1_073_741_824,
        },
    ];

    let mut framed = Vec::new();
    for message in &messages {
        JsonLinesCodec::encode(message, &mut framed).unwrap();
    }

    let mut decoder = JsonLinesCodec::decoder::<GuestMessage>();
    decoder.push(&framed).unwrap();

    assert_eq!(drain(&mut decoder), messages);
}

#[test]
fn an_over_long_line_fails_before_its_terminator_arrives() {
    let mut decoder = JsonLinesCodec::decoder::<GuestMessage>();

    decoder
        .push(&vec![b'x'; JsonLinesCodec::MAX_FRAME_BYTES])
        .unwrap();

    let error = decoder.push(b"x").unwrap_err();
    assert!(
        matches!(error, DecodeError::LineTooLong { len, .. } if len == JsonLinesCodec::MAX_FRAME_BYTES + 1),
        "unexpected error: {error}"
    );

    assert!(decoder.is_poisoned());
}

#[test]
fn a_line_of_exactly_the_cap_is_accepted() {
    let mut decoder = JsonLinesCodec::decoder::<GuestMessage>();

    decoder
        .push(&vec![b'x'; JsonLinesCodec::MAX_FRAME_BYTES])
        .unwrap();

    assert!(!decoder.is_poisoned());
    assert_eq!(decoder.pending(), JsonLinesCodec::MAX_FRAME_BYTES);
}

#[test]
fn a_complete_over_long_line_fails_on_decode() {
    let mut line = vec![b'x'; JsonLinesCodec::MAX_FRAME_BYTES + 1];
    line.push(b'\n');

    let mut decoder = JsonLinesCodec::decoder::<GuestMessage>();
    decoder.push(&line).unwrap();

    let error = decoder
        .next_message()
        .expect_err("expected a protocol violation");
    assert!(
        matches!(error, DecodeError::LineTooLong { len, .. } if len == JsonLinesCodec::MAX_FRAME_BYTES + 1),
        "unexpected error: {error}"
    );
}

#[test]
fn encoding_an_over_long_message_fails_and_leaves_the_buffer_untouched() {
    let message = GuestMessage::ReclaimNak {
        id: 1,
        reason: "x".repeat(JsonLinesCodec::MAX_FRAME_BYTES),
    };

    let mut out = b"prefix".to_vec();
    let error = JsonLinesCodec::encode(&message, &mut out).unwrap_err();

    assert!(
        matches!(error, EncodeError::LineTooLong { .. }),
        "unexpected error: {error}"
    );
    assert_eq!(out, b"prefix");
}

fn fatal(input: &[u8]) -> DecodeError {
    let mut decoder = JsonLinesCodec::decoder::<GuestMessage>();
    match decoder.push(input) {
        Err(error) => error,
        Ok(()) => decoder
            .next_message()
            .expect_err("expected a protocol violation"),
    }
}

#[test]
fn an_empty_line_is_fatal() {
    assert!(matches!(fatal(b"\n"), DecodeError::EmptyLine));
}

#[test]
fn a_carriage_return_is_fatal() {
    let framed = JsonLinesCodec::encode_to_vec(&GuestMessage::Bye {}).unwrap();
    let mut crlf = framed[..framed.len() - 1].to_vec();
    crlf.extend_from_slice(b"\r\n");

    assert!(matches!(
        fatal(&crlf),
        DecodeError::CarriageReturn { offset, .. } if offset == framed.len() - 1
    ));
}

#[test]
fn invalid_utf8_is_fatal() {
    assert!(matches!(fatal(b"\xff\xfe\n"), DecodeError::NotUtf8 { .. }));
}

#[test]
fn invalid_json_is_fatal() {
    assert!(matches!(
        fatal(b"{\"type\":\"bye\"\n"),
        DecodeError::Malformed { .. }
    ));
}

#[test]
fn an_unknown_type_is_fatal() {
    assert!(matches!(
        fatal(b"{\"type\":\"handshake\"}\n"),
        DecodeError::Malformed { .. }
    ));
}

#[test]
fn a_host_message_on_the_guest_stream_is_fatal() {
    assert!(matches!(
        fatal(b"{\"type\":\"welcome\",\"protocol\":1,\"ceiling\":0,\"status_interval_ms\":1000}\n"),
        DecodeError::Malformed { .. }
    ));
}

#[test]
fn a_violation_poisons_the_decoder() {
    let mut decoder = JsonLinesCodec::decoder::<GuestMessage>();
    let framed = JsonLinesCodec::encode_to_vec(&GuestMessage::Bye {}).unwrap();

    decoder.push(b"\n").unwrap();
    decoder.next_message().unwrap_err();

    assert!(decoder.is_poisoned());
    assert!(matches!(
        decoder.push(&framed).unwrap_err(),
        DecodeError::Poisoned
    ));
    assert!(matches!(
        decoder.next_message().unwrap_err(),
        DecodeError::Poisoned
    ));
}

#[test]
fn unknown_fields_are_ignored() {
    let line = br#"{"type":"grant","id":1,"target":42,"quota":7,"note":{"nested":[1,2]}}
"#;

    let mut decoder = JsonLinesCodec::decoder::<HostMessage>();
    decoder.push(line).unwrap();

    assert_eq!(
        decoder.next_message().unwrap(),
        Some(HostMessage::Grant { id: 1, target: 42 })
    );
}

#[test]
fn retry_after_ms_accepts_absent_null_and_present() {
    let cases = [
        (
            &br#"{"type":"deny","id":1,"reason":"ceiling"}
"#[..],
            None,
        ),
        (
            &br#"{"type":"deny","id":1,"reason":"ceiling","retry_after_ms":null}
"#[..],
            None,
        ),
        (
            &br#"{"type":"deny","id":1,"reason":"ceiling","retry_after_ms":250}
"#[..],
            Some(Duration::from_millis(250)),
        ),
    ];

    for (line, retry_after_ms) in cases {
        let mut decoder = JsonLinesCodec::decoder::<HostMessage>();
        decoder.push(line).unwrap();

        assert_eq!(
            decoder.next_message().unwrap(),
            Some(HostMessage::Deny {
                id: 1,
                reason: DenyReason::Ceiling,
                retry_after_ms,
            })
        );
    }
}
