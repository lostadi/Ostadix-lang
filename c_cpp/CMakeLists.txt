cmake_minimum_required(VERSION 3.16)

project(o-lang LANGUAGES C)

set(CMAKE_C_STANDARD 17)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

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

set(OLANG_EXAMPLES_DIR ${CMAKE_CURRENT_SOURCE_DIR}/../examples)
set(OLANG_BACKENDS_DIR ${CMAKE_CURRENT_SOURCE_DIR}/../backends)

add_test(NAME interpreter_hello
    COMMAND $<TARGET_FILE:O> ${OLANG_EXAMPLES_DIR}/hello.O ${OLANG_BACKENDS_DIR}
)
set_tests_properties(interpreter_hello PROPERTIES PASS_REGULAR_EXPRESSION "2")

add_test(NAME interpreter_meta_eval
    COMMAND $<TARGET_FILE:O> ${OLANG_EXAMPLES_DIR}/meta_eval.O ${OLANG_BACKENDS_DIR}
)
set_tests_properties(interpreter_meta_eval PROPERTIES PASS_REGULAR_EXPRESSION "1024|42")

file(WRITE ${CMAKE_CURRENT_BINARY_DIR}/run_aot_test.py ""
"import re, subprocess, sys\n"
"compiler, source, backends, output, pattern = sys.argv[1:]\n"
"subprocess.run([compiler, source, '-o', output, '--shim-dir', backends], check=True)\n"
"result = subprocess.run([output], check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)\n"
"print(result.stdout, end='')\n"
"if not re.search(pattern, result.stdout):\n"
"    raise SystemExit(f'expected /{pattern}/ in output')\n"
)

add_test(NAME aot_hello
    COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_BINARY_DIR}/run_aot_test.py
            $<TARGET_FILE:olangc>
            ${OLANG_EXAMPLES_DIR}/hello.O
            ${OLANG_BACKENDS_DIR}
            ${CMAKE_CURRENT_BINARY_DIR}/hello_c
            2
)

add_test(NAME aot_meta_eval
    COMMAND ${Python3_EXECUTABLE} ${CMAKE_CURRENT_BINARY_DIR}/run_aot_test.py
            $<TARGET_FILE:olangc>
            ${OLANG_EXAMPLES_DIR}/meta_eval.O
            ${OLANG_BACKENDS_DIR}
            ${CMAKE_CURRENT_BINARY_DIR}/meta_c
            "1024|42"
)
