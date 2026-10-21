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
use nebula_model::types::{BackendHealth, ChatRequest, StreamItem};
use nebula_model::{HydrogenBackend, ModelBackend};
use nebula_proto::{ChatMessage, Role, StopReason};

#[tokio::test]
async fn test_hydrogen_backend_health_and_chat_stream() {
    let test_id = format!("test_hyd_{}", ulid::Ulid::generate());
    let shm_name = format!(r"Local\Nebula_Hydrogen_Test_Shm_{test_id}");
    let eng_evt = format!(r"Local\Nebula_Hydrogen_Test_Eng_{test_id}");
    let dae_evt = format!(r"Local\Nebula_Hydrogen_Test_Dae_{test_id}");

    let backend = HydrogenBackend::open_named(&shm_name, &eng_evt, &dae_evt, true)
        .expect("open hydrogen backend");

    // Engine side simulator
    let engine_shm =
        Arc::new(ShmHandle::open(&shm_name, &eng_evt, &dae_evt, false).expect("engine open shm"));

    // 1. Health check test
    // Spawn an engine task that responds to ping
    let eng_clone = Arc::clone(&engine_shm);
    tokio::task::spawn_blocking(move || {
        // Wait for ping from daemon
        eng_clone.wait_daemon_event(5_000).expect("wait ping");
        // Write pong into response queue
        let hdr = eng_clone.header();
        let tail = hdr.response_queue.tail.load(Ordering::Acquire);
        let mask = hdr.response_queue.mask as usize;
        let rsq_offset = hdr.response_queue_offset as usize;

        let pong_frame = nebula_hydrogen::IpcFrameHeader {
            magic: nebula_hydrogen::IPC_MAGIC,
            msg_type: MessageType::Pong as u16,
            flags: FrameFlags::NONE,
            stream_id: 0,
            payload_size: 0,
            sequence_num: 1,
            timestamp_ns: 1000,
        };

        // SAFETY: Test writes into response queue
        unsafe {
            let base = eng_clone.header() as *const _ as *mut u8;
            let rsq_base = base.add(rsq_offset);
            let frame_bytes = std::slice::from_raw_parts(
                &pong_frame as *const _ as *const u8,
                std::mem::size_of::<nebula_hydrogen::IpcFrameHeader>(),
            );
            for (i, &b) in frame_bytes.iter().enumerate() {
                let off = (tail as usize + i) & mask;
                rsq_base.add(off).write(b);
            }
        }
        hdr.response_queue.head.store(
            tail + std::mem::size_of::<nebula_hydrogen::IpcFrameHeader>() as u64,
            Ordering::Release,
        );
        eng_clone
            .signal_engine_event()
            .expect("signal engine event");
    });

    let health = backend.health().await.expect("health succeeds");
    assert_eq!(health, BackendHealth::Ok);

    // 2. Chat streaming test
    let eng_clone2 = Arc::clone(&engine_shm);
    tokio::task::spawn_blocking(move || {
        // Wait for generate request
        eng_clone2.wait_daemon_event(5_000).expect("wait generate");

        // Write a TokenChunk frame
        let mut token = TokenPayload {
            token_id: 42,
            logit: 1.0,
            text_len: 5,
            is_final: 1,
            reserved: 0,
            text: [0u8; 64],
        };
        token.text[0..5].copy_from_slice(b"world");

        let token_bytes = unsafe {
            std::slice::from_raw_parts(
                &token as *const TokenPayload as *const u8,
                std::mem::size_of::<TokenPayload>(),
            )
        };

        let token_frame = nebula_hydrogen::IpcFrameHeader {
            magic: nebula_hydrogen::IPC_MAGIC,
            msg_type: MessageType::TokenChunk as u16,
            flags: FrameFlags::END_OF_STREAM,
            stream_id: 1,
            payload_size: token_bytes.len() as u32,
            sequence_num: 2,
            timestamp_ns: 2000,
        };

        let hdr = eng_clone2.header();
        let head = hdr.response_queue.head.load(Ordering::Acquire);
        let mask = hdr.response_queue.mask as usize;
        let rsq_offset = hdr.response_queue_offset as usize;

        // SAFETY: Test writes into response queue
        unsafe {
            let base = eng_clone2.header() as *const _ as *mut u8;
            let rsq_base = base.add(rsq_offset);
            let frame_bytes = std::slice::from_raw_parts(
                &token_frame as *const _ as *const u8,
                std::mem::size_of::<nebula_hydrogen::IpcFrameHeader>(),
            );
            for (i, &b) in frame_bytes.iter().enumerate() {
                let off = (head as usize + i) & mask;
                rsq_base.add(off).write(b);
            }
            for (i, &b) in token_bytes.iter().enumerate() {
                let off = (head as usize + frame_bytes.len() + i) & mask;
                rsq_base.add(off).write(b);
            }
        }
        let total =
            (std::mem::size_of::<nebula_hydrogen::IpcFrameHeader>() + token_bytes.len()) as u64;
        hdr.response_queue
            .head
            .store(head + total, Ordering::Release);
        eng_clone2
            .signal_engine_event()
            .expect("signal engine event");
    });

    let mut req = ChatRequest::new(vec![ChatMessage {
        role: Role::User,
        content: "hello".into(),
    }]);
    req.max_tokens = 10;

    let mut stream = backend.chat(req).await.expect("chat call succeeds");
    let mut items = Vec::new();
    while let Some(item) = stream.next().await {
        items.push(item.expect("stream item"));
    }

    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0],
        StreamItem::Token {
            text: "world".into(),
            reasoning: false,
        }
    );
    assert_eq!(items[1], StreamItem::Done(StopReason::Stop));
}
