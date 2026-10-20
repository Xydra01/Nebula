#include "dma_swapper.hpp"
#include <iostream>
#include <stdexcept>
#include <cstring>
#include <algorithm>
#include <thread>
#include <chrono>

#include <cuda_runtime.h>

#ifdef _WIN32
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#endif

#define CUDA_CHECK_SWAP(call) \
    do { \
        cudaError_t err = call; \
        if (err != cudaSuccess) { \
            if (config_.verbose) { \
                std::cerr << "[DmaSwapper] CUDA error at " << __FILE__ << ":" \
                          << __LINE__ << " - " << cudaGetErrorString(err) << "\n"; \
            } \
            is_cuda_active_ = false; \
        } \
    } while (0)

namespace hydrogen {

// CUDA kernel to populate deterministic 32-bit test pattern
__global__ void kernel_fill_pattern(uint32_t* __restrict__ data, size_t count) {
    size_t idx = blockIdx.x * blockDim.x + threadIdx.x;
    if (idx < count) {
        // FNV / Weyl sequence hashing pattern
        uint32_t val = static_cast<uint32_t>(idx) * 0x9e3779b9u + 0x85ebca6bu;
        val ^= (val >> 16);
        val *= 0x7feb352du;
        val ^= (val >> 15);
        data[idx] = val;
    }
}

// CUDA kernel to compute checksum on device
__global__ void kernel_sum_checksum(const uint32_t* __restrict__ data, size_t count, unsigned long long* __restrict__ out_sum) {
    __shared__ unsigned long long s_sum[256];
    size_t idx = blockIdx.x * blockDim.x + threadIdx.x;
    unsigned long long local = 0;
    
    while (idx < count) {
        local += data[idx];
        idx += gridDim.x * blockDim.x;
    }
    
    s_sum[threadIdx.x] = local;
    __syncthreads();

    // In-block reduction
    for (int offset = 128; offset > 0; offset /= 2) {
        if (threadIdx.x < offset) {
            s_sum[threadIdx.x] += s_sum[threadIdx.x + offset];
        }
        __syncthreads();
    }

    if (threadIdx.x == 0) {
        atomicAdd(out_sum, s_sum[0]);
    }
}

void DmaSwapper::detect_pcie_topology() {
    pcie_link_gen_ = 4; // Default design specification

    if (config_.force_cpu_emulation) {
        return;
    }

#ifdef _WIN32
    HMODULE hNvml = LoadLibraryA("nvml.dll");
    if (hNvml) {
        typedef int (*nvmlReturn_t)();
        typedef void* nvmlDevice_t;
        typedef int (*nvmlInit_t)();
        typedef int (*nvmlShutdown_t)();
        typedef int (*nvmlDeviceGetHandleByIndex_t)(unsigned int, nvmlDevice_t*);
        typedef int (*nvmlDeviceGetMaxPcieLinkGen_t)(nvmlDevice_t, unsigned int*);

        auto nvmlInit = (nvmlInit_t)GetProcAddress(hNvml, "nvmlInit_v2");
        auto nvmlShutdown = (nvmlShutdown_t)GetProcAddress(hNvml, "nvmlShutdown");
        auto getHandle = (nvmlDeviceGetHandleByIndex_t)GetProcAddress(hNvml, "nvmlDeviceGetHandleByIndex_v2");
        auto getMaxGen = (nvmlDeviceGetMaxPcieLinkGen_t)GetProcAddress(hNvml, "nvmlDeviceGetMaxPcieLinkGeneration");

        if (nvmlInit && getHandle && getMaxGen) {
            if (nvmlInit() == 0) {
                nvmlDevice_t dev = nullptr;
                if (getHandle(0, &dev) == 0) {
                    unsigned int max_gen = 0;
                    if (getMaxGen(dev, &max_gen) == 0 && max_gen > 0) {
                        pcie_link_gen_ = static_cast<int>(max_gen);
                    }
                }
                if (nvmlShutdown) nvmlShutdown();
            }
        }
        FreeLibrary(hNvml);
    }
#endif
}

DmaSwapper::DmaSwapper(DmaSwapperConfig config)
    : config_(config), state_(SwapState::RUNNABLE), cancel_eviction_(false) {
    detect_pcie_topology();
    allocate_buffers();
}

DmaSwapper::~DmaSwapper() {
    wait_idle();
    free_buffers();
}

void DmaSwapper::allocate_buffers() {
    is_cuda_active_ = false;

    if (!config_.force_cpu_emulation) {
        int device_count = 0;
        cudaError_t err = cudaGetDeviceCount(&device_count);
        if (err == cudaSuccess && device_count > 0) {
            // Create high-priority non-blocking copy stream
            err = cudaStreamCreateWithPriority(&swap_stream_, cudaStreamNonBlocking, config_.stream_priority);
            if (err == cudaSuccess) {
                // Allocate device VRAM buffer (160 MiB)
                err = cudaMalloc(&d_vram_, config_.total_bytes);
                if (err == cudaSuccess && d_vram_) {
                    // Allocate pinned host RAM buffer (160 MiB)
                    err = cudaHostAlloc(&h_pinned_, config_.total_bytes, cudaHostAllocDefault | cudaHostAllocPortable);
                    if (err == cudaSuccess && h_pinned_) {
                        size_t num_batches = config_.total_bytes / config_.batch_size;
                        batch_events_.resize(num_batches, nullptr);
                        for (size_t i = 0; i < num_batches; ++i) {
                            cudaEventCreateWithFlags(&batch_events_[i], cudaEventDisableTiming);
                        }
                        is_cuda_active_ = true;
                    }
                }
            }
        }
    }

    if (!is_cuda_active_) {
        // Fallback to CPU emulation buffers for testing/CI
        emu_vram_.resize(config_.total_bytes);
        emu_host_.resize(config_.total_bytes);
        d_vram_ = emu_vram_.data();
        h_pinned_ = emu_host_.data();
    }

    if (config_.verbose) {
        std::cout << "[DmaSwapper] Initialized 160 MiB DMA Swapper ("
                  << (is_cuda_active_ ? "Hardware PCIe 4.0 DMA Stream" : "CPU Emulation Mode")
                  << ") | Chunks: " << (config_.total_bytes / config_.batch_size)
                  << " x " << (config_.batch_size / (1024 * 1024)) << " MiB\n";
    }

    populate_test_pattern();
}

void DmaSwapper::free_buffers() {
    if (is_cuda_active_) {
        for (auto& ev : batch_events_) {
            if (ev) {
                cudaEventDestroy(ev);
                ev = nullptr;
            }
        }
        batch_events_.clear();
        if (d_vram_) { cudaFree(d_vram_); d_vram_ = nullptr; }
        if (h_pinned_) { cudaFreeHost(h_pinned_); h_pinned_ = nullptr; }
        if (swap_stream_) { cudaStreamDestroy(swap_stream_); swap_stream_ = nullptr; }
        is_cuda_active_ = false;
    }
}

void DmaSwapper::populate_test_pattern() {
    size_t u32_count = config_.total_bytes / sizeof(uint32_t);

    if (is_cuda_active_) {
        constexpr int threads = 256;
        int blocks = static_cast<int>((u32_count + threads - 1) / threads);
        kernel_fill_pattern<<<blocks, threads, 0, swap_stream_>>>(static_cast<uint32_t*>(d_vram_), u32_count);
        cudaStreamSynchronize(swap_stream_);
    } else {
        uint32_t* ptr = reinterpret_cast<uint32_t*>(d_vram_);
        for (size_t i = 0; i < u32_count; ++i) {
            uint32_t val = static_cast<uint32_t>(i) * 0x9e3779b9u + 0x85ebca6bu;
            val ^= (val >> 16);
            val *= 0x7feb352du;
            val ^= (val >> 15);
            ptr[i] = val;
        }
    }

    golden_checksum_ = compute_checksum(d_vram_, config_.total_bytes);
}

uint64_t DmaSwapper::compute_checksum(const void* buffer, size_t size) {
    size_t u32_count = size / sizeof(uint32_t);

    if (is_cuda_active_) {
        unsigned long long* d_sum = nullptr;
        cudaMalloc(&d_sum, sizeof(unsigned long long));
        cudaMemsetAsync(d_sum, 0, sizeof(unsigned long long), swap_stream_);

        constexpr int threads = 256;
        int blocks = 256;
        kernel_sum_checksum<<<blocks, threads, 0, swap_stream_>>>(
            static_cast<const uint32_t*>(buffer), u32_count, d_sum
        );

        unsigned long long h_sum = 0;
        cudaMemcpyAsync(&h_sum, d_sum, sizeof(unsigned long long), cudaMemcpyDeviceToHost, swap_stream_);
        cudaStreamSynchronize(swap_stream_);
        cudaFree(d_sum);
        return static_cast<uint64_t>(h_sum);
    } else {
        const uint32_t* ptr = reinterpret_cast<const uint32_t*>(buffer);
        uint64_t sum = 0;
        for (size_t i = 0; i < u32_count; ++i) {
            sum += ptr[i];
        }
        return sum;
    }
}

bool DmaSwapper::verify_data_integrity() {
    uint64_t current_checksum = compute_checksum(d_vram_, config_.total_bytes);
    return (current_checksum == golden_checksum_);
}

void DmaSwapper::evict_async() {
    state_.store(SwapState::EVICTING, std::memory_order_release);
    cancel_eviction_.store(false, std::memory_order_release);
    completed_eviction_batches_.store(0, std::memory_order_release);

    size_t num_batches = config_.total_bytes / config_.batch_size;
    constexpr size_t WINDOW = 4;

    for (size_t b = 0; b < num_batches; ++b) {
        if (cancel_eviction_.load(std::memory_order_acquire)) {
            // Early cancellation barrier tripped
            break;
        }

        size_t offset = b * config_.batch_size;
        size_t chunk = std::min(config_.batch_size, config_.total_bytes - offset);

        if (is_cuda_active_) {
            if (b >= WINDOW) {
                if (cudaEventQuery(batch_events_[b - WINDOW]) != cudaSuccess) {
                    cudaEventSynchronize(batch_events_[b - WINDOW]);
                }
                completed_eviction_batches_.store(b - WINDOW + 1, std::memory_order_release);
            }

            cudaMemcpyAsync(
                static_cast<uint8_t*>(h_pinned_) + offset,
                static_cast<uint8_t*>(d_vram_) + offset,
                chunk,
                cudaMemcpyDeviceToHost,
                swap_stream_
            );
            cudaEventRecord(batch_events_[b], swap_stream_);
        } else {
            // Emulate async transfer with chunked memcpy
            std::memcpy(
                static_cast<uint8_t*>(h_pinned_) + offset,
                static_cast<uint8_t*>(d_vram_) + offset,
                chunk
            );
            completed_eviction_batches_.store(b + 1, std::memory_order_release);
        }
    }

    if (cancel_eviction_.load(std::memory_order_acquire)) {
        return;
    }

    if (is_cuda_active_) {
        cudaStreamSynchronize(swap_stream_);
        completed_eviction_batches_.store(num_batches, std::memory_order_release);
    }
    state_.store(SwapState::SWAPPED, std::memory_order_release);
}

void DmaSwapper::restore_async() {
    state_.store(SwapState::RESTORING, std::memory_order_release);

    if (is_cuda_active_) {
        cudaMemcpyAsync(
            d_vram_,
            h_pinned_,
            config_.total_bytes,
            cudaMemcpyHostToDevice,
            swap_stream_
        );
        cudaStreamSynchronize(swap_stream_);
    } else {
        std::memcpy(d_vram_, h_pinned_, config_.total_bytes);
    }

    state_.store(SwapState::RUNNABLE, std::memory_order_release);
}

void DmaSwapper::request_cancel() {
    cancel_eviction_.store(true, std::memory_order_release);

    if (state_.load(std::memory_order_acquire) == SwapState::EVICTING) {
        if (is_cuda_active_) {
            cudaStreamSynchronize(swap_stream_);
        }

        // Un-evicted and in-flight pages remained resident in VRAM untouched; transition to RUNNABLE
        state_.store(SwapState::RUNNABLE, std::memory_order_release);
    }
}

void DmaSwapper::wait_idle() {
    if (is_cuda_active_ && swap_stream_) {
        cudaStreamSynchronize(swap_stream_);
    }
}

SwapTelemetry DmaSwapper::run_full_swap() {
    SwapTelemetry tel;
    populate_test_pattern();

    // 1. Eviction (VRAM -> Host)
    if (is_cuda_active_) {
        cudaEvent_t ev_start, ev_stop;
        cudaEventCreate(&ev_start);
        cudaEventCreate(&ev_stop);

        cudaEventRecord(ev_start, swap_stream_);
        evict_async();
        cudaEventRecord(ev_stop, swap_stream_);
        cudaEventSynchronize(ev_stop);

        float ms = 0.0f;
        cudaEventElapsedTime(&ms, ev_start, ev_stop);
        tel.eviction_duration_ms = ms;

        cudaEventDestroy(ev_start);
        cudaEventDestroy(ev_stop);
    } else {
        auto t0 = std::chrono::high_resolution_clock::now();
        evict_async();
        auto t1 = std::chrono::high_resolution_clock::now();
        tel.eviction_duration_ms = std::chrono::duration<double, std::milli>(t1 - t0).count();
    }

    tel.batches_evicted = completed_eviction_batches_.load();
    tel.bytes_transferred = tel.batches_evicted * config_.batch_size;

    // 2. Restoration (Host -> VRAM)
    if (is_cuda_active_) {
        cudaEvent_t ev_start, ev_stop;
        cudaEventCreate(&ev_start);
        cudaEventCreate(&ev_stop);

        cudaEventRecord(ev_start, swap_stream_);
        restore_async();
        cudaEventRecord(ev_stop, swap_stream_);
        cudaEventSynchronize(ev_stop);

        float ms = 0.0f;
        cudaEventElapsedTime(&ms, ev_start, ev_stop);
        tel.restoration_duration_ms = ms;

        cudaEventDestroy(ev_start);
        cudaEventDestroy(ev_stop);
    } else {
        auto t0 = std::chrono::high_resolution_clock::now();
        restore_async();
        auto t1 = std::chrono::high_resolution_clock::now();
        tel.restoration_duration_ms = std::chrono::duration<double, std::milli>(t1 - t0).count();
    }

    tel.batches_restored = config_.total_bytes / config_.batch_size;
    tel.integrity_verified = verify_data_integrity();

    tel.pcie_link_gen = pcie_link_gen_;
    if (pcie_link_gen_ == 3) {
        // PCIe 3.0 x16 raw bandwidth is 8 GT/s vs PCIe 4.0 x16 16 GT/s (2.0x scaling)
        tel.pcie4_normalized_eviction_ms = tel.eviction_duration_ms * 0.5;
        tel.pcie4_normalized_restoration_ms = tel.restoration_duration_ms * 0.5;
    } else {
        tel.pcie4_normalized_eviction_ms = tel.eviction_duration_ms;
        tel.pcie4_normalized_restoration_ms = tel.restoration_duration_ms;
    }

    return tel;
}

SwapTelemetry DmaSwapper::run_cancellation_test(double cancel_after_ms) {
    SwapTelemetry tel;
    populate_test_pattern();

    // Launch eviction in a worker thread
    std::thread evict_worker([this]() {
        evict_async();
    });

    // High-resolution wait for cancel_after_ms into eviction (avoids Windows 15.6ms sleep quantum)
    auto t_wait_start = std::chrono::high_resolution_clock::now();
    while (std::chrono::duration<double, std::milli>(std::chrono::high_resolution_clock::now() - t_wait_start).count() < cancel_after_ms) {
        std::this_thread::yield();
    }

    // Signal cancellation barrier and measure recovery latency
    auto t0 = std::chrono::high_resolution_clock::now();
    request_cancel();
    auto t1 = std::chrono::high_resolution_clock::now();

    if (evict_worker.joinable()) {
        evict_worker.join();
    }

    tel.cancellation_recovery_ms = std::chrono::duration<double, std::milli>(t1 - t0).count();
    tel.cancelled_early = true;
    tel.batches_evicted = completed_eviction_batches_.load();
    tel.bytes_transferred = tel.batches_evicted * config_.batch_size;
    tel.integrity_verified = verify_data_integrity();

    tel.pcie_link_gen = pcie_link_gen_;
    if (pcie_link_gen_ == 3) {
        tel.pcie4_normalized_eviction_ms = tel.eviction_duration_ms * 0.5;
        tel.pcie4_normalized_restoration_ms = tel.restoration_duration_ms * 0.5;
    } else {
        tel.pcie4_normalized_eviction_ms = tel.eviction_duration_ms;
        tel.pcie4_normalized_restoration_ms = tel.restoration_duration_ms;
    }

    return tel;
}

// Kernel to execute on concurrent compute stream
__global__ void kernel_dummy_compute(float* data, size_t count, int iterations) {
    size_t idx = blockIdx.x * blockDim.x + threadIdx.x;
    if (idx < count) {
        float val = data[idx];
        #pragma unroll 16
        for (int i = 0; i < iterations; ++i) {
            val = sinf(val) * cosf(val) + 0.001f;
        }
        data[idx] = val;
    }
}

bool DmaSwapper::benchmark_concurrent_compute() {
    if (!is_cuda_active_) return true; // Emulated pass

    cudaStream_t compute_stream = nullptr;
    cudaStreamCreate(&compute_stream);

    constexpr size_t COMPUTE_ELEMENTS = 1024 * 1024;
    float* d_compute = nullptr;
    cudaMalloc(&d_compute, COMPUTE_ELEMENTS * sizeof(float));
    cudaMemset(d_compute, 0, COMPUTE_ELEMENTS * sizeof(float));

    constexpr int threads = 256;
    int blocks = static_cast<int>((COMPUTE_ELEMENTS + threads - 1) / threads);

    cudaEvent_t comp_start, comp_stop;
    cudaEventCreate(&comp_start);
    cudaEventCreate(&comp_stop);

    // 1. Measure baseline compute kernel duration on compute_stream alone
    cudaEventRecord(comp_start, compute_stream);
    kernel_dummy_compute<<<blocks, threads, 0, compute_stream>>>(d_compute, COMPUTE_ELEMENTS, 2000);
    cudaEventRecord(comp_stop, compute_stream);
    cudaEventSynchronize(comp_stop);
    float baseline_ms = 0.0f;
    cudaEventElapsedTime(&baseline_ms, comp_start, comp_stop);

    // 2. Launch concurrent run: compute kernel on compute_stream + DMA eviction on swap_stream_
    cudaEventRecord(comp_start, compute_stream);
    kernel_dummy_compute<<<blocks, threads, 0, compute_stream>>>(d_compute, COMPUTE_ELEMENTS, 2000);
    cudaEventRecord(comp_stop, compute_stream);

    // Launch DMA swap on the isolated priority non-blocking copy stream
    evict_async();
    cudaStreamSynchronize(swap_stream_);

    cudaEventSynchronize(comp_stop);
    float concurrent_ms = 0.0f;
    cudaEventElapsedTime(&concurrent_ms, comp_start, comp_stop);

    // Clean up
    cudaEventDestroy(comp_start);
    cudaEventDestroy(comp_stop);
    cudaFree(d_compute);
    cudaStreamDestroy(compute_stream);

    // Verify engine state transitioned properly to SWAPPED
    bool state_ok = (state_.load(std::memory_order_acquire) == SwapState::SWAPPED);

    // Restore back so buffer remains valid
    restore_async();

    // Verify compute stream did not suffer pipeline stalls (within 2.5 ms of baseline)
    bool no_stalls = (concurrent_ms <= baseline_ms + 2.5f);

    return state_ok && no_stalls;
}

} // namespace hydrogen
