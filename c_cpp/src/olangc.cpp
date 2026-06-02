#include <iostream>

int main(int argc, char *argv[]) {
    (void)argc; (void)argv;
    std::cerr << "olangc: C/C++ compiler not yet implemented.\n"
              << "The O-lang compiler requires embedding runtime sources and shim scripts.\n"
              << "Use the Rust version (cargo run --bin olangc) for now.\n";
    return 1;
}
