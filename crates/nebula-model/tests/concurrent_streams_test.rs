#![allow(
    clippy::unwrap_used,
    clippy::panic,
    missing_docs,
    unsafe_code,
    clippy::cast_possible_truncation
)]

use std::sync::Arc;
use std::sync::atomic::Ordering;

use futures_util::StreamExt;
use nebula_hydrogen::{FrameFlags, MessageType, ShmHandle, TokenPayload};
use nebula_model::types::{ChatRequest, StreamItem};
use nebula_model::{HydrogenBackend, ModelBackend};
use nebula_proto::{ChatMessage, Role, StopReason};

unsafe fn read_from_ring(base: *const u8, mask: usize, offset: usize, dest: &mut [u8]) {
    for (i, byte) in dest.iter_mut().enumerate() {
        let ring_off = (offset + i) & mask;
        // SAFETY: Read within shared ring buffer bounds.
        *byte = unsafe { *base.add(ring_off) };
    }
}

#[tokio::test]
async fn test_concurrent_multi_stream_demuxer_isolation() {
    let test_id = format!("test_mux_{}", ulid::Ulid::generate());
    let shm_name = format!(r"Local\Nebula_Hydrogen_Test_Shm_{test_id}");
    let eng_evt = format!(r"Local\Nebula_Hydrogen_Test_Eng_{test_id}");
    let dae_evt = format!(r"Local\Nebula_Hydrogen_Test_Dae_{test_id}");

    let backend = Arc::new(
        HydrogenBackend::open_named(&shm_name, &eng_evt, &dae_evt, true)
            .expect("open hydrogen backend"),
    );

    let engine_shm =
        Arc::new(ShmHandle::open(&shm_name, &eng_evt, &dae_evt, false).expect("engine open shm"));

    const NUM_STREAMS: usize = 8;
    const TOKENS_PER_STREAM: usize = 50;

    // Engine simulator thread: reads 8 requests, then writes interleaved tokens
    let eng_clone = Arc::clone(&engine_shm);
    let engine_worker = tokio::task::spawn_blocking(move || {
        let mut streams = Vec::new();

        // 1. Collect all 8 stream requests from the request queue
        while streams.len() < NUM_STREAMS {
            eng_clone
                .wait_daemon_event(5_000)
                .expect("wait daemon request");

            let hdr = eng_clone.header();
            let mut head = hdr.request_queue.head.load(Ordering::Acquire);
            let mut tail = hdr.request_queue.tail.load(Ordering::Acquire);
            let mask = hdr.request_queue.mask as usize;
            let req_offset = hdr.request_queue_offset as usize;
            let base = eng_clone.header() as *const _ as *const u8;
            let req_base = unsafe { base.add(req_offset) };

            while tail < head {
                let frame_size = std::mem::size_of::<nebula_hydrogen::IpcFrameHeader>();
                if head.wrapping_sub(tail) < frame_size as u64 {
                    break;
                }

                let mut frame_buf = [0u8; std::mem::size_of::<nebula_hydrogen::IpcFrameHeader>()];
                unsafe {
                    read_from_ring(req_base, mask, tail as usize, &mut frame_buf);
                }
                let frame: nebula_hydrogen::IpcFrameHeader =
                    unsafe { std::ptr::read_unaligned(frame_buf.as_ptr().cast()) };

                let total = frame_size + frame.payload_size as usize;
                if head.wrapping_sub(tail) < total as u64 {
                    break;
                }

                if frame.msg_type == MessageType::GenerateRequest as u16 {
                    streams.push(frame.stream_id);
                }

                tail += total as u64;
                hdr.request_queue.tail.store(tail, Ordering::Release);
                head = hdr.request_queue.head.load(Ordering::Acquire);
            }
        }

        assert_eq!(streams.len(), NUM_STREAMS);

        // 2. Prepare interleaved sequence of response frames across all 8 streams
        // Round-robin interleaved to maximize contention and verify demuxer ordering
        for step in 0..TOKENS_PER_STREAM {
            let is_final = step + 1 == TOKENS_PER_STREAM;

            for &stream_id in &streams {
                let token_text = format!("s{stream_id}_t{step} ");
                let mut payload = TokenPayload {
                    token_id: (stream_id * 1000 + step as u32),
                    logit: 10.0,
                    text_len: token_text.len() as u16,
                    is_final: if is_final { 1 } else { 0 },
                    reserved: 0,
                    text: [0u8; 64],
                };
                payload.text[..token_text.len()].copy_from_slice(token_text.as_bytes());

                let payload_bytes = unsafe {
                    std::slice::from_raw_parts(
                        &payload as *const TokenPayload as *const u8,
                        std::mem::size_of::<TokenPayload>(),
                    )
                };

                let flags = if is_final {
                    FrameFlags::END_OF_STREAM
                } else {
                    FrameFlags::NONE
                };

                // Write frame directly into response queue
                let hdr = eng_clone.header();
                let head = hdr.response_queue.head.load(Ordering::Acquire);
                let mask = hdr.response_queue.mask as usize;
                let rsq_offset = hdr.response_queue_offset as usize;

                let resp_frame = nebula_hydrogen::IpcFrameHeader {
                    magic: nebula_hydrogen::IPC_MAGIC,
                    msg_type: MessageType::TokenChunk as u16,
                    flags,
                    stream_id,
                    payload_size: payload_bytes.len() as u32,
                    sequence_num: (step + 1) as u64,
                    timestamp_ns: 1000,
                };

                unsafe {
                    let base = eng_clone.header() as *const _ as *mut u8;
                    let rsq_base = base.add(rsq_offset);
                    let frame_bytes = std::slice::from_raw_parts(
                        &resp_frame as *const _ as *const u8,
                        std::mem::size_of::<nebula_hydrogen::IpcFrameHeader>(),
                    );

                    for (i, &b) in frame_bytes.iter().enumerate() {
                        let off = (head as usize + i) & mask;
                        rsq_base.add(off).write(b);
                    }

                    for (i, &b) in payload_bytes.iter().enumerate() {
                        let off = (head as usize
                            + std::mem::size_of::<nebula_hydrogen::IpcFrameHeader>()
                            + i)
                            & mask;
                        rsq_base.add(off).write(b);
                    }
                }

                let total_frame_len = (std::mem::size_of::<nebula_hydrogen::IpcFrameHeader>()
                    + payload_bytes.len()) as u64;
                hdr.response_queue
                    .head
                    .store(head + total_frame_len, Ordering::Release);

                eng_clone
                    .signal_engine_event()
                    .expect("signal engine event");
            }
        }
    });

    // 3. Launch 8 concurrent client tasks sending chat requests
    let mut tasks = Vec::new();
    for client_idx in 0..NUM_STREAMS {
        let b = Arc::clone(&backend);
        tasks.push(tokio::spawn(async move {
            let mut req = ChatRequest::new(vec![ChatMessage {
                role: Role::User,
                content: format!("Stream {client_idx} prompt"),
            }]);
            req.max_tokens = TOKENS_PER_STREAM as u32;

            let mut stream = b.chat(req).await.expect("chat call succeeds");
            let mut received_tokens = Vec::new();
            let mut stop_reason = None;

            while let Some(item) = stream.next().await {
                match item.expect("stream item ok") {
                    StreamItem::Token { text, .. } => {
                        received_tokens.push(text);
                    }
                    StreamItem::Done(reason) => {
                        stop_reason = Some(reason);
                        break;
                    }
                    _ => {}
                }
            }

            (client_idx, received_tokens, stop_reason)
        }));
    }

    // Wait for all client streams and engine worker
    engine_worker.await.expect("engine worker ok");

    for task in tasks {
        let (client_idx, tokens, stop_reason) = task.await.expect("client task ok");
        assert_eq!(
            stop_reason,
            Some(StopReason::Stop),
            "Stream {client_idx} did not finish cleanly with StopReason::Stop"
        );
        assert_eq!(
            tokens.len(),
            TOKENS_PER_STREAM,
            "Stream {client_idx} missed tokens! Got {} / {TOKENS_PER_STREAM}",
            tokens.len()
        );

        // Verify exact token content and order: tokens must match s{stream_id}_t{step}
        // Extract stream_prefix from first token (e.g., "s1")
        let first = &tokens[0];
        let stream_prefix = first.split('_').next().expect("prefix");

        for (step, token) in tokens.iter().enumerate() {
            let expected = format!("{stream_prefix}_t{step} ");
            assert_eq!(
                token, &expected,
                "Token mismatch on {stream_prefix} at step {step}: expected '{expected}', got '{token}'"
            );
        }
    }
}
