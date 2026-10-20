cmake_minimum_required(VERSION 3.28)
project(hydrogen LANGUAGES CXX CUDA)

# Enforce ISO C++20 for host and device compilation
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

set(CMAKE_CUDA_STANDARD 20)
set(CMAKE_CUDA_STANDARD_REQUIRED ON)
set(CMAKE_CUDA_EXTENSIONS OFF)

# Target NVIDIA Ada Lovelace SM 8.9 (RTX 4070)
set(CMAKE_CUDA_ARCHITECTURES 89)

# Common runtime output directories
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)
set(CMAKE_LIBRARY_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib)
set(CMAKE_ARCHIVE_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib)

# Compiler flags
if(MSVC)
    add_compile_options(
        $<$<COMPILE_LANGUAGE:CXX>:/std:c++20>
        $<$<COMPILE_LANGUAGE:CXX>:/permissive->
        $<$<COMPILE_LANGUAGE:CXX>:/utf-8>
        $<$<COMPILE_LANGUAGE:CXX>:/W4>
        $<$<COMPILE_LANGUAGE:CXX>:/O2>
        $<$<COMPILE_LANGUAGE:CXX>:/Oi>
        $<$<COMPILE_LANGUAGE:CXX>:/Gy>
        $<$<COMPILE_LANGUAGE:CXX>:/DNOMINMAX>
        $<$<COMPILE_LANGUAGE:CXX>:/DWIN32_LEAN_AND_MEAN>
        $<$<COMPILE_LANGUAGE:CXX>:/D_CRT_SECURE_NO_WARNINGS>
    )
endif()

if(CMAKE_CUDA_COMPILER_ID STREQUAL "NVIDIA")
    add_compile_options(
        $<$<COMPILE_LANGUAGE:CUDA>:-O3>
        $<$<COMPILE_LANGUAGE:CUDA>:--use_fast_math>
        $<$<COMPILE_LANGUAGE:CUDA>:--expt-relaxed-constexpr>
        $<$<COMPILE_LANGUAGE:CUDA>:-Xcompiler=/utf-8,/permissive-,/W4>
    )
endif()

# Sanity Check Target (Issue H01)
add_executable(hydrogen_sanity_test src/sanity_check.cu)

target_include_directories(hydrogen_sanity_test PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/include
    ${CMAKE_CURRENT_SOURCE_DIR}/src
)

set_target_properties(hydrogen_sanity_test PROPERTIES
    CUDA_SEPARABLE_COMPILATION OFF
)

# Model Subsystem Library (Issue H02)
add_library(hydrogen_model STATIC
    src/model/tensor_loader.cpp
)

target_include_directories(hydrogen_model PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}/include
    ${CMAKE_CURRENT_SOURCE_DIR}/src
)

# Model Inspection Utility (Issue H02)
add_executable(hydrogen_inspect_model
    src/tools/inspect_model.cpp
)

target_link_libraries(hydrogen_inspect_model PRIVATE
    hydrogen_model
)

# Forward Baseline Engine Library (Issues H03, H07, H08)
add_library(hydrogen_engine STATIC
    src/model/forward_baseline.cu
    src/attention/cascade_attention.cu
    src/model/marlin_gemm.cu
)

target_include_directories(hydrogen_engine PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}/include
    ${CMAKE_CURRENT_SOURCE_DIR}/src
)

target_link_libraries(hydrogen_engine PUBLIC
    hydrogen_model
)

set_target_properties(hydrogen_engine PROPERTIES
    CUDA_SEPARABLE_COMPILATION OFF
)

# Forward Baseline Smoke Test Target (Issue H03)
add_executable(hydrogen_smoke_test
    src/smoke_test.cpp
)

target_include_directories(hydrogen_smoke_test PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/include
    ${CMAKE_CURRENT_SOURCE_DIR}/src
)

target_link_libraries(hydrogen_smoke_test PRIVATE
    hydrogen_engine
    hydrogen_model
)

# Memory Subsystem Library (Issues H04, H05, H06)
add_library(hydrogen_memory STATIC
    src/memory/page_allocator.cpp
    src/memory/radix_tree.cpp
    src/memory/page_pool.cpp
)

target_include_directories(hydrogen_memory PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}/include
    ${CMAKE_CURRENT_SOURCE_DIR}/src
)

# Page Block Test Target (Issue H04)
add_executable(hydrogen_test_page_block
    src/tools/test_page_block.cpp
)

target_include_directories(hydrogen_test_page_block PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/include
    ${CMAKE_CURRENT_SOURCE_DIR}/src
)

target_link_libraries(hydrogen_test_page_block PRIVATE
    hydrogen_memory
)

# Radix Tree Test Target (Issue H05)
add_executable(hydrogen_test_radix_tree
    src/tools/test_radix_tree.cpp
)

target_include_directories(hydrogen_test_radix_tree PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/include
    ${CMAKE_CURRENT_SOURCE_DIR}/src
)

target_link_libraries(hydrogen_test_radix_tree PRIVATE
    hydrogen_memory
)

# Lock-Free Page Pool Stress Test Target (Issue H06)
add_executable(hydrogen_stress_test_page_pool
    src/tools/stress_test_page_pool.cpp
)

target_include_directories(hydrogen_stress_test_page_pool PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/include
    ${CMAKE_CURRENT_SOURCE_DIR}/src
)

target_link_libraries(hydrogen_stress_test_page_pool PRIVATE
    hydrogen_memory
)

# FlashInfer Cascade Attention Benchmark Target (Issue H07)
add_executable(hydrogen_bench_cascade_attention
    src/tools/bench_cascade_attention.cpp
)

target_include_directories(hydrogen_bench_cascade_attention PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/include
    ${CMAKE_CURRENT_SOURCE_DIR}/src
)

target_link_libraries(hydrogen_bench_cascade_attention PRIVATE
    hydrogen_engine
    hydrogen_memory
)

# Marlin / CUTLASS Quantized GEMM Benchmark Target (Issue H08)
add_executable(hydrogen_bench_marlin_gemm
    src/tools/bench_marlin_gemm.cpp
)

target_include_directories(hydrogen_bench_marlin_gemm PRIVATE
    ${CMAKE_CURRENT_SOURCE_DIR}/include
    ${CMAKE_CURRENT_SOURCE_DIR}/src
)

target_link_libraries(hydrogen_bench_marlin_gemm PRIVATE
    hydrogen_engine
    hydrogen_memory
    hydrogen_model
)
