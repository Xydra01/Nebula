#include <cuda_runtime.h>
#include <cmath>
#include <cstdint>
#include <iomanip>
#include <iostream>
#include <string>

// CUDA Error checking macro
#define CUDA_CHECK(call)                                                       \
    do {                                                                       \
        cudaError_t status = (call);                                           \
        if (status != cudaSuccess) {                                           \
            std::cerr << "CUDA Error at " << __FILE__ << ":" << __LINE__       \
                      << " - " << cudaGetErrorString(status) << std::endl;     \
            return 1;                                                          \
        }                                                                      \
    } while (0)

// Lightweight asynchronous vector-add kernel targeting Ada Lovelace SM 8.9
__global__ void vector_add_kernel(const float* __restrict__ a,
                                  const float* __restrict__ b,
                                  float* __restrict__ c,
                                  size_t n) {
    size_t idx = blockDim.x * blockIdx.x + threadIdx.x;
    if (idx < n) {
        c[idx] = a[idx] + b[idx];
    }
}

int main() {
    int device_count = 0;
    cudaError_t err = cudaGetDeviceCount(&device_count);
    if (err != cudaSuccess || device_count == 0) {
        std::cerr << "{\"status\":\"FAIL\",\"error\":\"No CUDA-capable devices found\"}"
                  << std::endl;
        return 1;
    }

    cudaDeviceProp prop;
    CUDA_CHECK(cudaGetDeviceProperties(&prop, 0));

    // Force CUDA primary context initialization before sampling baseline VRAM
    CUDA_CHECK(cudaFree(0));

    // Initial VRAM baseline
    size_t mem_free_pre = 0;
    size_t mem_total = 0;
    CUDA_CHECK(cudaMemGetInfo(&mem_free_pre, &mem_total));

    // Ensure at least 32 MiB free VRAM is available
    const size_t min_headroom_bytes = 32 * 1024 * 1024;
    if (mem_free_pre < min_headroom_bytes) {
        std::cerr << "{\"status\":\"FAIL\",\"error\":\"Insufficient free VRAM for sanity check (<32MB)\"}"
                  << std::endl;
        return 1;
    }

    // Workload: 1,048,576 floats = 4 MB per vector (12 MB total across A, B, C)
    const size_t num_elements = 1048576;
    const size_t buffer_bytes = num_elements * sizeof(float);

    // Host pinned allocation
    float* h_a = nullptr;
    float* h_b = nullptr;
    float* h_c = nullptr;
    CUDA_CHECK(cudaMallocHost(reinterpret_cast<void**>(&h_a), buffer_bytes));
    CUDA_CHECK(cudaMallocHost(reinterpret_cast<void**>(&h_b), buffer_bytes));
    CUDA_CHECK(cudaMallocHost(reinterpret_cast<void**>(&h_c), buffer_bytes));

    // Initialize host buffers
    for (size_t i = 0; i < num_elements; ++i) {
        h_a[i] = std::sin(static_cast<float>(i));
        h_b[i] = std::cos(static_cast<float>(i));
        h_c[i] = 0.0f;
    }

    // Device allocation
    float* d_a = nullptr;
    float* d_b = nullptr;
    float* d_c = nullptr;
    CUDA_CHECK(cudaMalloc(reinterpret_cast<void**>(&d_a), buffer_bytes));
    CUDA_CHECK(cudaMalloc(reinterpret_cast<void**>(&d_b), buffer_bytes));
    CUDA_CHECK(cudaMalloc(reinterpret_cast<void**>(&d_c), buffer_bytes));

    // Stream and event initialization
    cudaStream_t stream = nullptr;
    CUDA_CHECK(cudaStreamCreateWithFlags(&stream, cudaStreamNonBlocking));

    cudaEvent_t start_event = nullptr;
    cudaEvent_t stop_event = nullptr;
    CUDA_CHECK(cudaEventCreate(&start_event));
    CUDA_CHECK(cudaEventCreate(&stop_event));

    // Copy to device asynchronously
    CUDA_CHECK(cudaMemcpyAsync(d_a, h_a, buffer_bytes, cudaMemcpyHostToDevice, stream));
    CUDA_CHECK(cudaMemcpyAsync(d_b, h_b, buffer_bytes, cudaMemcpyHostToDevice, stream));

    // Launch kernel: 4096 blocks of 256 threads = 1,048,576 threads
    const int threads_per_block = 256;
    const int blocks = static_cast<int>((num_elements + threads_per_block - 1) / threads_per_block);

    CUDA_CHECK(cudaEventRecord(start_event, stream));
    vector_add_kernel<<<blocks, threads_per_block, 0, stream>>>(d_a, d_b, d_c, num_elements);
    CUDA_CHECK(cudaEventRecord(stop_event, stream));

    // Copy back to host asynchronously
    CUDA_CHECK(cudaMemcpyAsync(h_c, d_c, buffer_bytes, cudaMemcpyDeviceToHost, stream));

    // Synchronize stream
    CUDA_CHECK(cudaStreamSynchronize(stream));

    float kernel_duration_ms = 0.0f;
    CUDA_CHECK(cudaEventElapsedTime(&kernel_duration_ms, start_event, stop_event));

    // CPU element verification
    size_t verified_elements = 0;
    bool verification_passed = true;
    for (size_t i = 0; i < num_elements; ++i) {
        float expected = h_a[i] + h_b[i];
        if (std::fabs(h_c[i] - expected) > 1e-5f) {
            verification_passed = false;
            break;
        }
        verified_elements++;
    }

    // Teardown and deallocation tracking
    size_t allocated_bytes = buffer_bytes * 3;
    size_t freed_bytes = 0;

    if (cudaFree(d_a) == cudaSuccess) freed_bytes += buffer_bytes;
    if (cudaFree(d_b) == cudaSuccess) freed_bytes += buffer_bytes;
    if (cudaFree(d_c) == cudaSuccess) freed_bytes += buffer_bytes;

    CUDA_CHECK(cudaFreeHost(h_a));
    CUDA_CHECK(cudaFreeHost(h_b));
    CUDA_CHECK(cudaFreeHost(h_c));

    CUDA_CHECK(cudaEventDestroy(start_event));
    CUDA_CHECK(cudaEventDestroy(stop_event));
    CUDA_CHECK(cudaStreamDestroy(stream));

    size_t vram_leak_bytes = allocated_bytes - freed_bytes;

    // Reset device to release any driver-internal buffers
    CUDA_CHECK(cudaDeviceReset());

    std::string compute_cap = std::to_string(prop.major) + "." + std::to_string(prop.minor);
    bool status_ok = verification_passed && (verified_elements == num_elements) && (vram_leak_bytes == 0);

    // Emit structured JSON telemetry
    std::cout << "{\n"
              << "  \"status\": \"" << (status_ok ? "PASS" : "FAIL") << "\",\n"
              << "  \"device_name\": \"" << prop.name << "\",\n"
              << "  \"compute_capability\": \"" << compute_cap << "\",\n"
              << "  \"multiprocessors\": " << prop.multiProcessorCount << ",\n"
              << "  \"total_vram_bytes\": " << mem_total << ",\n"
              << "  \"free_vram_bytes\": " << mem_free_pre << ",\n"
              << "  \"allocated_bytes\": " << allocated_bytes << ",\n"
              << "  \"freed_bytes\": " << freed_bytes << ",\n"
              << "  \"verified_elements\": " << verified_elements << ",\n"
              << "  \"kernel_duration_us\": " << std::fixed << std::setprecision(2)
              << (kernel_duration_ms * 1000.0f) << ",\n"
              << "  \"vram_leak_bytes\": " << vram_leak_bytes << "\n"
              << "}" << std::endl;

    return status_ok ? 0 : 1;
}
