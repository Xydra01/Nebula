#pragma once

#include <cstdint>
#include <cstddef>
#include <atomic>
#include <string>
#include <cstring>
#include <chrono>

#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#endif

namespace hydrogen::ipc {

// ============================================================================
// IPC Magic & Geometry Constants
// ============================================================================

/// @brief Protocol Magic identifier: "HYDR" (0x48594452)
inline constexpr uint32_t IPC_MAGIC = 0x48594452;

/// @brief Protocol Version
inline constexpr uint32_t IPC_VERSION = 1;

/// @brief Default named shared memory location
inline constexpr const wchar_t* DEFAULT_SHM_NAME = L"Local\\Nebula_Hydrogen_IPC";

/// @brief Engine event: signaled by Engine when response/token is ready
inline constexpr const wchar_t* DEFAULT_ENGINE_EVENT_NAME = L"Local\\Nebula_Hydrogen_IPC_EngineEvent";

/// @brief Daemon event: signaled by Daemon when request/command is submitted
inline constexpr const wchar_t* DEFAULT_DAEMON_EVENT_NAME = L"Local\\Nebula_Hydrogen_IPC_DaemonEvent";

/// @brief Total shared memory backing size: 64 MiB (67,108,864 bytes)
inline constexpr size_t SHM_TOTAL_CAPACITY_BYTES = 64ULL * 1024 * 1024;

/// @brief Daemon -> Engine Request Queue: 16 MiB
inline constexpr size_t REQUEST_QUEUE_CAPACITY_BYTES = 16ULL * 1024 * 1024;

/// @brief Engine -> Daemon Response Queue: 48 MiB
inline constexpr size_t RESPONSE_QUEUE_CAPACITY_BYTES = 48ULL * 1024 * 1024;

/// @brief Target token signaling latency ceiling: strictly < 50 us (0.050 ms)
inline constexpr double MAX_SIGNALING_LATENCY_CEILING_US = 50.0;

// ============================================================================
// Protocol Message Types & Flags
// ============================================================================

enum class MessageType : uint16_t {
    MSG_NONE              = 0x0000,
    MSG_PING              = 0x0001,
    MSG_PONG              = 0x0002,
    MSG_WRAP_MARKER       = 0x000F, ///< Sentinel indicating ring buffer wrap-around
    MSG_GENERATE_REQUEST  = 0x0010,
    MSG_TOKEN_CHUNK       = 0x0020,
    MSG_TOOL_CALL         = 0x0030,
    MSG_TOOL_SPAWN        = 0x0031,
    MSG_TOOL_COMPLETE     = 0x0032,
    MSG_CANCEL_STREAM     = 0x0040,
    MSG_TERMINATE_ENGINE  = 0x00FF
};

namespace FrameFlags {
    inline constexpr uint16_t NONE          = 0x0000;
    inline constexpr uint16_t END_OF_STREAM = 0x0001;
    inline constexpr uint16_t URGENT        = 0x0002;
    inline constexpr uint16_t WRAP_AROUND   = 0x0004;
}

// ============================================================================
// Binary Protocol Frame Layouts (Strict Memory Alignment)
// ============================================================================

#pragma pack(push, 8)

struct IpcFrameHeader {
    uint32_t magic;          ///< IPC_MAGIC (0x48594452)
    uint16_t msg_type;       ///< MessageType enum value
    uint16_t flags;          ///< FrameFlags bitmask
    uint32_t stream_id;      ///< Decoder / agent stream identifier
    uint32_t payload_size;   ///< Length in bytes of the following payload
    uint64_t sequence_num;   ///< Monotonically increasing sequence number
    uint64_t timestamp_ns;   ///< Monotonic nanosecond timestamp for latency measurement
};

/// @brief Compact inline token payload for zero-allocation token streaming
struct TokenPayload {
    uint32_t token_id;
    float logit;
    uint16_t text_len;
    uint8_t is_final;
    uint8_t reserved;
    char text[64];           ///< Inlined string representation (null-terminated)
};

/// @brief Sampling and generation parameters submitted with MSG_GENERATE_REQUEST
struct GenerateParams {
    float temperature;
    float top_p;
    float min_p;
    uint32_t top_k;
    uint32_t max_tokens;
    uint32_t stop_token_count;
    uint32_t stop_tokens[16];
    uint8_t reasoning_effort; ///< 0=none, 1=low, 2=medium, 3=high
    uint8_t speculative_depth;
    uint8_t reserved[6];
};

/// @brief Tool call descriptor submitted with MSG_TOOL_CALL
struct ToolCallDescriptor {
    uint32_t call_id;
    uint16_t tool_name_len;
    uint16_t args_json_len;
    char tool_name[64];
    // Variable length arguments JSON follows directly in payload
};

#pragma pack(pop)

// ============================================================================
// Lock-Free SPSC Ring Buffer Queue Control Block
// ============================================================================

#if defined(_MSC_VER)
#pragma warning(push)
#pragma warning(disable: 4324) // structure was padded due to alignment specifier
#endif

struct alignas(64) ShmQueueHeader {
    uint64_t capacity_bytes;
    uint64_t mask;
    alignas(64) std::atomic<uint64_t> head; ///< Write cursor (advanced by producer)
    alignas(64) std::atomic<uint64_t> tail; ///< Read cursor (advanced by consumer)
    uint64_t total_bytes_written;
    uint64_t total_bytes_read;
    uint64_t total_frames_written;
    uint64_t total_frames_read;
};

/// @brief Global Shared Memory Header positioned at byte 0 of the mapped file
struct alignas(64) ShmGlobalHeader {
    uint32_t magic;
    uint32_t version;
    uint64_t total_size_bytes;

    uint32_t daemon_pid;
    uint32_t engine_pid;

    /// @brief Immediate Process Spawn Hook: atomic counter incremented when tool processes spawn
    alignas(64) std::atomic<uint32_t> active_tool_processes;

    /// @brief Engine scheduling mode: 0=default spin/poll, 1=cudaDeviceScheduleYield
    alignas(64) std::atomic<uint32_t> cuda_yield_mode;

    uint64_t request_queue_offset;
    uint64_t response_queue_offset;

    ShmQueueHeader request_queue;
    ShmQueueHeader response_queue;
};

#if defined(_MSC_VER)
#pragma warning(pop)
#endif

} // namespace hydrogen::ipc
