//! Incremental, transport-only Responses SSE framing.

use super::{StreamAssemblyError, extract_data_payload, find_sse_boundary_bytes};

/// Upper bound on an unterminated SSE frame retained between `push` calls.
///
/// A peer that never sends a frame delimiter would otherwise grow the buffer
/// without limit. The cap is far above any legitimate Responses event and is
/// checked only after every complete frame has been drained.
pub(crate) const MAX_PENDING_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Reassembles Responses server-sent events from arbitrarily split byte chunks.
#[derive(Debug, Default)]
pub(crate) struct ResponsesSseDecoder {
    buffer: Vec<u8>,
}

impl ResponsesSseDecoder {
    /// Append bytes and return every data payload completed by them.
    ///
    /// Each result corresponds to one `\n\n`-delimited frame. A frame that is
    /// not valid UTF-8, or one that exceeds [`MAX_PENDING_FRAME_BYTES`] without
    /// a delimiter, yields an error while later input continues to be framed.
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<Result<String, StreamAssemblyError>> {
        self.buffer.extend_from_slice(bytes);
        let mut payloads = Vec::new();
        let mut offset = 0usize;

        while let Some((boundary_index, delimiter_len)) = find_sse_boundary_bytes(&self.buffer, offset) {
            let frame_start = offset;
            offset = boundary_index + delimiter_len;
            match std::str::from_utf8(&self.buffer[frame_start..boundary_index]) {
                Ok(event) => {
                    if let Some(payload) = extract_data_payload(event) {
                        payloads.push(Ok(payload.into_owned()));
                    }
                }
                Err(error) => payloads
                    .push(Err(StreamAssemblyError::InvalidPayload(format!("Responses SSE is not UTF-8: {error}")))),
            }
        }

        if offset > 0 {
            self.buffer.drain(..offset);
        }

        if self.buffer.len() > MAX_PENDING_FRAME_BYTES {
            self.buffer.clear();
            payloads.push(Err(StreamAssemblyError::InvalidPayload(format!(
                "Responses SSE frame exceeded the maximum buffered size of {MAX_PENDING_FRAME_BYTES} bytes"
            ))));
        }
        payloads
    }

    /// Reject a buffer that still holds an undelimited, non-whitespace frame.
    ///
    /// A clean end of stream is one where only whitespace remains, because the
    /// final delimiter of the last event has already been consumed.
    pub(crate) fn finish(&self) -> Result<(), StreamAssemblyError> {
        if self.buffer.iter().all(u8::is_ascii_whitespace) {
            Ok(())
        } else {
            Err(StreamAssemblyError::InvalidPayload("Responses SSE ended with an incomplete event".to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PENDING_FRAME_BYTES, ResponsesSseDecoder, StreamAssemblyError};
    use crate::providers::shared::responses_adapter::{ResponsesStreamAdapter, ResponsesStreamEvent};
    use proptest::prelude::*;
    use proptest::test_runner::TestCaseResult;
    use serde_json::json;

    /// Consume property frames until the completed terminal, ignoring later frames.
    fn consume_until_completed(
        frames: Vec<Result<String, StreamAssemblyError>>,
        visible: &mut Vec<String>,
        completed_terminal: &mut bool,
    ) -> TestCaseResult {
        for frame in frames {
            if *completed_terminal {
                break;
            }
            let payload = frame.map_err(|error| TestCaseError::fail(error.to_string()))?;
            match ResponsesStreamAdapter::parse_sse_data_for_provider("PropertyProvider", &payload)
                .map_err(|error| TestCaseError::fail(error.to_string()))?
            {
                ResponsesStreamEvent::TextDelta { delta, .. } => visible.push(delta),
                ResponsesStreamEvent::CompletedResponse { .. } => *completed_terminal = true,
                other => return Err(TestCaseError::fail(format!("unexpected event: {other:?}"))),
            }
        }
        Ok(())
    }

    /// Prove incomplete and invalid wire events are rejected.
    #[test]
    fn incomplete_and_invalid_wire_events_are_rejected() {
        let mut incomplete = ResponsesSseDecoder::default();
        assert!(incomplete.push(b"data: {\"type\":").is_empty());
        assert!(incomplete.finish().is_err());

        let mut invalid = ResponsesSseDecoder::default();
        assert!(matches!(invalid.push(b"data: \xff\n\n").as_slice(), [Err(_)]));
    }

    /// Prove an unterminated frame beyond the cap is rejected and discarded.
    #[test]
    fn unterminated_frame_beyond_the_cap_is_rejected_and_discarded() {
        let mut decoder = ResponsesSseDecoder::default();
        let oversized = vec![b'a'; MAX_PENDING_FRAME_BYTES + 1];

        let frames = decoder.push(&oversized);

        assert!(matches!(
            frames.as_slice(),
            [Err(StreamAssemblyError::InvalidPayload(message))] if message.contains("maximum buffered size")
        ));
        assert!(decoder.finish().is_ok());
    }

    /// Prove a valid prefix is delivered before an invalid suffix in one chunk.
    #[test]
    fn valid_prefix_is_delivered_before_invalid_suffix_in_one_chunk() {
        let mut decoder = ResponsesSseDecoder::default();
        let frames = decoder.push(b"data: {\"type\":\"keepalive\"}\n\ndata: \xff\n\n");

        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].as_deref().expect("valid first frame"), "{\"type\":\"keepalive\"}");
        assert!(frames[1].is_err());
    }

    proptest! {
        /// Prove arbitrary byte partitions preserve UTF-8, CRLF, and JSON escapes.
        #[test]
        fn arbitrary_network_byte_partitions_preserve_utf8_crlf_and_json_escapes(
            deltas in prop::collection::vec(prop::collection::vec(any::<char>(), 0..16), 0..12),
            split_hints in prop::collection::vec(any::<u8>(), 0..64),
            use_crlf in any::<bool>(),
        ) {
            let expected = deltas
                .iter()
                .map(|characters| characters.iter().collect::<String>())
                .collect::<Vec<_>>();
            let delimiter = if use_crlf { "\r\n\r\n" } else { "\n\n" };
            let wire = expected
                .iter()
                .enumerate()
                .map(|(index, delta)| {
                    let payload = json!({
                        "type": "response.output_text.delta",
                        "sequence_number": u64::try_from(index).unwrap_or(u64::MAX),
                        "item_id": "message_1",
                        "output_index": 0,
                        "content_index": 0,
                        "delta": delta,
                    });
                    format!("data: {payload}{delimiter}")
                })
                .collect::<String>()
                .into_bytes();

            let mut decoder = ResponsesSseDecoder::default();
            let mut decoded = Vec::new();
            let mut offset = 0usize;
            for hint in split_hints {
                if offset == wire.len() {
                    break;
                }
                let remaining = wire.len() - offset;
                let chunk_len = usize::from(hint) % remaining + 1;
                for payload in decoder.push(&wire[offset..offset + chunk_len]) {
                    decoded.push(payload.map_err(|error| TestCaseError::fail(error.to_string()))?);
                }
                offset += chunk_len;
            }
            for payload in decoder.push(&wire[offset..]) {
                decoded.push(payload.map_err(|error| TestCaseError::fail(error.to_string()))?);
            }
            decoder.finish().map_err(|error| TestCaseError::fail(error.to_string()))?;

            let mut actual = Vec::new();
            for payload in decoded {
                match ResponsesStreamAdapter::parse_sse_data_for_provider("PropertyProvider", &payload)
                    .map_err(|error| TestCaseError::fail(error.to_string()))?
                {
                    ResponsesStreamEvent::TextDelta { delta, .. } => actual.push(delta),
                    other => return Err(TestCaseError::fail(format!("unexpected event: {other:?}"))),
                }
            }
            prop_assert_eq!(actual, expected);
        }

        /// Prove an invalid suffix after completion never erases the prefix or terminal.
        #[test]
        fn invalid_suffix_after_completed_never_erases_prefix_or_terminal(
            characters in prop::collection::vec(any::<char>(), 0..32),
            split_hints in prop::collection::vec(any::<u8>(), 0..48),
            use_crlf in any::<bool>(),
        ) {
            let expected = characters.iter().collect::<String>();
            let delimiter = if use_crlf { "\r\n\r\n" } else { "\n\n" };
            let text = json!({
                "type": "response.output_text.delta",
                "sequence_number": 1,
                "item_id": "message_1",
                "output_index": 0,
                "content_index": 0,
                "delta": expected,
            });
            let completed = json!({
                "type": "response.completed",
                "sequence_number": 2,
                "response": {"output": []},
            });
            let mut wire = format!("data: {text}{delimiter}data: {completed}{delimiter}").into_bytes();
            wire.extend_from_slice(b"data: \xff\n\n");

            let mut decoder = ResponsesSseDecoder::default();
            let mut offset = 0usize;
            let mut visible = Vec::new();
            let mut completed_terminal = false;

            for hint in split_hints {
                if offset == wire.len() || completed_terminal {
                    break;
                }
                let remaining = wire.len() - offset;
                let chunk_len = usize::from(hint) % remaining + 1;
                consume_until_completed(
                    decoder.push(&wire[offset..offset + chunk_len]),
                    &mut visible,
                    &mut completed_terminal,
                )?;
                offset += chunk_len;
            }
            if !completed_terminal {
                consume_until_completed(decoder.push(&wire[offset..]), &mut visible, &mut completed_terminal)?;
            }

            prop_assert!(completed_terminal);
            prop_assert_eq!(visible, vec![expected]);
        }
    }
}
