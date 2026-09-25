use std::{marker::PhantomData, str};

use serde::{Serialize, de::DeserializeOwned};

use crate::{
    DecodeError, EncodeError,
    codec::{Codec, Decoder},
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JsonLinesCodec;

impl Codec for JsonLinesCodec {
    type Decoder<M: DeserializeOwned> = JsonLinesDecoder<M>;

    const MAX_FRAME_BYTES: usize = 64 * 1024;

    fn encode<M>(message: &M, out: &mut Vec<u8>) -> Result<(), EncodeError>
    where
        M: Serialize + ?Sized,
    {
        let start = out.len();

        if let Err(error) = serde_json::to_writer(&mut *out, message) {
            out.truncate(start);
            return Err(error.into());
        }

        let len = out.len() - start;
        if len > Self::MAX_FRAME_BYTES {
            out.truncate(start);
            return Err(EncodeError::LineTooLong {
                len,
                cap: Self::MAX_FRAME_BYTES,
            });
        }

        out.push(b'\n');
        Ok(())
    }

    fn decoder<M: DeserializeOwned>() -> Self::Decoder<M> {
        JsonLinesDecoder::new()
    }
}

#[derive(Debug)]
pub struct JsonLinesDecoder<M> {
    buf: Vec<u8>,
    poisoned: bool,
    message: PhantomData<fn() -> M>,
}

impl<M> JsonLinesDecoder<M> {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: Vec::new(),
            poisoned: false,
            message: PhantomData,
        }
    }
}

impl<M> Default for JsonLinesDecoder<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: DeserializeOwned> Decoder<M> for JsonLinesDecoder<M> {
    fn push(&mut self, chunk: &[u8]) -> Result<(), DecodeError> {
        if self.poisoned {
            return Err(DecodeError::Poisoned);
        }

        self.buf.extend_from_slice(chunk);

        let unterminated = self
            .buf
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(self.buf.len(), |index| self.buf.len() - index - 1);

        if unterminated > JsonLinesCodec::MAX_FRAME_BYTES {
            self.poisoned = true;
            return Err(DecodeError::LineTooLong {
                len: unterminated,
                cap: JsonLinesCodec::MAX_FRAME_BYTES,
            });
        }

        Ok(())
    }

    fn next_message(&mut self) -> Result<Option<M>, DecodeError> {
        if self.poisoned {
            return Err(DecodeError::Poisoned);
        }

        let Some(index) = self.buf.iter().position(|&byte| byte == b'\n') else {
            return Ok(None);
        };

        let result = decode_line(&self.buf[..index]);
        self.buf.drain(..=index);

        match result {
            Ok(message) => Ok(Some(message)),
            Err(error) => {
                self.poisoned = true;
                Err(error)
            }
        }
    }

    fn pending(&self) -> usize {
        self.buf.len()
    }

    fn is_poisoned(&self) -> bool {
        self.poisoned
    }
}

fn decode_line<M: DeserializeOwned>(line: &[u8]) -> Result<M, DecodeError> {
    if line.is_empty() {
        return Err(DecodeError::EmptyLine);
    }

    if line.len() > JsonLinesCodec::MAX_FRAME_BYTES {
        return Err(DecodeError::LineTooLong {
            len: line.len(),
            cap: JsonLinesCodec::MAX_FRAME_BYTES,
        });
    }

    if let Some(offset) = line.iter().position(|&byte| byte == b'\r') {
        return Err(DecodeError::CarriageReturn {
            offset,
            len: line.len(),
        });
    }

    let text = str::from_utf8(line)?;

    serde_json::from_str(text).map_err(|source| DecodeError::Malformed {
        snippet: snippet(text),
        source,
    })
}

const SNIPPET_CHARS: usize = 256;

fn snippet(line: &str) -> String {
    match line.char_indices().nth(SNIPPET_CHARS) {
        Some((offset, _)) => format!("{}...", &line[..offset]),
        None => line.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::{
        GuestMessage, HostMessage,
        message::fixtures::{guest_message, guest_messages, host_message, host_messages},
    };

    const CAP: usize = JsonLinesCodec::MAX_FRAME_BYTES;

    const fn request() -> GuestMessage {
        GuestMessage::GrantRequest {
            id: 1,
            current: 2_147_483_648,
            target: 4_294_967_296,
        }
    }

    fn drain<M, D: Decoder<M>>(decoder: &mut D) -> Vec<M> {
        let mut messages = Vec::new();
        while let Ok(Some(message)) = decoder.next_message() {
            messages.push(message);
        }
        messages
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

    fn rechunked<M, S>(stream: &[u8], sizes: &[usize]) -> Result<Vec<M>, TestCaseError>
    where
        M: serde::de::DeserializeOwned,
        S: Codec,
    {
        let mut decoder = S::decoder::<M>();
        let mut decoded = Vec::new();
        let mut offset = 0;

        for size in sizes.iter().copied().cycle() {
            if offset == stream.len() {
                break;
            }

            let end = (offset + size).min(stream.len());
            decoder
                .push(&stream[offset..end])
                .map_err(|error| TestCaseError::fail(error.to_string()))?;
            offset = end;

            while let Some(message) = decoder
                .next_message()
                .map_err(|error| TestCaseError::fail(error.to_string()))?
            {
                decoded.push(message);
            }
        }

        Ok(decoded)
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

        decoder.push(&vec![b'x'; CAP]).unwrap();

        let error = decoder.push(b"x").unwrap_err();
        assert!(
            matches!(error, DecodeError::LineTooLong { len, cap } if len == CAP + 1 && cap == CAP),
            "unexpected error: {error}"
        );

        assert!(decoder.is_poisoned());
    }

    #[test]
    fn a_line_of_exactly_the_cap_is_accepted() {
        let mut decoder = JsonLinesCodec::decoder::<GuestMessage>();

        decoder.push(&vec![b'x'; CAP]).unwrap();

        assert!(!decoder.is_poisoned());
        assert_eq!(decoder.pending(), CAP);
    }

    #[test]
    fn a_complete_over_long_line_fails_on_decode() {
        let mut line = vec![b'x'; CAP + 1];
        line.push(b'\n');

        let mut decoder = JsonLinesCodec::decoder::<GuestMessage>();
        decoder.push(&line).unwrap();

        let error = decoder
            .next_message()
            .expect_err("expected a protocol violation");
        assert!(
            matches!(error, DecodeError::LineTooLong { len, cap } if len == CAP + 1 && cap == CAP),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn encoding_an_over_long_message_fails_and_leaves_the_buffer_untouched() {
        let message = GuestMessage::ReclaimNak {
            id: 1,
            reason: "x".repeat(CAP),
        };

        let mut out = b"prefix".to_vec();
        let error = JsonLinesCodec::encode(&message, &mut out).unwrap_err();

        assert!(
            matches!(error, EncodeError::LineTooLong { cap, .. } if cap == CAP),
            "unexpected error: {error}"
        );
        assert_eq!(out, b"prefix");
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
    fn a_malformed_line_is_quoted_back_only_so_far() {
        let short = "x".repeat(SNIPPET_CHARS);
        assert_eq!(snippet(&short), short);

        let long = "x".repeat(SNIPPET_CHARS + 1);
        assert_eq!(snippet(&long), format!("{short}..."));
    }

    #[test]
    fn guest_messages_survive_the_codec() {
        for message in guest_messages() {
            let framed = JsonLinesCodec::encode_to_vec(&message).unwrap();
            assert_eq!(framed.last(), Some(&b'\n'));

            let mut decoder = JsonLinesCodec::decoder::<GuestMessage>();
            decoder.push(&framed).unwrap();

            assert_eq!(decoder.next_message().unwrap(), Some(message));
            assert_eq!(decoder.next_message().unwrap(), None);
        }
    }

    #[test]
    fn host_messages_survive_the_codec() {
        for message in host_messages() {
            let framed = JsonLinesCodec::encode_to_vec(&message).unwrap();
            assert_eq!(framed.last(), Some(&b'\n'));

            let mut decoder = JsonLinesCodec::decoder::<HostMessage>();
            decoder.push(&framed).unwrap();

            assert_eq!(decoder.next_message().unwrap(), Some(message));
            assert_eq!(decoder.next_message().unwrap(), None);
        }
    }

    proptest! {
        #[test]
        fn guest_messages_round_trip(message in guest_message()) {
            let framed = JsonLinesCodec::encode_to_vec(&message)?;
            let decoded = rechunked::<GuestMessage, JsonLinesCodec>(&framed, &[framed.len().max(1)])?;
            prop_assert_eq!(decoded, vec![message]);
        }

        #[test]
        fn host_messages_round_trip(message in host_message()) {
            let framed = JsonLinesCodec::encode_to_vec(&message)?;
            let decoded = rechunked::<HostMessage, JsonLinesCodec>(&framed, &[framed.len().max(1)])?;
            prop_assert_eq!(decoded, vec![message]);
        }

        #[test]
        fn a_guest_stream_survives_arbitrary_chunking(
            messages in prop::collection::vec(guest_message(), 0..8),
            sizes in prop::collection::vec(1usize..64, 1..8),
        ) {
            let mut stream = Vec::new();
            for message in &messages {
                JsonLinesCodec::encode(message, &mut stream)?;
            }

            let decoded = rechunked::<GuestMessage, JsonLinesCodec>(&stream, &sizes)?;
            prop_assert_eq!(decoded, messages);
        }

        #[test]
        fn a_host_stream_survives_arbitrary_chunking(
            messages in prop::collection::vec(host_message(), 0..8),
            sizes in prop::collection::vec(1usize..64, 1..8),
        ) {
            let mut stream = Vec::new();
            for message in &messages {
                JsonLinesCodec::encode(message, &mut stream)?;
            }

            let decoded = rechunked::<HostMessage, JsonLinesCodec>(&stream, &sizes)?;
            prop_assert_eq!(decoded, messages);
        }
    }
}
