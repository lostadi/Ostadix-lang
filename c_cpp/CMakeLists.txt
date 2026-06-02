cmake_minimum_required(VERSION 3.16)

project(o-lang LANGUAGES C CXX)

set(CMAKE_C_STANDARD 17)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

function(olang_configure_target target)
    target_include_directories(${target}
        PUBLIC
            ${CMAKE_CURRENT_SOURCE_DIR}/include
            ${CMAKE_CURRENT_SOURCE_DIR}/third_party
    )

    if(CMAKE_C_COMPILER_ID MATCHES "GNU|Clang" OR CMAKE_CXX_COMPILER_ID MATCHES "GNU|Clang")
        target_compile_options(${target} PRIVATE -Wall -Wextra -Wpedantic)
    endif()
endfunction()

add_library(olang_core STATIC
    src/value.c
    src/parser.c
)
olang_configure_target(olang_core)

add_library(olang_cpp STATIC
    src/process.cpp
    src/eval.cpp
    src/scheduler.cpp
    src/nix_ops.cpp
    src/nixos_ops.cpp
)
target_link_libraries(olang_cpp PUBLIC olang_core)
olang_configure_target(olang_cpp)

add_executable(O src/main.cpp)
target_link_libraries(O PRIVATE olang_cpp)
olang_configure_target(O)

add_executable(olangc src/olangc.cpp)
target_link_libraries(olangc PRIVATE olang_cpp)
olang_configure_target(olangc)

if(UNIX)
    target_link_libraries(olang_cpp PUBLIC pthread)
endif()
