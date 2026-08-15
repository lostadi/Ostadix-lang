cmake_minimum_required(VERSION 3.16)

project(o-lang LANGUAGES C)

set(CMAKE_C_STANDARD 17)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
option(OLANG_WARNINGS_AS_ERRORS "Treat C17 compiler warnings as errors" OFF)

find_package(Threads REQUIRED)
find_package(Python3 REQUIRED COMPONENTS Interpreter)

set(OLANG_CORE_SRCS
    src/value.c
    src/parser.c
    src/process.c
    src/eval.c
    src/scheduler.c
    src/nix_ops.c
    src/nixos_ops.c
)

function(olang_configure_target target)
    target_include_directories(${target} PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/include)
    target_compile_definitions(${target} PRIVATE
        _POSIX_C_SOURCE=200809L
        _XOPEN_SOURCE=700
    )
    target_link_libraries(${target} PRIVATE Threads::Threads)

    if(CMAKE_C_COMPILER_ID MATCHES "GNU|Clang")
        target_compile_options(${target} PRIVATE -Wall -Wextra -Wpedantic -O2)
        if(OLANG_WARNINGS_AS_ERRORS)
            target_compile_options(${target} PRIVATE -Werror)
        endif()
    endif()
endfunction()

add_executable(O
    ${OLANG_CORE_SRCS}
    src/main.c
)
olang_configure_target(O)

add_executable(olangc
    ${OLANG_CORE_SRCS}
    src/olangc.c
)
olang_configure_target(olangc)

add_executable(fresh_env_test
    ${OLANG_CORE_SRCS}
    tests/test_fresh_env.c
)
olang_configure_target(fresh_env_test)

# olangc locates runtime sources relative to its executable; provide them in
# out-of-source CMake build directories without making the legacy C++ path active.
add_custom_command(TARGET olangc POST_BUILD
    COMMAND ${CMAKE_COMMAND} -E create_symlink
            ${CMAKE_CURRENT_SOURCE_DIR}/src
            $<TARGET_FILE_DIR:olangc>/src
    COMMAND ${CMAKE_COMMAND} -E create_symlink
            ${CMAKE_CURRENT_SOURCE_DIR}/include
            $<TARGET_FILE_DIR:olangc>/include
)

enable_testing()

set(OLANG_BACKENDS_DIR ${CMAKE_CURRENT_SOURCE_DIR}/../backends)
set(OLANG_EXAMPLE_RUNNER ${CMAKE_CURRENT_SOURCE_DIR}/../tests/example_manifest.py)

add_test(NAME interpreter_manifest
    COMMAND ${Python3_EXECUTABLE} ${OLANG_EXAMPLE_RUNNER}
            run
            --edition c17
            --runner $<TARGET_FILE:O>
            --backends ${OLANG_BACKENDS_DIR}
)

add_test(NAME aot_manifest
    COMMAND ${Python3_EXECUTABLE} ${OLANG_EXAMPLE_RUNNER}
            run-c17-aot
            --compiler $<TARGET_FILE:olangc>
            --backends ${OLANG_BACKENDS_DIR}
)

add_test(NAME aot_path_safety
    COMMAND sh ${CMAKE_CURRENT_SOURCE_DIR}/tests/test_olangc_paths.sh
            $<TARGET_FILE:olangc>
            ${CMAKE_C_COMPILER}
            $<TARGET_FILE:O>
)

add_test(NAME fresh_environment_semantics
    COMMAND $<TARGET_FILE:fresh_env_test> ${OLANG_BACKENDS_DIR}
)

if(OLANG_WARNINGS_AS_ERRORS)
    set_tests_properties(aot_manifest aot_path_safety PROPERTIES
        ENVIRONMENT "OLANGC_WARNINGS_AS_ERRORS=1"
    )
endif()
