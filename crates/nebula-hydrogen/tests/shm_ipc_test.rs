#![allow(clippy::unwrap_used, clippy::panic, missing_docs, unsafe_code)]

use std::sync::Arc;
use std::sync::atomic::Ordering;

use nebula_hydrogen::{
    FrameFlags, IPC_MAGIC, IPC_VERSION, MessageType, REQUEST_QUEUE_CAPACITY_BYTES,
    RESPONSE_QUEUE_CAPACITY_BYTES, ShmHandle, TokenPayload, ToolSpawnGuard,
};

#[test]
fn test_shm_init_and_geometry() {
    let test_id = format!("test_init_{}", ulid::Ulid::generate());
    let shm_name = format!(r"Local\Nebula_Hydrogen_Test_Shm_{test_id}");
    let eng_evt = format!(r"Local\Nebula_Hydrogen_Test_Eng_{test_id}");
    let dae_evt = format!(r"Local\Nebula_Hydrogen_Test_Dae_{test_id}");

    let handle = ShmHandle::open(&shm_name, &eng_evt, &dae_evt, true).expect("open shm");
    let hdr = handle.header();

    assert_eq!(hdr.magic, IPC_MAGIC);
    assert_eq!(hdr.version, IPC_VERSION);
    assert_eq!(
        hdr.request_queue.capacity_bytes,
        REQUEST_QUEUE_CAPACITY_BYTES as u64
    );
    assert_eq!(
        hdr.response_queue.capacity_bytes,
        RESPONSE_QUEUE_CAPACITY_BYTES as u64
    );
    assert_eq!(handle.active_tool_processes(), 0);
    assert_eq!(handle.cuda_yield_mode(), 0);
}

#[test]
fn test_active_tool_processes_barrier() {
    let test_id = format!("test_atp_{}", ulid::Ulid::generate());
    let shm_name = format!(r"Local\Nebula_Hydrogen_Test_Shm_{test_id}");
    let eng_evt = format!(r"Local\Nebula_Hydrogen_Test_Eng_{test_id}");
    let dae_evt = format!(r"Local\Nebula_Hydrogen_Test_Dae_{test_id}");

    let handle = Arc::new(ShmHandle::open(&shm_name, &eng_evt, &dae_evt, true).expect("open shm"));
    assert_eq!(handle.active_tool_processes(), 0);

    // Direct spawn and complete
    handle.tool_process_spawned();
    assert_eq!(handle.active_tool_processes(), 1);

    handle.tool_process_completed();
    assert_eq!(handle.active_tool_processes(), 0);

    // RAII guard test with global registration
    nebula_hydrogen::register_global_shm(Arc::clone(&handle));

    {
        let guard1 = ToolSpawnGuard::new();
        assert_eq!(handle.active_tool_processes(), 1);

        {
            let guard2 = ToolSpawnGuard::new();
            assert_eq!(handle.active_tool_processes(), 2);
            drop(guard2);
        }

        assert_eq!(handle.active_tool_processes(), 1);
        drop(guard1);
    }

    assert_eq!(handle.active_tool_processes(), 0);
    nebula_hydrogen::unregister_global_shm();
}

#[test]
fn test_token_payload_safe_deserialization() {
    let mut payload = TokenPayload {
        token_id: 12345,
        logit: 9.87,
        text_len: 5,
        is_final: 0,
        reserved: 0,
        text: [0u8; 64],
    };
    payload.text[0..5].copy_from_slice(b"hello");

    let bytes = unsafe {
        std::slice::from_raw_parts(
            &payload as *const TokenPayload as *const u8,
            std::mem::size_of::<TokenPayload>(),
        )
    };

    let parsed = TokenPayload::from_bytes(bytes).expect("parse valid payload");
    assert_eq!(parsed.token_id, 12345);
    assert_eq!(parsed.text_str(), "hello");
    assert_eq!(parsed.is_final, 0);

    // Too short slice returns None safely
    assert!(TokenPayload::from_bytes(&bytes[..10]).is_none());
}

#[test]
fn test_frame_write_and_read() {
    let test_id = format!("test_frame_{}", ulid::Ulid::generate());
    let shm_name = format!(r"Local\Nebula_Hydrogen_Test_Shm_{test_id}");
    let eng_evt = format!(r"Local\Nebula_Hydrogen_Test_Eng_{test_id}");
    let dae_evt = format!(r"Local\Nebula_Hydrogen_Test_Dae_{test_id}");

    let handle = ShmHandle::open(&shm_name, &eng_evt, &dae_evt, true).expect("open shm");

    // Write request frame
    let req_body = b"{\"prompt\": \"Hello Hydrogen\"}";
    handle
        .write_request_frame(MessageType::GenerateRequest, FrameFlags::NONE, 42, req_body)
        .expect("write request frame");

    // Verify request frame is written into request queue
    let hdr = handle.header();
    let head = hdr.request_queue.head.load(Ordering::Acquire);
    assert!(head > 0);
}
